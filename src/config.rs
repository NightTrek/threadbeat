use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

const DEFAULT_POLL_MILLIS: u64 = 1_000;
const DEFAULT_MAX_FRAME_BYTES: usize = 1_048_576;
const DEFAULT_MAX_PROMPT_BYTES: usize = 32_768;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub app_server: Option<AppServerConfig>,
    pub scheduler: SchedulerConfig,
    pub delivery: DeliveryConfig,
    pub consent: ConsentConfig,
}

impl Default for Config {
    fn default() -> Self {
        let dirs = project_dirs();
        let state_dir = dirs.as_ref().map_or_else(
            || PathBuf::from(".threadbeat-dev/state"),
            |d| d.data_local_dir().to_path_buf(),
        );
        let runtime_dir = runtime_dir(&state_dir);
        Self {
            state_dir,
            runtime_dir,
            app_server: None,
            scheduler: SchedulerConfig::default(),
            delivery: DeliveryConfig::default(),
            consent: ConsentConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppServerConfig {
    pub endpoint: AppServerEndpoint,
    /// Task used only by the operator-invoked compatibility probe. Runtime
    /// delivery probes against the task it is about to target.
    #[serde(default)]
    pub probe_thread_id: Option<String>,
    #[serde(default = "default_connect_timeout_millis")]
    pub connect_timeout_millis: u64,
    #[serde(default = "default_request_timeout_millis")]
    pub request_timeout_millis: u64,
    #[serde(default = "default_max_frame_bytes")]
    pub max_frame_bytes: usize,
    #[serde(default)]
    pub verified_builds: Vec<VerifiedBuildConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedBuildConfig {
    pub exact_user_agent: String,
    pub exact_platform_family: String,
    pub exact_platform_os: String,
    pub evidence_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppServerEndpoint {
    /// A loopback-only WebSocket endpoint, including an SSH-proxied loopback listener.
    WebSocket {
        #[schemars(with = "String")]
        url: Url,
    },
    /// A local Unix socket speaking the app-server WebSocket transport.
    UnixWebSocket {
        path: PathBuf,
        #[serde(default = "default_unix_request_uri")]
        request_uri: String,
    },
    /// A fixed local proxy command, normally `codex app-server proxy` or a
    /// constrained SSH forced command that reaches an existing daemon.
    ProxyCommand {
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct SchedulerConfig {
    pub poll_millis: u64,
    pub max_prompt_bytes: usize,
    pub max_local_backlog_per_task: usize,
    pub max_remote_queue_depth: usize,
    pub clock_jump_quarantine_seconds: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            poll_millis: DEFAULT_POLL_MILLIS,
            max_prompt_bytes: DEFAULT_MAX_PROMPT_BYTES,
            max_local_backlog_per_task: 100,
            max_remote_queue_depth: 50,
            clock_jump_quarantine_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct DeliveryConfig {
    /// Delivery is opt-in until a released Codex build passes the semantic matrix.
    pub enabled: bool,
    /// When true, an unsupported or unverified app-server build blocks delivery.
    pub strict_compatibility: bool,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            strict_compatibility: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsentMode {
    #[default]
    CliGrant,
    TrustedAgentCurrentTask,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ConsentConfig {
    pub mode: ConsentMode,
    pub grant_ttl_seconds: u64,
}

impl Default for ConsentConfig {
    fn default() -> Self {
        Self {
            mode: ConsentMode::CliGrant,
            grant_ttl_seconds: 900,
        }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        project_dirs().map_or_else(
            || PathBuf::from(".threadbeat-dev/config.toml"),
            |d| d.config_dir().join("config.toml"),
        )
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path.map_or_else(Self::default_path, Path::to_path_buf);
        if !path.exists() {
            let config = Self::default();
            config.validate()?;
            return Ok(config);
        }

        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.state_dir.is_absolute() && !self.state_dir.starts_with(".threadbeat-dev") {
            bail!("stateDir must be absolute outside the repository development default");
        }
        if !self.runtime_dir.is_absolute() && !self.runtime_dir.starts_with(".threadbeat-dev") {
            bail!("runtimeDir must be absolute outside the repository development default");
        }
        if self.scheduler.poll_millis < 100 || self.scheduler.poll_millis > 60_000 {
            bail!("scheduler.pollMillis must be between 100 and 60000");
        }
        if !(1..=1_048_576).contains(&self.scheduler.max_prompt_bytes) {
            bail!("scheduler.maxPromptBytes must be between 1 and 1048576");
        }
        if self.scheduler.max_remote_queue_depth == 0 || self.scheduler.max_remote_queue_depth > 99
        {
            bail!("scheduler.maxRemoteQueueDepth must be between 1 and 99");
        }
        if self.scheduler.max_local_backlog_per_task == 0
            || self.scheduler.max_local_backlog_per_task > 10_000
        {
            bail!("scheduler.maxLocalBacklogPerTask must be between 1 and 10000");
        }
        if self.scheduler.clock_jump_quarantine_seconds == 0
            || self.scheduler.clock_jump_quarantine_seconds > 3_600
        {
            bail!("scheduler.clockJumpQuarantineSeconds must be between 1 and 3600");
        }
        if self.consent.grant_ttl_seconds < 60 || self.consent.grant_ttl_seconds > 86_400 {
            bail!("consent.grantTtlSeconds must be between 60 and 86400");
        }
        if !self.delivery.strict_compatibility {
            bail!("delivery.strictCompatibility must remain true in v1");
        }
        if self.delivery.enabled && self.app_server.is_none() {
            bail!("delivery cannot be enabled without appServer configuration");
        }
        if let Some(app_server) = &self.app_server {
            app_server.validate()?;
        }
        Ok(())
    }

    pub fn database_path(&self) -> PathBuf {
        self.state_dir.join("threadbeat.db")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.state_dir.join("daemon.lock")
    }

    pub fn control_socket_path(&self) -> PathBuf {
        self.runtime_dir.join("control.sock")
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.scheduler.poll_millis)
    }
}

impl AppServerConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_frame_bytes == 0 || self.max_frame_bytes > 16 * 1_048_576 {
            bail!("appServer.maxFrameBytes must be between 1 and 16777216");
        }
        if self.connect_timeout_millis == 0 || self.request_timeout_millis == 0 {
            bail!("app-server timeouts must be positive");
        }
        if self.probe_thread_id.as_ref().is_some_and(|thread_id| {
            thread_id.is_empty() || thread_id.len() > 256 || has_unsafe_text(thread_id)
        }) {
            bail!("appServer.probeThreadId is invalid");
        }
        for build in &self.verified_builds {
            if build.exact_user_agent.is_empty()
                || build.exact_user_agent.len() > 512
                || build.exact_platform_family.is_empty()
                || build.exact_platform_family.len() > 64
                || build.exact_platform_os.is_empty()
                || build.exact_platform_os.len() > 64
                || build.evidence_id.is_empty()
                || build.evidence_id.len() > 256
                || has_unsafe_text(&build.exact_user_agent)
                || has_unsafe_text(&build.exact_platform_family)
                || has_unsafe_text(&build.exact_platform_os)
                || has_unsafe_text(&build.evidence_id)
            {
                bail!("appServer.verifiedBuilds contains an invalid entry");
            }
        }
        match &self.endpoint {
            AppServerEndpoint::WebSocket { url } => validate_loopback_websocket(url),
            AppServerEndpoint::UnixWebSocket { path, request_uri } => {
                if !path.is_absolute() {
                    bail!("app-server Unix socket path must be absolute");
                }
                let uri = Url::parse(request_uri).context("invalid Unix WebSocket requestUri")?;
                if !matches!(uri.scheme(), "ws" | "http") {
                    bail!("Unix WebSocket requestUri must use ws or http");
                }
                Ok(())
            }
            AppServerEndpoint::ProxyCommand { program, args } => {
                if !program.is_absolute() {
                    bail!("app-server proxy program must be an absolute path");
                }
                if args.len() > 32 || args.iter().map(String::len).sum::<usize>() > 8_192 {
                    bail!("app-server proxy command exceeds argument limits");
                }
                if args.iter().any(|arg| arg.contains('\0')) {
                    bail!("app-server proxy argument contains NUL");
                }
                Ok(())
            }
        }
    }
}

fn validate_loopback_websocket(url: &Url) -> Result<()> {
    if url.scheme() != "ws" {
        bail!("v1 permits only ws:// through a loopback or constrained SSH proxy");
    }
    let Some(host) = url.host_str() else {
        bail!("app-server WebSocket URL needs a host");
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        bail!("app-server WebSocket URL must be loopback-only");
    }
    Ok(())
}

fn has_unsafe_text(value: &str) -> bool {
    value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
    })
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("dev", "NightTrek", "Threadbeat")
}

fn runtime_dir(state_dir: &Path) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || state_dir.join("run"),
        |base| PathBuf::from(base).join("threadbeat"),
    )
}

const fn default_connect_timeout_millis() -> u64 {
    5_000
}

const fn default_request_timeout_millis() -> u64 {
    15_000
}

const fn default_max_frame_bytes() -> usize {
    DEFAULT_MAX_FRAME_BYTES
}

fn default_unix_request_uri() -> String {
    "ws://localhost/".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_websocket() {
        let url = Url::parse("ws://192.0.2.1:4500").expect("valid URL");
        assert!(validate_loopback_websocket(&url).is_err());
    }

    #[test]
    fn accepts_loopback_websocket() {
        let url = Url::parse("ws://127.0.0.1:4500").expect("valid URL");
        assert!(validate_loopback_websocket(&url).is_ok());
    }

    #[test]
    fn packaged_example_config_parses_and_is_safe() {
        let config: Config = toml::from_str(include_str!("../config/threadbeat.example.toml"))
            .expect("example config parses");
        config.validate().expect("example config validates");
        assert!(!config.delivery.enabled);
        assert!(config.delivery.strict_compatibility);
        assert!(config.app_server.is_none());
    }
}
