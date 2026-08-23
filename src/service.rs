use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServicePlatform {
    SystemdUser,
    LaunchdAgent,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceFilePlan {
    pub path: PathBuf,
    pub mode: u32,
    pub sha256: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDirectoryPlan {
    pub path: PathBuf,
    pub mode: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServicePlan {
    pub version: u16,
    pub platform: ServicePlatform,
    pub binary: PathBuf,
    pub config: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub destination: PathBuf,
    pub directories: Vec<ServiceDirectoryPlan>,
    pub files: Vec<ServiceFilePlan>,
    pub activation_command: Vec<String>,
    pub removal_command: Vec<String>,
    pub digest: String,
}

impl ServicePlan {
    pub fn build(
        platform: ServicePlatform,
        binary: &Path,
        config: &Path,
        state_dir: &Path,
        runtime_dir: &Path,
        destination: &Path,
    ) -> Result<Self> {
        validate_plan_path(binary, "service binary")?;
        validate_plan_path(config, "service config")?;
        validate_plan_path(state_dir, "service state directory")?;
        validate_plan_path(runtime_dir, "service runtime directory")?;
        validate_plan_path(destination, "service destination")?;
        validate_destination(platform, destination)?;
        if state_dir == runtime_dir {
            bail!("service state and runtime directories must be distinct");
        }
        for managed in [state_dir, runtime_dir] {
            if binary.starts_with(managed)
                || config.starts_with(managed)
                || destination.starts_with(managed)
            {
                bail!("service-managed directories must not contain source or service files");
            }
        }
        let content = match platform {
            ServicePlatform::SystemdUser => render_systemd(binary, config, state_dir, runtime_dir),
            ServicePlatform::LaunchdAgent => render_launchd(binary, config),
        };
        let file = ServiceFilePlan {
            path: destination.to_path_buf(),
            mode: 0o600,
            sha256: sha256_hex(content.as_bytes()),
            content,
        };
        let (activation_command, removal_command) = match platform {
            ServicePlatform::SystemdUser => (
                vec![
                    "systemctl".to_owned(),
                    "--user".to_owned(),
                    "enable".to_owned(),
                    "--now".to_owned(),
                    "threadbeat.service".to_owned(),
                ],
                vec![
                    "systemctl".to_owned(),
                    "--user".to_owned(),
                    "disable".to_owned(),
                    "--now".to_owned(),
                    "threadbeat.service".to_owned(),
                ],
            ),
            ServicePlatform::LaunchdAgent => (
                vec![
                    "launchctl".to_owned(),
                    "bootstrap".to_owned(),
                    "gui/$UID".to_owned(),
                    destination.display().to_string(),
                ],
                vec![
                    "launchctl".to_owned(),
                    "bootout".to_owned(),
                    "gui/$UID/dev.nighttrek.threadbeat".to_owned(),
                ],
            ),
        };

        let mut plan = Self {
            version: 1,
            platform,
            binary: binary.to_path_buf(),
            config: config.to_path_buf(),
            state_dir: state_dir.to_path_buf(),
            runtime_dir: runtime_dir.to_path_buf(),
            destination: destination.to_path_buf(),
            directories: vec![
                ServiceDirectoryPlan {
                    path: state_dir.to_path_buf(),
                    mode: 0o700,
                },
                ServiceDirectoryPlan {
                    path: runtime_dir.to_path_buf(),
                    mode: 0o700,
                },
            ],
            files: vec![file],
            activation_command,
            removal_command,
            digest: String::new(),
        };
        plan.digest = plan.compute_digest()?;
        Ok(plan)
    }

    pub fn compute_digest(&self) -> Result<String> {
        let mut canonical = self.clone();
        canonical.digest.clear();
        let encoded = serde_json::to_vec(&canonical).context("failed to encode service plan")?;
        Ok(sha256_hex(&encoded))
    }

    pub fn apply(&self, expected_digest: &str) -> Result<()> {
        let actual = self.compute_digest()?;
        if expected_digest != actual || self.digest != actual {
            bail!("service plan digest mismatch; regenerate and review the plan");
        }
        if self.version != 1 {
            bail!("unsupported service plan version");
        }
        if detect_platform()? != self.platform {
            bail!("service plan platform does not match this host");
        }

        let expected = Self::build(
            self.platform,
            &self.binary,
            &self.config,
            &self.state_dir,
            &self.runtime_dir,
            &self.destination,
        )?;
        if self.directories != expected.directories
            || self.files != expected.files
            || self.activation_command != expected.activation_command
            || self.removal_command != expected.removal_command
        {
            bail!("service plan derived content is not canonical; regenerate the plan");
        }
        validate_source_file(&self.binary, true, "service binary")?;
        validate_source_file(&self.config, false, "service config")?;

        preflight_private_directory(&expected.directories[0])?;
        preflight_private_directory(&expected.directories[1])?;
        preflight_service_file(&expected.files[0])?;

        create_private_directory(&expected.directories[0])?;
        create_private_directory(&expected.directories[1])?;
        write_atomic_no_symlink(&expected.files[0])?;
        Ok(())
    }
}

fn validate_plan_path(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute() || path.file_name().is_none() {
        bail!("{label} path must be an absolute non-root path");
    }
    let text = path
        .to_str()
        .with_context(|| format!("{label} path must be valid UTF-8"))?;
    if text
        .chars()
        .any(|character| character.is_control() || is_bidi_control(character))
    {
        bail!("{label} path contains unsafe control text");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("{label} path must not contain dot components");
    }
    Ok(())
}

fn validate_destination(platform: ServicePlatform, destination: &Path) -> Result<()> {
    let expected_name = match platform {
        ServicePlatform::SystemdUser => "threadbeat.service",
        ServicePlatform::LaunchdAgent => "dev.nighttrek.threadbeat.plist",
    };
    if destination.file_name().and_then(|name| name.to_str()) != Some(expected_name) {
        bail!("service destination has an unexpected basename");
    }
    let parent = destination
        .parent()
        .context("service destination has no parent directory")?;
    let location_matches = match platform {
        ServicePlatform::SystemdUser => path_ends_with(parent, &["systemd", "user"]),
        ServicePlatform::LaunchdAgent => path_ends_with(parent, &["Library", "LaunchAgents"]),
    };
    if !location_matches {
        bail!("service destination is outside the expected platform location shape");
    }
    Ok(())
}

fn path_ends_with(path: &Path, suffix: &[&str]) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    components.len() >= suffix.len()
        && components[components.len() - suffix.len()..]
            .iter()
            .copied()
            .eq(suffix.iter().copied())
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn validate_source_file(path: &Path, executable: bool, label: &str) -> Result<()> {
    ensure_existing_components_no_symlinks(path)?;
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to inspect {label}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} must be a regular non-symlink file");
    }
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("service binary is not executable");
        }
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}

