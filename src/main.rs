use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use threadbeat::{
    VERSION,
    config::Config,
    control::{ControlRequest, OriginContext, call_daemon},
    service::{ServicePlan, ServicePlatform, detect_platform},
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "threadbeat", version = VERSION, about)]
struct Cli {
    /// Path to the operator-owned configuration file.
    #[arg(long, global = true, env = "THREADBEAT_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the resident scheduler and private control socket.
    Daemon,
    /// Serve the six Threadbeat MCP tools over stdio.
    Mcp(McpArgs),
    /// Call the private daemon control API from an interactive shell.
    Ctl(CtlArgs),
    /// Plan or apply service-manager files.
    Service(ServiceArgs),
    /// Inspect embedded and configured compatibility information.
    Schema(SchemaArgs),
}

#[derive(Debug, Args)]
struct McpArgs {
    /// Explicitly select the MCP stdio transport (the only v1 transport).
    #[arg(long, default_value_t = true)]
    stdio: bool,
}

#[derive(Debug, Args)]
struct CtlArgs {
    #[command(subcommand)]
    command: CtlCommand,
}

#[derive(Debug, Subcommand)]
enum CtlCommand {
    /// Invoke a task-scoped tool using explicit JSON arguments.
    Call {
        #[arg(long)]
        thread_id: String,
        #[arg(long)]
        tool: String,
        #[arg(long, default_value = "{}")]
        arguments: String,
    },
    /// Grant temporary CLI-confirmed activation authority to one task.
    Authorize {
        #[arg(long)]
        thread_id: String,
        #[arg(long, default_value_t = 15)]
        minutes: u64,
    },
    /// Run sanitized daemon/app-server/storage diagnostics.
    Doctor {
        #[arg(long)]
        thread_id: String,
        #[arg(long)]
        full: bool,
    },
}

#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Print a reviewable, content-addressed service installation plan.
    Plan {
        #[arg(long)]
        platform: Option<ServicePlatformArg>,
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Apply exactly a previously reviewed plan digest. Does not enable/start it.
    Apply {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        plan_digest: String,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ServicePlatformArg {
    SystemdUser,
    LaunchdAgent,
}

impl From<ServicePlatformArg> for ServicePlatform {
    fn from(value: ServicePlatformArg) -> Self {
        match value {
            ServicePlatformArg::SystemdUser => Self::SystemdUser,
            ServicePlatformArg::LaunchdAgent => Self::LaunchdAgent,
        }
    }
}

#[derive(Debug, Args)]
struct SchemaArgs {
    #[command(subcommand)]
    command: SchemaCommand,
}

#[derive(Debug, Subcommand)]
enum SchemaCommand {
    /// Run the configured app-server compatibility probe without delivery.
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();
    let config_path = cli.config.clone().unwrap_or_else(Config::default_path);
    let config = Config::load(Some(&config_path))?;

    match cli.command {
        Command::Daemon => threadbeat::daemon::run(config).await,
        Command::Mcp(args) => {
            if !args.stdio {
                bail!("v1 supports only the MCP stdio transport");
            }
            threadbeat::mcp::run(config).await
        }
        Command::Ctl(args) => run_ctl(config, args).await,
        Command::Service(args) => run_service(&config, config_path, args),
        Command::Schema(args) => match args.command {
            SchemaCommand::Check => {
                let report = threadbeat::app_server::probe_configured(&config).await?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                if report.strict_queue_ready {
                    Ok(())
                } else {
                    bail!("configured app-server did not pass the strict queue probe")
                }
            }
        },
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

async fn run_ctl(config: Config, args: CtlArgs) -> Result<()> {
    let (thread_id, tool, arguments) = match args.command {
        CtlCommand::Call {
            thread_id,
            tool,
            arguments,
        } => {
            let arguments = serde_json::from_str::<Value>(&arguments)
                .context("--arguments must be a JSON value")?;
            (thread_id, tool, arguments)
        }
        CtlCommand::Authorize { thread_id, minutes } => {
            if minutes == 0 || minutes > 24 * 60 {
                bail!("--minutes must be between 1 and 1440");
            }
            let ttl_seconds = minutes
                .checked_mul(60)
                .context("authorization TTL overflow")?;
            (
                thread_id,
                "_admin_authorize_task".to_owned(),
                json!({ "ttlSeconds": ttl_seconds }),
            )
        }
        CtlCommand::Doctor { thread_id, full } => (
            thread_id,
            "automation_doctor".to_owned(),
            json!({ "level": if full { "full" } else { "quick" } }),
        ),
    };
    let request = ControlRequest::new(
        OriginContext {
            thread_id,
            call_id: None,
        },
        tool,
        arguments,
    );
    let response = call_daemon(
        &config.control_socket_path(),
        &request,
        Duration::from_secs(20),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    if response.error.is_some() {
        bail!("daemon returned an error")
    }
    Ok(())
}

fn run_service(config: &Config, config_path: PathBuf, args: ServiceArgs) -> Result<()> {
    match args.command {
        ServiceCommand::Plan {
            platform,
            binary,
            destination,
            output,
        } => {
            let platform = platform.map_or_else(detect_platform, |value| Ok(value.into()))?;
            let binary = binary.map_or_else(std::env::current_exe, Ok)?;
            let plan = ServicePlan::build(
                platform,
                &binary,
                &config_path,
                &config.state_dir,
                &config.runtime_dir,
                &destination,
            )?;
            let encoded = serde_json::to_string_pretty(&plan)?;
            if let Some(output) = output {
                std::fs::write(&output, format!("{encoded}\n"))
                    .with_context(|| format!("failed to write {}", output.display()))?;
            } else {
                println!("{encoded}");
            }
            Ok(())
        }
        ServiceCommand::Apply { plan, plan_digest } => {
            let encoded = std::fs::read_to_string(&plan)
                .with_context(|| format!("failed to read {}", plan.display()))?;
            let plan: ServicePlan =
                serde_json::from_str(&encoded).context("invalid service plan")?;
            plan.apply(&plan_digest)?;
            println!("installed service file; activation was not run automatically");
            println!("activation command: {}", plan.activation_command.join(" "));
            Ok(())
        }
    }
}