fn ensure_existing_components_no_symlinks(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!("refusing path with a symlink component");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn create_private_directory(plan: &ServiceDirectoryPlan) -> Result<()> {
    preflight_private_directory(plan)?;
    if plan.path.exists() {
        return Ok(());
    }
    create_private_dir(&plan.path)
        .with_context(|| format!("failed to create {}", plan.path.display()))?;
    ensure_existing_components_no_symlinks(&plan.path)?;
    let metadata = fs::symlink_metadata(&plan.path)
        .with_context(|| format!("failed to inspect {}", plan.path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("refusing non-directory or symlinked service state path");
    }
    verify_mode(&plan.path, 0o700, "service state directory")
}

fn preflight_private_directory(plan: &ServiceDirectoryPlan) -> Result<()> {
    if plan.mode != 0o700 {
        bail!("service state directories must use mode 0700");
    }
    ensure_existing_components_no_symlinks(&plan.path)?;
    match fs::symlink_metadata(&plan.path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("refusing non-directory or symlinked service state path");
            }
            verify_mode(&plan.path, 0o700, "existing service state directory")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = plan
                .path
                .parent()
                .context("service state directory has no parent")?;
            ensure_existing_directory_no_symlinks(parent, "service state parent")?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", plan.path.display()));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

pub fn detect_platform() -> Result<ServicePlatform> {
    if cfg!(target_os = "macos") {
        Ok(ServicePlatform::LaunchdAgent)
    } else if cfg!(target_os = "linux") {
        Ok(ServicePlatform::SystemdUser)
    } else {
        bail!("Threadbeat service setup supports macOS and Linux")
    }
}

fn write_atomic_no_symlink(plan: &ServiceFilePlan) -> Result<()> {
    preflight_service_file(plan)?;
    let parent = plan
        .path
        .parent()
        .context("service destination has no parent directory")?;

    let temporary = parent.join(format!(".threadbeat-{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<()> {
        let mut file = open_private_new(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        let temporary_metadata = fs::symlink_metadata(&temporary)
            .with_context(|| format!("failed to inspect {}", temporary.display()))?;
        if temporary_metadata.file_type().is_symlink() || !temporary_metadata.is_file() {
            bail!("temporary service file is not a regular file");
        }
        set_file_mode(&file, 0o600)?;
        verify_file_mode(&file, 0o600, "temporary service file")?;
        file.write_all(plan.content.as_bytes())
            .context("failed to write service file")?;
        file.sync_all().context("failed to sync service file")?;
        preflight_service_file(plan)?;
        fs::rename(&temporary, &plan.path)
            .with_context(|| format!("failed to install {}", plan.path.display()))?;
        validate_installed_file(plan)?;
        sync_directory(parent)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn preflight_service_file(plan: &ServiceFilePlan) -> Result<()> {
    if plan.mode != 0o600 {
        bail!("service files must use mode 0600");
    }
    let parent = plan
        .path
        .parent()
        .context("service destination has no parent directory")?;
    ensure_existing_directory_no_symlinks(parent, "service destination parent")?;

    match fs::symlink_metadata(&plan.path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("refusing to replace non-regular service destination");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", plan.path.display()));
        }
    }
    let parent_meta = fs::symlink_metadata(parent)
        .with_context(|| format!("failed to inspect {}", parent.display()))?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        bail!("refusing to write through a symlinked or non-directory parent");
    }
    if sha256_hex(plan.content.as_bytes()) != plan.sha256 {
        bail!("service file content digest mismatch");
    }
    Ok(())
}

fn open_private_new(path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn ensure_existing_directory_no_symlinks(path: &Path, label: &str) -> Result<()> {
    ensure_existing_components_no_symlinks(path)?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{label} must be an existing non-symlink directory");
    }
    Ok(())
}

fn validate_installed_file(plan: &ServiceFilePlan) -> Result<()> {
    let metadata = fs::symlink_metadata(&plan.path)
        .with_context(|| format!("failed to inspect {}", plan.path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("installed service destination is not a regular file");
    }
    verify_mode(&plan.path, 0o600, "installed service file")?;
    let content = fs::read(&plan.path)
        .with_context(|| format!("failed to verify {}", plan.path.display()))?;
    if sha256_hex(&content) != plan.sha256 || content != plan.content.as_bytes() {
        bail!("installed service file failed content verification");
    }
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(file: &fs::File, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .context("failed to set temporary service file permissions")
}

#[cfg(not(unix))]
fn set_file_mode(_file: &fs::File, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn verify_file_mode(file: &fs::File, expected: u32, label: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let actual = file
        .metadata()
        .with_context(|| format!("failed to inspect {label}"))?
        .permissions()
        .mode()
        & 0o777;
    if actual != expected {
        bail!("{label} permissions are not the required mode");
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_file_mode(_file: &fs::File, _expected: u32, _label: &str) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn verify_mode(path: &Path, expected: u32, label: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let actual = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label}"))?
        .permissions()
        .mode()
        & 0o777;
    if actual != expected {
        bail!("{label} permissions are not the required mode");
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_mode(_path: &Path, _expected: u32, _label: &str) -> Result<()> {
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    let directory = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("failed to open {} for sync", path.display()))?;
    directory
        .sync_all()
        .context("failed to sync service directory")
}

fn render_systemd(binary: &Path, config: &Path, state_dir: &Path, runtime_dir: &Path) -> String {
    format!(
        "[Unit]\nDescription=Threadbeat durable heartbeat scheduler\nAfter=network-online.target time-sync.target\nWants=network-online.target time-sync.target\n\n[Service]\nType=simple\nExecStart={} --config {} daemon\nRestart=on-failure\nRestartSec=5s\nNoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=read-only\nReadWritePaths={} {}\nLockPersonality=true\nMemoryDenyWriteExecute=true\nRestrictSUIDSGID=true\nLimitCORE=0\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(binary),
        systemd_quote(config),
        systemd_quote(state_dir),
        systemd_quote(runtime_dir)
    )
}

fn render_launchd(binary: &Path, config: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>dev.nighttrek.threadbeat</string>\n<key>ProgramArguments</key><array><string>{}</string><string>--config</string><string>{}</string><string>daemon</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ProcessType</key><string>Background</string>\n<key>SoftResourceLimits</key><dict><key>Core</key><integer>0</integer></dict>\n<key>HardResourceLimits</key><dict><key>Core</key><integer>0</integer></dict>\n<key>Umask</key><integer>63</integer>\n</dict></plist>\n",
        xml_escape(&binary.display().to_string()),
        xml_escape(&config.display().to_string())
    )
}

fn systemd_quote(path: &Path) -> String {
    let value = path.display().to_string().replace('%', "%%");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn applicable_plan() -> (TempDir, ServicePlan) {
        let directory = TempDir::new().expect("tempdir");
        let root = directory.path().canonicalize().expect("canonical tempdir");
        let binary = root.join("bin/threadbeat");
        let config = root.join("config/threadbeat.toml");
        fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary parent");
        fs::create_dir_all(config.parent().expect("config parent")).expect("config parent");
        fs::write(&binary, b"test binary").expect("binary");
        fs::write(&config, b"test = true\n").expect("config");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).expect("binary mode");
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("config mode");
        }
        let platform = detect_platform().expect("supported test platform");
        let destination = match platform {
            ServicePlatform::SystemdUser => root.join(".config/systemd/user/threadbeat.service"),
            ServicePlatform::LaunchdAgent => {
                root.join("Library/LaunchAgents/dev.nighttrek.threadbeat.plist")
            }
        };
        fs::create_dir_all(destination.parent().expect("destination parent"))
            .expect("destination parent");
        let state_parent = root.join("state-parent");
        let runtime_parent = root.join("runtime-parent");
        fs::create_dir(&state_parent).expect("state parent");
        fs::create_dir(&runtime_parent).expect("runtime parent");
        let plan = ServicePlan::build(
            platform,
            &binary,
            &config,
            &state_parent.join("state"),
            &runtime_parent.join("runtime"),
            &destination,
        )
        .expect("plan");
        (directory, plan)
    }

    #[test]
    fn plan_digest_detects_changes() {
        let mut plan = ServicePlan::build(
            ServicePlatform::SystemdUser,
            Path::new("/opt/threadbeat/bin/threadbeat"),
            Path::new("/etc/threadbeat/config.toml"),
            Path::new("/home/test/.local/share/threadbeat"),
            Path::new("/run/user/1000/threadbeat"),
            Path::new("/home/test/.config/systemd/user/threadbeat.service"),
        )
        .expect("build plan");
        let digest = plan.digest.clone();
        plan.files[0].content.push_str("# changed\n");
        assert_ne!(digest, plan.compute_digest().expect("digest"));
    }

    #[test]
    fn launchd_escapes_paths() {
        let content = render_launchd(
            Path::new("/tmp/a&b/threadbeat"),
            Path::new("/tmp/a<b/config.toml"),
        );
        assert!(content.contains("a&amp;b"));
        assert!(content.contains("a&lt;b"));
    }

    #[test]
    fn rejects_control_and_bidi_path_text() {
        let newline = ServicePlan::build(
            ServicePlatform::SystemdUser,
            Path::new("/opt/threadbeat\nExecStart=/tmp/attacker"),
            Path::new("/etc/threadbeat/config.toml"),
            Path::new("/home/test/.local/share/threadbeat"),
            Path::new("/run/user/1000/threadbeat"),
            Path::new("/home/test/.config/systemd/user/threadbeat.service"),
        );
        assert!(newline.is_err());

        let bidi = ServicePlan::build(
            ServicePlatform::LaunchdAgent,
            Path::new("/Applications/threadbeat"),
            Path::new("/Users/test/.config/threadbeat/config.toml"),
            Path::new("/Users/test/Library/Application Support/Threadbeat"),
            Path::new("/Users/test/Library/Caches/Threadbeat/run\u{202e}"),
            Path::new("/Users/test/Library/LaunchAgents/dev.nighttrek.threadbeat.plist"),
        );
        assert!(bidi.is_err());
    }

    #[test]
    fn rejects_unexpected_destination_shape() {
        assert!(
            ServicePlan::build(
                ServicePlatform::SystemdUser,
                Path::new("/opt/threadbeat/bin/threadbeat"),
                Path::new("/etc/threadbeat/config.toml"),
                Path::new("/home/test/.local/share/threadbeat"),
                Path::new("/run/user/1000/threadbeat"),
                Path::new("/home/test/.config/systemd/user/not-threadbeat.service"),
            )
            .is_err()
        );
        assert!(
            ServicePlan::build(
                ServicePlatform::LaunchdAgent,
                Path::new("/Applications/threadbeat"),
                Path::new("/Users/test/.config/threadbeat/config.toml"),
                Path::new("/Users/test/Library/Application Support/Threadbeat"),
                Path::new("/Users/test/Library/Caches/Threadbeat/run"),
                Path::new("/tmp/dev.nighttrek.threadbeat.plist"),
            )
            .is_err()
        );
    }

    #[test]
    fn apply_rejects_recomputed_plan_with_extra_file() {
        let (_directory, mut plan) = applicable_plan();
        let unauthorized = plan.state_dir.join("unauthorized.service");
        plan.files.push(ServiceFilePlan {
            path: unauthorized.clone(),
            mode: 0o600,
            sha256: sha256_hex(b"attacker"),
            content: "attacker".to_owned(),
        });
        plan.digest = plan.compute_digest().expect("attacker digest");
        assert!(plan.apply(&plan.digest).is_err());
        assert!(!unauthorized.exists());
        assert!(!plan.destination.exists());
    }

    #[test]
    fn apply_rejects_recomputed_mode_or_command_changes() {
        let (_directory, mut plan) = applicable_plan();
        plan.directories[0].mode = 0o755;
        plan.activation_command = vec!["attacker".to_owned()];
        plan.digest = plan.compute_digest().expect("attacker digest");
        assert!(plan.apply(&plan.digest).is_err());
        assert!(!plan.destination.exists());
    }

    #[test]
    fn canonical_apply_installs_one_private_file_and_directories() {
        let (_directory, plan) = applicable_plan();
        plan.apply(&plan.digest).expect("apply canonical plan");
        assert_eq!(
            fs::read(&plan.destination).expect("installed content"),
            plan.files[0].content.as_bytes()
        );
        verify_mode(&plan.state_dir, 0o700, "state").expect("state mode");
        verify_mode(&plan.runtime_dir, 0o700, "runtime").expect("runtime mode");
        verify_mode(&plan.destination, 0o600, "service").expect("service mode");
    }

    #[test]
    fn apply_refuses_unplanned_parent_directory_creation() {
        let (_directory, plan) = applicable_plan();
        fs::remove_dir(plan.state_dir.parent().expect("state parent"))
            .expect("remove empty state parent");
        assert!(plan.apply(&plan.digest).is_err());
        assert!(!plan.state_dir.exists());
        assert!(!plan.runtime_dir.exists());
        assert!(!plan.destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_to_repermission_existing_managed_directory() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, plan) = applicable_plan();
        fs::create_dir(&plan.state_dir).expect("state directory");
        fs::set_permissions(&plan.state_dir, fs::Permissions::from_mode(0o755))
            .expect("permissive state mode");
        assert!(plan.apply(&plan.digest).is_err());
        let mode = fs::symlink_metadata(&plan.state_dir)
            .expect("state metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
        assert!(!plan.destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_rejects_symlinked_managed_path_component() {
        use std::os::unix::fs::symlink;

        let (directory, baseline) = applicable_plan();
        let root = directory.path().canonicalize().expect("canonical tempdir");
        let real = root.join("real-state-parent");
        let link = root.join("linked-state-parent");
        fs::create_dir(&real).expect("real state parent");
        symlink(&real, &link).expect("state symlink");
        let plan = ServicePlan::build(
            baseline.platform,
            &baseline.binary,
            &baseline.config,
            &link.join("state"),
            &baseline.runtime_dir,
            &baseline.destination,
        )
        .expect("plan with not-yet-validated symlink component");
        assert!(plan.apply(&plan.digest).is_err());
        assert!(!real.join("state").exists());
        assert!(!plan.destination.exists());
    }
}
