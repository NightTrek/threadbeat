//! Minimal, bounded client for the subset of the Codex app-server protocol used by Threadbeat.
//!
//! Codex app-server is JSON-RPC-like but intentionally omits the `jsonrpc` member.  This module
//! supports text WebSocket frames over a loopback TCP socket or Unix socket, plus bounded JSONL
//! over a fixed proxy command such as constrained SSH invoking `codex app-server proxy`.
//!
//! The public mutation surface is deliberately narrow.  It cannot call `turn/start`, queue
//! reorder, or queue update.  Every queue mutation uses an exact queue item identity, and an
//! unverified or unsupported app-server build is read-only apart from the initialization probe.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::{WebSocketStream, client_async_with_config};
use url::{Host, Url};

const INITIALIZE_METHOD: &str = "initialize";
const INITIALIZED_METHOD: &str = "initialized";
const QUEUE_ADD_METHOD: &str = "thread/queue/add";
const QUEUE_LIST_METHOD: &str = "thread/queue/list";
const QUEUE_DELETE_METHOD: &str = "thread/queue/delete";
const QUEUE_START_METHOD: &str = "thread/queue/start";

const METHOD_NOT_FOUND: i64 = -32_601;
const MAX_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 32 * 1024;
const DEFAULT_QUEUE_PAGE_SIZE: usize = 25;

trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

type BoxedIo = Box<dyn AsyncIo>;
type Socket = WebSocketStream<BoxedIo>;

/// Transport endpoint.  All values are installation-time configuration, never per-MCP-call
/// arguments.
#[derive(Clone)]
pub enum AppServerEndpoint {
    /// Plain WebSocket is accepted only for an IP loopback address or the exact host `localhost`.
    LoopbackWebSocket(Url),
    /// WebSocket HTTP upgrade over an already-existing Unix domain socket using the safe default
    /// request URI `ws://localhost/`.
    UnixSocket(PathBuf),
    /// Unix-socket WebSocket with an explicit installation-time handshake URI.
    UnixSocketWithRequestUri { path: PathBuf, request_uri: Url },
    /// Bounded JSONL over a fixed absolute command without a shell.
    JsonlProxy(ProxyCommand),
}

/// Fixed process specification for a JSONL proxy.
///
/// A typical remote deployment uses an absolute local SSH executable and fixed arguments that
/// invoke `codex app-server proxy --sock /absolute/path` under a forced remote command.  Threadbeat
/// does not start app-server itself.
#[derive(Clone)]
pub struct ProxyCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl fmt::Debug for ProxyCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxyCommand")
            .field("program", &"[redacted]")
            .field("arg_count", &self.args.len())
            .finish()
    }
}

impl fmt::Debug for AppServerEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoopbackWebSocket(_) => formatter.write_str("LoopbackWebSocket([redacted])"),
            Self::UnixSocket(_) | Self::UnixSocketWithRequestUri { .. } => {
                formatter.write_str("UnixSocket([redacted])")
            }
            Self::JsonlProxy(command) => {
                formatter.debug_tuple("JsonlProxy").field(command).finish()
            }
        }
    }
}

/// Exact build entry backed by external semantic-test evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBuild {
    pub exact_user_agent: String,
    pub exact_platform_family: String,
    pub exact_platform_os: String,
    pub evidence_id: String,
}

impl VerifiedBuild {
    fn matches(&self, identity: &ServerIdentity) -> bool {
        self.exact_user_agent == identity.user_agent
            && self.exact_platform_family == identity.platform_family
            && self.exact_platform_os == identity.platform_os
    }
}

/// Hard transport/protocol bounds.  Custom values are still subject to a 16 MiB ceiling.
#[derive(Clone, Debug)]
pub struct AppServerLimits {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub write_timeout: Duration,
    pub request_timeout: Duration,
    pub max_incoming_message_bytes: usize,
    pub max_incoming_frame_bytes: usize,
    pub max_outgoing_message_bytes: usize,
    pub max_json_depth: usize,
    pub max_interstitial_messages: usize,
    pub max_queue_items: usize,
    pub max_queue_pages: usize,
    pub max_opaque_id_bytes: usize,
    pub max_cursor_bytes: usize,
}

impl Default for AppServerLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            max_incoming_message_bytes: 1024 * 1024,
            max_incoming_frame_bytes: 1024 * 1024,
            max_outgoing_message_bytes: 64 * 1024,
            max_json_depth: 64,
            max_interstitial_messages: 64,
            max_queue_items: 100,
            max_queue_pages: 8,
            max_opaque_id_bytes: 256,
            max_cursor_bytes: 4096,
        }
    }
}

impl AppServerLimits {
    fn validate(&self) -> Result<(), AppServerError> {
        let byte_limits = [
            self.max_incoming_message_bytes,
            self.max_incoming_frame_bytes,
            self.max_outgoing_message_bytes,
        ];
        if byte_limits
            .iter()
            .any(|limit| *limit == 0 || *limit > MAX_LIMIT_BYTES)
            || self.max_json_depth == 0
            || self.max_json_depth > 128
            || self.max_interstitial_messages == 0
            || self.max_interstitial_messages > 1024
            || self.max_queue_items == 0
            || self.max_queue_items > 100
            || self.max_queue_pages == 0
            || self.max_queue_pages > 128
            || self.max_opaque_id_bytes == 0
            || self.max_cursor_bytes == 0
        {
            return Err(AppServerError::new(
                AppServerErrorKind::InvalidConfiguration,
            ));
        }
        if [
            self.connect_timeout,
            self.handshake_timeout,
            self.write_timeout,
            self.request_timeout,
        ]
        .iter()
        .any(Duration::is_zero)
        {
            return Err(AppServerError::new(
                AppServerErrorKind::InvalidConfiguration,
            ));
        }
        Ok(())
    }
}

/// Complete client configuration.
#[derive(Clone)]
pub struct AppServerConfig {
    pub endpoint: AppServerEndpoint,
    /// A known task used only for the bounded `thread/queue/list` capability/shape probe.
    pub probe_thread_id: String,
    pub verified_builds: Vec<VerifiedBuild>,
    pub limits: AppServerLimits,
    pub client_name: String,
    pub client_title: Option<String>,
    pub client_version: String,
}

impl fmt::Debug for AppServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppServerConfig")
            .field("endpoint", &self.endpoint)
            .field("probe_thread_id", &"[redacted]")
            .field("verified_build_count", &self.verified_builds.len())
            .field("limits", &self.limits)
            .field("client_name", &self.client_name)
            .field("client_title", &self.client_title)
            .field("client_version", &self.client_version)
            .finish()
    }
}

impl AppServerConfig {
    #[must_use]
    pub fn new(endpoint: AppServerEndpoint, probe_thread_id: impl Into<String>) -> Self {
        Self {
            endpoint,
            probe_thread_id: probe_thread_id.into(),
            verified_builds: Vec::new(),
            limits: AppServerLimits::default(),
            client_name: "threadbeat".to_owned(),
            client_title: Some("Threadbeat".to_owned()),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    fn validate(&self) -> Result<(), AppServerError> {
        self.limits.validate()?;
        validate_opaque_id(&self.probe_thread_id, self.limits.max_opaque_id_bytes)?;
        validate_display_field(&self.client_name, 128)?;
        validate_display_field(&self.client_version, 128)?;
        if let Some(title) = &self.client_title {
            validate_display_field(title, 128)?;
        }
        for build in &self.verified_builds {
            validate_display_field(&build.exact_user_agent, 512)?;
            validate_display_field(&build.exact_platform_family, 64)?;
            validate_display_field(&build.exact_platform_os, 64)?;
            validate_display_field(&build.evidence_id, 256)?;
        }
        validate_endpoint(&self.endpoint)?;
        Ok(())
    }
}

/// Sanitized identity returned by `initialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerIdentity {
    pub user_agent: String,
    pub platform_family: String,
    pub platform_os: String,
}

/// Queue support is usable for mutations only in the `Verified` state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendStatus {
    Verified { evidence_id: String },
    Unverified,
    Unsupported,
}

impl BackendStatus {
    #[must_use]
    pub fn allows_mutation(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

/// Whether a failed operation may have reached the remote mutation boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteDisposition {
    NotStarted,
    MayHaveCommitted,
}

/// Sanitized error classification.  Raw remote messages, paths, prompts, and endpoints are never
/// retained in this error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppServerErrorKind {
    InvalidConfiguration,
    ConnectionFailed,
    Timeout,
    MessageTooLarge,
    ProtocolViolation,
    UnexpectedServerRequest,
    RemoteRejected,
    ConnectionClosed,
    UnverifiedBuild,
    UnsupportedBuild,
    OwnershipViolation,
    SharedQueueBlocked,
    ProxyFailed,
}

#[derive(Debug)]
pub struct AppServerError {
    kind: AppServerErrorKind,
    write_disposition: WriteDisposition,
    remote_code: Option<i64>,
}

impl AppServerError {
    fn new(kind: AppServerErrorKind) -> Self {
        Self {
            kind,
            write_disposition: WriteDisposition::NotStarted,
            remote_code: None,
        }
    }

    fn after_mutation(mut self) -> Self {
        self.write_disposition = WriteDisposition::MayHaveCommitted;
        self
    }

    fn remote(code: Option<i64>) -> Self {
        Self {
            kind: AppServerErrorKind::RemoteRejected,
            write_disposition: WriteDisposition::NotStarted,
            remote_code: code,
        }
    }

    #[must_use]
    pub fn kind(&self) -> AppServerErrorKind {
        self.kind
    }

    #[must_use]
    pub fn write_disposition(&self) -> WriteDisposition {
        self.write_disposition
    }

    #[must_use]
    pub fn remote_code(&self) -> Option<i64> {
        self.remote_code
    }
}

impl fmt::Display for AppServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            AppServerErrorKind::InvalidConfiguration => "app-server configuration is invalid",
            AppServerErrorKind::ConnectionFailed => "app-server connection failed",
            AppServerErrorKind::Timeout => "app-server operation timed out",
            AppServerErrorKind::MessageTooLarge => "app-server message exceeded a configured bound",
            AppServerErrorKind::ProtocolViolation => {
                "app-server returned an invalid protocol message"
            }
            AppServerErrorKind::UnexpectedServerRequest => {
                "app-server sent an unexpected request; no response was fabricated"
            }
            AppServerErrorKind::RemoteRejected => "app-server rejected the request",
            AppServerErrorKind::ConnectionClosed => "app-server connection closed",
            AppServerErrorKind::UnverifiedBuild => "app-server build is not semantically verified",
            AppServerErrorKind::UnsupportedBuild => "app-server queue backend is unsupported",
            AppServerErrorKind::OwnershipViolation => "remote queue ownership could not be proven",
            AppServerErrorKind::SharedQueueBlocked => "shared task queue is not safe to mutate",
            AppServerErrorKind::ProxyFailed => "app-server proxy failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AppServerError {}

/// Digest of canonical queue input.  The bytes, rather than a prompt preview, are used for exact
/// ownership checks.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputDigest([u8; 32]);

impl InputDigest {
    pub fn from_input(input: &[Value], max_depth: usize) -> Result<Self, AppServerError> {
        let value = Value::Array(input.to_vec());
        let mut canonical = Vec::new();
        write_canonical_json(&value, &mut canonical, 0, max_depth)?;
        let digest = Sha256::digest(&canonical);
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(&digest);
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for InputDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InputDigest(")?;
        formatter.write_str(&self.to_hex())?;
        formatter.write_str(")")
    }
}

/// Persisted local ownership evidence.  `client_user_message_id` must be generated randomly and
/// durably stored by the caller before invoking `add_exact`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueOwnership {
    pub thread_id: String,
    pub client_user_message_id: String,
    pub input_digest: InputDigest,
    pub queued_submission_id: Option<String>,
}

impl QueueOwnership {
    pub fn pending(
        thread_id: impl Into<String>,
        client_user_message_id: impl Into<String>,
        input: &[Value],
        limits: &AppServerLimits,
    ) -> Result<Self, AppServerError> {
        let thread_id = thread_id.into();
        let client_user_message_id = client_user_message_id.into();
        validate_opaque_id(&thread_id, limits.max_opaque_id_bytes)?;
        validate_opaque_id(&client_user_message_id, limits.max_opaque_id_bytes)?;
        validate_text_queue_input(input)?;
        Ok(Self {
            thread_id,
            client_user_message_id,
            input_digest: InputDigest::from_input(input, limits.max_json_depth)?,
            queued_submission_id: None,
        })
    }

    fn with_submission_id(mut self, id: String) -> Self {
        self.queued_submission_id = Some(id);
        self
    }
}

/// Content-free queue row view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueSubmission {
    pub id: String,
    pub client_user_message_id: String,
    pub input_digest: InputDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuePage {
    pub submissions: Vec<QueueSubmission>,
    pub next_cursor: Option<String>,
}

/// Exact reconciliation result.  More than one matching row is always a violation, even when the
/// rows are byte-for-byte identical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipResolution {
    Absent,
    Exact(QueueSubmission),
    Violation,
}

#[must_use]
pub fn reconcile_ownership(
    submissions: &[QueueSubmission],
    ownership: &QueueOwnership,
) -> OwnershipResolution {
    if let Some(remote_id) = &ownership.queued_submission_id {
        let id_matches: Vec<_> = submissions
            .iter()
            .filter(|row| &row.id == remote_id)
            .collect();
        if id_matches.is_empty() {
            return OwnershipResolution::Absent;
        }
        if id_matches.len() != 1
            || id_matches[0].client_user_message_id != ownership.client_user_message_id
            || id_matches[0].input_digest != ownership.input_digest
        {
            return OwnershipResolution::Violation;
        }
    }

    let candidates: Vec<_> = submissions
        .iter()
        .filter(|row| row.client_user_message_id == ownership.client_user_message_id)
        .collect();
    if candidates.is_empty() {
        return OwnershipResolution::Absent;
    }

    let exact: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|row| {
            row.input_digest == ownership.input_digest
                && ownership
                    .queued_submission_id
                    .as_ref()
                    .is_none_or(|remote_id| remote_id == &row.id)
        })
        .collect();

    if candidates.len() == 1 && exact.len() == 1 {
        OwnershipResolution::Exact(exact[0].clone())
    } else {
        OwnershipResolution::Violation
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueueAddOutcome {
    Added(QueueOwnership),
    AlreadyPresent(QueueOwnership),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueDeleteOutcome {
    Deleted,
    AbsentBeforeMutation,
    NotDeleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueStartOutcome {
    pub turn_id: String,
}

/// Content-free result of the configured compatibility probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub configured: bool,
    pub probe_thread_configured: bool,
    pub status: ProbeStatus,
    pub strict_queue_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_identity: Option<ProbeServerIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<AppServerErrorKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_code: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    NotConfigured,
    MissingProbeThread,
    Verified,
    Unverified,
    Unsupported,
    Failed,
}

/// Sanitized fields returned by initialize.  The server's Codex-home path is never retained.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeServerIdentity {
    pub user_agent: String,
    pub platform_family: String,
    pub platform_os: String,
}

/// Probe the endpoint selected in the operator-owned crate configuration.
///
/// Operational failures are returned as a sanitized report so `schema check` can always print a
/// useful result.  The outer `Result` is retained for callers that already use `?` and for future
/// local construction failures; it never contains a raw remote error message.
pub async fn probe_configured(
    config: &crate::config::Config,
) -> Result<ProbeReport, AppServerError> {
    let Some(configured) = &config.app_server else {
        return Ok(empty_probe_report(ProbeStatus::NotConfigured, false));
    };
    let Some(probe_thread_id) = configured.probe_thread_id.as_deref() else {
        return Ok(empty_probe_report(ProbeStatus::MissingProbeThread, true));
    };

    let adapter_config = match adapter_config_for_thread(configured, probe_thread_id) {
        Ok(adapter_config) => adapter_config,
        Err(error) => return Ok(failed_probe_report(&error)),
    };
    match AppServerClient::connect(adapter_config).await {
        Ok(client) => {
            let identity = ProbeServerIdentity {
                user_agent: client.identity.user_agent.clone(),
                platform_family: client.identity.platform_family.clone(),
                platform_os: client.identity.platform_os.clone(),
            };
            let (status, strict_queue_ready, evidence_id) = match &client.backend_status {
                BackendStatus::Verified { evidence_id } => {
                    (ProbeStatus::Verified, true, Some(evidence_id.clone()))
                }
                BackendStatus::Unverified => (ProbeStatus::Unverified, false, None),
                BackendStatus::Unsupported => (ProbeStatus::Unsupported, false, None),
            };
            Ok(ProbeReport {
                configured: true,
                probe_thread_configured: true,
                status,
                strict_queue_ready,
                server_identity: Some(identity),
                evidence_id,
                error_kind: None,
                remote_code: None,
            })
        }
        Err(error) => Ok(failed_probe_report(&error)),
    }
}

fn empty_probe_report(status: ProbeStatus, configured: bool) -> ProbeReport {
    ProbeReport {
        configured,
        probe_thread_configured: false,
        status,
        strict_queue_ready: false,
        server_identity: None,
        evidence_id: None,
        error_kind: None,
        remote_code: None,
    }
}

fn failed_probe_report(error: &AppServerError) -> ProbeReport {
    ProbeReport {
        configured: true,
        probe_thread_configured: true,
        status: ProbeStatus::Failed,
        strict_queue_ready: false,
        server_identity: None,
        evidence_id: None,
        error_kind: Some(error.kind()),
        remote_code: error.remote_code(),
    }
}

pub(crate) fn adapter_config_for_thread(
    configured: &crate::config::AppServerConfig,
    probe_thread_id: &str,
) -> Result<AppServerConfig, AppServerError> {
    let endpoint = match &configured.endpoint {
        crate::config::AppServerEndpoint::WebSocket { url } => {
            AppServerEndpoint::LoopbackWebSocket(url.clone())
        }
        crate::config::AppServerEndpoint::UnixWebSocket { path, request_uri } => {
            let request_uri = Url::parse(request_uri)
                .map_err(|_| AppServerError::new(AppServerErrorKind::InvalidConfiguration))?;
            AppServerEndpoint::UnixSocketWithRequestUri {
                path: path.clone(),
                request_uri,
            }
        }
        crate::config::AppServerEndpoint::ProxyCommand { program, args } => {
            AppServerEndpoint::JsonlProxy(ProxyCommand {
                program: program.clone(),
                args: args.iter().map(OsString::from).collect(),
            })
        }
    };

    let mut adapter = AppServerConfig::new(endpoint, probe_thread_id);
    adapter.limits.connect_timeout = Duration::from_millis(configured.connect_timeout_millis);
    adapter.limits.handshake_timeout = Duration::from_millis(configured.connect_timeout_millis);
    adapter.limits.write_timeout = Duration::from_millis(configured.request_timeout_millis);
    adapter.limits.request_timeout = Duration::from_millis(configured.request_timeout_millis);
    adapter.limits.max_incoming_message_bytes = configured.max_frame_bytes;
    adapter.limits.max_incoming_frame_bytes = configured.max_frame_bytes;
    adapter.limits.max_outgoing_message_bytes = adapter
        .limits
        .max_outgoing_message_bytes
        .min(configured.max_frame_bytes);
    adapter.verified_builds = configured
        .verified_builds
        .iter()
        .map(|build| VerifiedBuild {
            exact_user_agent: build.exact_user_agent.clone(),
            exact_platform_family: build.exact_platform_family.clone(),
            exact_platform_os: build.exact_platform_os.clone(),
            evidence_id: build.evidence_id.clone(),
        })
        .collect();
    env!("CARGO_PKG_VERSION").clone_into(&mut adapter.client_version);
    adapter.client_title = Some("Threadbeat compatibility probe".to_owned());

    // The adapter's own bounds and endpoint validation remain authoritative even if a Config was
    // constructed programmatically without calling Config::validate.
    adapter.validate()?;
    Ok(adapter)
}

/// Construct the only queue input shape emitted by this adapter.
pub fn text_queue_input(prompt: &str) -> Result<Vec<Value>, AppServerError> {
    if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    Ok(vec![json!({
        "type": "text",
        "text": prompt,
        "text_elements": []
    })])
}

fn validate_text_queue_input(input: &[Value]) -> Result<(), AppServerError> {
    let [Value::Object(item)] = input else {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    };
    let text = item.get("text").and_then(Value::as_str);
    let text_elements = item.get("text_elements").and_then(Value::as_array);
    if item.len() != 3
        || item.get("type").and_then(Value::as_str) != Some("text")
        || text.is_none_or(|value| value.is_empty() || value.len() > MAX_PROMPT_BYTES)
        || text_elements.is_none_or(|elements| !elements.is_empty())
    {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    Ok(())
}

pub struct AppServerClient {
    transport: Transport,
    limits: AppServerLimits,
    next_request_id: u64,
    identity: ServerIdentity,
    backend_status: BackendStatus,
}

impl AppServerClient {
    /// Connect, initialize with `experimentalApi=true`, send `initialized`, then probe
    /// `thread/queue/list` against a known disposable/current task.
    pub async fn connect(config: AppServerConfig) -> Result<Self, AppServerError> {
        config.validate()?;
        let transport = Transport::connect(&config.endpoint, &config.limits).await?;
        let mut client = Self {
            transport,
            limits: config.limits.clone(),
            next_request_id: 1,
            identity: ServerIdentity {
                user_agent: String::new(),
                platform_family: String::new(),
                platform_os: String::new(),
            },
            backend_status: BackendStatus::Unverified,
        };

        let initialize = client
            .request_value(
                INITIALIZE_METHOD,
                json!({
                    "clientInfo": {
                        "name": &config.client_name,
                        "title": &config.client_title,
                        "version": &config.client_version
                    },
                    "capabilities": {
                        "experimentalApi": true
                    }
                }),
                RequestEffect::ReadOnly,
            )
            .await?;
        client.identity = parse_initialize_response(initialize)?;
        client.send_notification(INITIALIZED_METHOD).await?;

        let probe = client
            .list_queue_page_raw(&config.probe_thread_id, None, 1)
            .await;
        match probe {
            Ok(_) => {
                client.backend_status = config
                    .verified_builds
                    .iter()
                    .find(|build| build.matches(&client.identity))
                    .map_or(BackendStatus::Unverified, |build| BackendStatus::Verified {
                        evidence_id: build.evidence_id.clone(),
                    });
            }
            Err(error) if error.remote_code() == Some(METHOD_NOT_FOUND) => {
                client.backend_status = BackendStatus::Unsupported;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    AppServerErrorKind::ProtocolViolation | AppServerErrorKind::MessageTooLarge
                ) =>
            {
                client.backend_status = BackendStatus::Unsupported;
            }
            Err(error) => return Err(error),
        }

        Ok(client)
    }

    #[must_use]
    pub fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    #[must_use]
    pub fn backend_status(&self) -> &BackendStatus {
        &self.backend_status
    }

    #[must_use]
    pub fn limits(&self) -> &AppServerLimits {
        &self.limits
    }

    /// List the complete bounded queue while preserving server FIFO order.
    pub async fn list_queue(
        &mut self,
        thread_id: &str,
    ) -> Result<Vec<QueueSubmission>, AppServerError> {
        self.ensure_verified()?;
        self.list_queue_all_raw(thread_id).await
    }

    /// Reconcile first, then add exactly one text-only queue entry.  If an exact single row already
    /// exists, no remote mutation occurs.
    pub async fn add_exact(
        &mut self,
        ownership: QueueOwnership,
        input: Vec<Value>,
    ) -> Result<QueueAddOutcome, AppServerError> {
        self.ensure_verified()?;
        self.validate_ownership_input(&ownership, &input)?;
        if ownership.queued_submission_id.is_some() {
            return Err(AppServerError::new(
                AppServerErrorKind::InvalidConfiguration,
            ));
        }

        let current = self.list_queue_all_raw(&ownership.thread_id).await?;
        match reconcile_ownership(&current, &ownership) {
            OwnershipResolution::Exact(row) => {
                return Ok(QueueAddOutcome::AlreadyPresent(
                    ownership.with_submission_id(row.id),
                ));
            }
            OwnershipResolution::Violation => {
                return Err(AppServerError::new(AppServerErrorKind::OwnershipViolation));
            }
            OwnershipResolution::Absent => {}
        }
        if current.len() >= self.limits.max_queue_items {
            return Err(AppServerError::new(AppServerErrorKind::SharedQueueBlocked));
        }

        let result = self
            .request_value(
                QUEUE_ADD_METHOD,
                json!({
                    "threadId": &ownership.thread_id,
                    "clientUserMessageId": &ownership.client_user_message_id,
                    "input": &input
                }),
                RequestEffect::Mutation,
            )
            .await?;
        let wire: WireQueueAddResponse =
            parse_typed(result).map_err(AppServerError::after_mutation)?;
        let row = wire
            .queued_submission
            .into_public(&self.limits)
            .map_err(AppServerError::after_mutation)?;
        if row.client_user_message_id != ownership.client_user_message_id
            || row.input_digest != ownership.input_digest
            || current.iter().any(|existing| existing.id == row.id)
        {
            return Err(
                AppServerError::new(AppServerErrorKind::OwnershipViolation).after_mutation()
            );
        }
        Ok(QueueAddOutcome::Added(ownership.with_submission_id(row.id)))
    }

    /// Delete only an exact owned remote row.  Absence is reported without issuing delete.
    pub async fn delete_exact(
        &mut self,
        ownership: &QueueOwnership,
    ) -> Result<QueueDeleteOutcome, AppServerError> {
        self.ensure_verified()?;
        let remote_id = ownership
            .queued_submission_id
            .as_deref()
            .ok_or_else(|| AppServerError::new(AppServerErrorKind::InvalidConfiguration))?;
        let current = self.list_queue_all_raw(&ownership.thread_id).await?;
        match reconcile_ownership(&current, ownership) {
            OwnershipResolution::Absent => return Ok(QueueDeleteOutcome::AbsentBeforeMutation),
            OwnershipResolution::Violation => {
                return Err(AppServerError::new(AppServerErrorKind::OwnershipViolation));
            }
            OwnershipResolution::Exact(_) => {}
        }

        let result = self
            .request_value(
                QUEUE_DELETE_METHOD,
                json!({
                    "threadId": &ownership.thread_id,
                    "queuedSubmissionId": remote_id
                }),
                RequestEffect::Mutation,
            )
            .await?;
        let response: WireQueueDeleteResponse =
            parse_typed(result).map_err(AppServerError::after_mutation)?;
        if response.deleted {
            Ok(QueueDeleteOutcome::Deleted)
        } else {
            // The row existed during the ownership check but may have started or been removed by
            // another client before this exact delete reached app-server.  Do not call that a
            // successful cancellation.
            Ok(QueueDeleteOutcome::NotDeleted)
        }
    }

    /// Start an exact owned item only when it is currently the shared FIFO head.  This method never
    /// sends a null/omitted queue ID and never falls back to `turn/start`.
    pub async fn start_exact_head(
        &mut self,
        ownership: &QueueOwnership,
    ) -> Result<QueueStartOutcome, AppServerError> {
        self.ensure_verified()?;
        let remote_id = ownership
            .queued_submission_id
            .as_deref()
            .ok_or_else(|| AppServerError::new(AppServerErrorKind::InvalidConfiguration))?;
        let current = self.list_queue_all_raw(&ownership.thread_id).await?;
        match reconcile_ownership(&current, ownership) {
            OwnershipResolution::Exact(_) => {}
            OwnershipResolution::Absent | OwnershipResolution::Violation => {
                return Err(AppServerError::new(AppServerErrorKind::OwnershipViolation));
            }
        }
        if current.first().map(|row| row.id.as_str()) != Some(remote_id) {
            return Err(AppServerError::new(AppServerErrorKind::SharedQueueBlocked));
        }

        let result = self
            .request_value(
                QUEUE_START_METHOD,
                json!({
                    "threadId": &ownership.thread_id,
                    "queuedSubmissionId": remote_id
                }),
                RequestEffect::Mutation,
            )
            .await?;
        let response: WireQueueStartResponse =
            parse_typed(result).map_err(AppServerError::after_mutation)?;
        validate_opaque_id(&response.turn.id, self.limits.max_opaque_id_bytes)
            .map_err(AppServerError::after_mutation)?;
        Ok(QueueStartOutcome {
            turn_id: response.turn.id,
        })
    }

    fn ensure_verified(&self) -> Result<(), AppServerError> {
        match &self.backend_status {
            BackendStatus::Verified { .. } => Ok(()),
            BackendStatus::Unverified => {
                Err(AppServerError::new(AppServerErrorKind::UnverifiedBuild))
            }
            BackendStatus::Unsupported => {
                Err(AppServerError::new(AppServerErrorKind::UnsupportedBuild))
            }
        }
    }

    fn validate_ownership_input(
        &self,
        ownership: &QueueOwnership,
        input: &[Value],
    ) -> Result<(), AppServerError> {
        validate_text_queue_input(input)?;
        validate_opaque_id(&ownership.thread_id, self.limits.max_opaque_id_bytes)?;
        validate_opaque_id(
            &ownership.client_user_message_id,
            self.limits.max_opaque_id_bytes,
        )?;
        if InputDigest::from_input(input, self.limits.max_json_depth)? != ownership.input_digest {
            return Err(AppServerError::new(AppServerErrorKind::OwnershipViolation));
        }
        Ok(())
    }

    async fn list_queue_all_raw(
        &mut self,
        thread_id: &str,
    ) -> Result<Vec<QueueSubmission>, AppServerError> {
        validate_opaque_id(thread_id, self.limits.max_opaque_id_bytes)?;
        let mut all = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let page_size = DEFAULT_QUEUE_PAGE_SIZE.min(self.limits.max_queue_items);

        for _ in 0..self.limits.max_queue_pages {
            let page = self
                .list_queue_page_raw(thread_id, cursor.as_deref(), page_size)
                .await?;
            if all.len().saturating_add(page.submissions.len()) > self.limits.max_queue_items {
                return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
            }
            all.extend(page.submissions);
            match page.next_cursor {
                None => return Ok(all),
                Some(next) => {
                    if !seen_cursors.insert(next.clone()) {
                        return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
                    }
                    cursor = Some(next);
                }
            }
        }
        Err(AppServerError::new(AppServerErrorKind::ProtocolViolation))
    }

    async fn list_queue_page_raw(
        &mut self,
        thread_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<QueuePage, AppServerError> {
        validate_opaque_id(thread_id, self.limits.max_opaque_id_bytes)?;
        if limit == 0 || limit > self.limits.max_queue_items {
            return Err(AppServerError::new(
                AppServerErrorKind::InvalidConfiguration,
            ));
        }
        if let Some(value) = cursor {
            validate_opaque_id(value, self.limits.max_cursor_bytes)?;
        }
        let result = self
            .request_value(
                QUEUE_LIST_METHOD,
                json!({
                    "threadId": thread_id,
                    "cursor": cursor,
                    "limit": limit
                }),
                RequestEffect::ReadOnly,
            )
            .await?;
        let wire: WireQueueListResponse = parse_typed(result)?;
        if wire.data.len() > limit || wire.data.len() > self.limits.max_queue_items {
            return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
        }
        let submissions = wire
            .data
            .into_iter()
            .map(|row| row.into_public(&self.limits))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(cursor) = &wire.next_cursor {
            validate_opaque_id(cursor, self.limits.max_cursor_bytes)?;
        }
        Ok(QueuePage {
            submissions,
            next_cursor: wire.next_cursor,
        })
    }

    async fn send_notification(&mut self, method: &'static str) -> Result<(), AppServerError> {
        let bytes = serde_json::to_vec(&json!({ "method": method }))
            .map_err(|_| AppServerError::new(AppServerErrorKind::ProtocolViolation))?;
        if bytes.len() > self.limits.max_outgoing_message_bytes {
            return Err(AppServerError::new(AppServerErrorKind::MessageTooLarge));
        }
        timeout(self.limits.write_timeout, self.transport.send_text(bytes))
            .await
            .map_err(|_| AppServerError::new(AppServerErrorKind::Timeout))??;
        Ok(())
    }

    async fn request_value(
        &mut self,
        method: &'static str,
        params: Value,
        effect: RequestEffect,
    ) -> Result<Value, AppServerError> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| AppServerError::new(AppServerErrorKind::ProtocolViolation))?;
        let bytes = serde_json::to_vec(&json!({
            "id": id,
            "method": method,
            "params": params
        }))
        .map_err(|_| AppServerError::new(AppServerErrorKind::ProtocolViolation))?;
        if bytes.len() > self.limits.max_outgoing_message_bytes {
            return Err(AppServerError::new(AppServerErrorKind::MessageTooLarge));
        }

        let send_result = timeout(self.limits.write_timeout, self.transport.send_text(bytes)).await;
        match send_result {
            Err(_) => {
                let error = AppServerError::new(AppServerErrorKind::Timeout);
                return Err(effect.mark_after_send(error));
            }
            Ok(Err(error)) => return Err(effect.mark_after_send(error)),
            Ok(Ok(())) => {}
        }

        let response = timeout(
            self.limits.request_timeout,
            self.wait_for_response(id, effect),
        )
        .await
        .map_err(|_| effect.mark_after_send(AppServerError::new(AppServerErrorKind::Timeout)))??;
        Ok(response)
    }

    async fn wait_for_response(
        &mut self,
        expected_id: u64,
        effect: RequestEffect,
    ) -> Result<Value, AppServerError> {
        let mut interstitial = 0_usize;
        loop {
            let bytes = self
                .transport
                .receive_text(&self.limits)
                .await
                .map_err(|error| effect.mark_after_send(error))?;
            if bytes.len() > self.limits.max_incoming_message_bytes {
                return Err(effect
                    .mark_after_send(AppServerError::new(AppServerErrorKind::MessageTooLarge)));
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                effect.mark_after_send(AppServerError::new(AppServerErrorKind::ProtocolViolation))
            })?;
            if json_depth(&value, 0) > self.limits.max_json_depth {
                return Err(effect
                    .mark_after_send(AppServerError::new(AppServerErrorKind::ProtocolViolation)));
            }
            match classify_rpc_message(&value, expected_id) {
                ParsedRpcMessage::Response(result) => return Ok(result),
                ParsedRpcMessage::RemoteError(code) => {
                    return Err(effect.mark_after_send(AppServerError::remote(code)));
                }
                ParsedRpcMessage::Notification => {
                    interstitial = interstitial.saturating_add(1);
                    if interstitial > self.limits.max_interstitial_messages {
                        return Err(effect.mark_after_send(AppServerError::new(
                            AppServerErrorKind::ProtocolViolation,
                        )));
                    }
                }
                ParsedRpcMessage::ServerRequest => {
                    return Err(effect.mark_after_send(AppServerError::new(
                        AppServerErrorKind::UnexpectedServerRequest,
                    )));
                }
                ParsedRpcMessage::Invalid => {
                    return Err(effect.mark_after_send(AppServerError::new(
                        AppServerErrorKind::ProtocolViolation,
                    )));
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RequestEffect {
    ReadOnly,
    Mutation,
}

impl RequestEffect {
    fn mark_after_send(self, error: AppServerError) -> AppServerError {
        match self {
            Self::ReadOnly => error,
            Self::Mutation => error.after_mutation(),
        }
    }
}

enum Transport {
    WebSocket(Socket),
    Jsonl(JsonlProxyTransport),
}

impl Transport {
    async fn connect(
        endpoint: &AppServerEndpoint,
        limits: &AppServerLimits,
    ) -> Result<Self, AppServerError> {
        match endpoint {
            AppServerEndpoint::LoopbackWebSocket(url) => {
                let address = loopback_socket_address(url)?;
                let stream = timeout(limits.connect_timeout, TcpStream::connect(address))
                    .await
                    .map_err(|_| AppServerError::new(AppServerErrorKind::Timeout))?
                    .map_err(|_| AppServerError::new(AppServerErrorKind::ConnectionFailed))?;
                let _ = stream.set_nodelay(true);
                Self::connect_websocket(url.as_str(), Box::new(stream), limits).await
            }
            AppServerEndpoint::UnixSocket(path) => {
                #[cfg(unix)]
                {
                    validate_unix_socket(path).await?;
                    let stream = timeout(limits.connect_timeout, UnixStream::connect(path))
                        .await
                        .map_err(|_| AppServerError::new(AppServerErrorKind::Timeout))?
                        .map_err(|_| AppServerError::new(AppServerErrorKind::ConnectionFailed))?;
                    Self::connect_websocket("ws://localhost/", Box::new(stream), limits).await
                }
                #[cfg(not(unix))]
                {
                    let _ = path;
                    Err(AppServerError::new(
                        AppServerErrorKind::InvalidConfiguration,
                    ))
                }
            }
            AppServerEndpoint::UnixSocketWithRequestUri { path, request_uri } => {
                #[cfg(unix)]
                {
                    validate_unix_socket(path).await?;
                    let stream = timeout(limits.connect_timeout, UnixStream::connect(path))
                        .await
                        .map_err(|_| AppServerError::new(AppServerErrorKind::Timeout))?
                        .map_err(|_| AppServerError::new(AppServerErrorKind::ConnectionFailed))?;
                    Self::connect_websocket(request_uri.as_str(), Box::new(stream), limits).await
                }
                #[cfg(not(unix))]
                {
                    let _ = (path, request_uri);
                    Err(AppServerError::new(
                        AppServerErrorKind::InvalidConfiguration,
                    ))
                }
            }
            AppServerEndpoint::JsonlProxy(command) => {
                Ok(Self::Jsonl(JsonlProxyTransport::spawn(command, limits)?))
            }
        }
    }

    async fn connect_websocket(
        request_uri: &str,
        stream: BoxedIo,
        limits: &AppServerLimits,
    ) -> Result<Self, AppServerError> {
        let config = WebSocketConfig::default()
            .read_buffer_size(16 * 1024)
            .write_buffer_size(16 * 1024)
            .max_write_buffer_size(128 * 1024)
            .max_message_size(Some(limits.max_incoming_message_bytes))
            .max_frame_size(Some(limits.max_incoming_frame_bytes));
        let (socket, response) = timeout(
            limits.handshake_timeout,
            client_async_with_config(request_uri, stream, Some(config)),
        )
        .await
        .map_err(|_| AppServerError::new(AppServerErrorKind::Timeout))?
        .map_err(|_| AppServerError::new(AppServerErrorKind::ConnectionFailed))?;
        if response.headers().contains_key("sec-websocket-extensions") {
            return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
        }
        Ok(Self::WebSocket(socket))
    }

    async fn send_text(&mut self, bytes: Vec<u8>) -> Result<(), AppServerError> {
        match self {
            Self::WebSocket(socket) => {
                let text = String::from_utf8(bytes)
                    .map_err(|_| AppServerError::new(AppServerErrorKind::ProtocolViolation))?;
                socket
                    .send(Message::Text(text.into()))
                    .await
                    .map_err(|_| AppServerError::new(AppServerErrorKind::ConnectionClosed))
            }
            Self::Jsonl(proxy) => proxy.send_line(bytes),
        }
    }

    async fn receive_text(&mut self, limits: &AppServerLimits) -> Result<Vec<u8>, AppServerError> {
        match self {
            Self::Jsonl(proxy) => proxy.receive_line().await,
            Self::WebSocket(socket) => loop {
                let message = socket
                    .next()
                    .await
                    .ok_or_else(|| AppServerError::new(AppServerErrorKind::ConnectionClosed))?
                    .map_err(|_| AppServerError::new(AppServerErrorKind::ConnectionClosed))?;
                match message {
                    Message::Text(text) => {
                        if text.len() > limits.max_incoming_message_bytes {
                            return Err(AppServerError::new(AppServerErrorKind::MessageTooLarge));
                        }
                        return Ok(text.as_bytes().to_vec());
                    }
                    Message::Ping(payload) => {
                        if payload.len() > 125 {
                            return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
                        }
                        socket.send(Message::Pong(payload)).await.map_err(|_| {
                            AppServerError::new(AppServerErrorKind::ConnectionClosed)
                        })?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => {
                        return Err(AppServerError::new(AppServerErrorKind::ConnectionClosed));
                    }
                    Message::Binary(_) | Message::Frame(_) => {
                        return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
                    }
                }
            },
        }
    }
}

enum ProxyEvent {
    Line(Vec<u8>),
    Failure,
    TooLarge,
    Eof,
}

struct JsonlProxyTransport {
    child: Option<Child>,
    writer: Option<SyncSender<Vec<u8>>>,
    incoming: tokio_mpsc::Receiver<ProxyEvent>,
    writer_thread: Option<JoinHandle<()>>,
    reader_thread: Option<JoinHandle<()>>,
}

impl JsonlProxyTransport {
    fn spawn(command: &ProxyCommand, limits: &AppServerLimits) -> Result<Self, AppServerError> {
        validate_proxy_command(command)?;
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| AppServerError::new(AppServerErrorKind::ProxyFailed))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppServerError::new(AppServerErrorKind::ProxyFailed))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppServerError::new(AppServerErrorKind::ProxyFailed))?;

        let (writer, writer_rx) = mpsc::sync_channel::<Vec<u8>>(8);
        let (event_tx, incoming) = tokio_mpsc::channel::<ProxyEvent>(8);
        let writer_event_tx = event_tx.clone();
        let writer_thread = std::thread::Builder::new()
            .name("threadbeat-proxy-write".to_owned())
            .spawn(move || proxy_writer_loop(stdin, writer_rx, writer_event_tx))
            .map_err(|_| AppServerError::new(AppServerErrorKind::ProxyFailed))?;
        let max_line = limits.max_incoming_message_bytes;
        let reader_thread = std::thread::Builder::new()
            .name("threadbeat-proxy-read".to_owned())
            .spawn(move || proxy_reader_loop(stdout, event_tx, max_line))
            .map_err(|_| AppServerError::new(AppServerErrorKind::ProxyFailed))?;

        Ok(Self {
            child: Some(child),
            writer: Some(writer),
            incoming,
            writer_thread: Some(writer_thread),
            reader_thread: Some(reader_thread),
        })
    }

    fn send_line(&self, bytes: Vec<u8>) -> Result<(), AppServerError> {
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| AppServerError::new(AppServerErrorKind::ProxyFailed))?;
        writer.try_send(bytes).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                AppServerError::new(AppServerErrorKind::ProxyFailed)
            }
        })
    }

    async fn receive_line(&mut self) -> Result<Vec<u8>, AppServerError> {
        match self.incoming.recv().await {
            Some(ProxyEvent::Line(line)) => Ok(line),
            Some(ProxyEvent::TooLarge) => {
                Err(AppServerError::new(AppServerErrorKind::MessageTooLarge))
            }
            Some(ProxyEvent::Failure | ProxyEvent::Eof) | None => {
                Err(AppServerError::new(AppServerErrorKind::ProxyFailed))
            }
        }
    }
}

impl Drop for JsonlProxyTransport {
    fn drop(&mut self) {
        self.writer.take();
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child.take();
        // Dropping a JoinHandle detaches it.  Joining here could deadlock if a proxy emitted enough
        // unsolicited lines to fill the bounded receiver while the client is being dropped.  Once
        // Drop returns, the receiver closes and both threads terminate naturally.
        self.writer_thread.take();
        self.reader_thread.take();
    }
}

fn proxy_writer_loop(
    mut stdin: std::process::ChildStdin,
    receiver: mpsc::Receiver<Vec<u8>>,
    events: tokio_mpsc::Sender<ProxyEvent>,
) {
    for mut line in receiver {
        line.push(b'\n');
        if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
            let _ = events.blocking_send(ProxyEvent::Failure);
            return;
        }
    }
}

fn proxy_reader_loop(
    stdout: std::process::ChildStdout,
    events: tokio_mpsc::Sender<ProxyEvent>,
    max_line: usize,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_bounded_line(&mut reader, max_line) {
            Ok(Some(line)) => {
                if events.blocking_send(ProxyEvent::Line(line)).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = events.blocking_send(ProxyEvent::Eof);
                return;
            }
            Err(ReadLineError::TooLarge) => {
                let _ = events.blocking_send(ProxyEvent::TooLarge);
                return;
            }
            Err(ReadLineError::Io) => {
                let _ = events.blocking_send(ProxyEvent::Failure);
                return;
            }
        }
    }
}

enum ReadLineError {
    TooLarge,
    Io,
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_line: usize,
) -> Result<Option<Vec<u8>>, ReadLineError> {
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| ReadLineError::Io)?;
        if available.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Err(ReadLineError::Io)
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        let payload_bytes = newline.map_or(take, |_| take.saturating_sub(1));
        if output.len().saturating_add(payload_bytes) > max_line {
            return Err(ReadLineError::TooLarge);
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            if output.last() == Some(&b'\n') {
                output.pop();
            }
            if output.last() == Some(&b'\r') {
                output.pop();
            }
            if output.len() > max_line {
                return Err(ReadLineError::TooLarge);
            }
            return Ok(Some(output));
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireInitializeResponse {
    user_agent: String,
    platform_family: String,
    platform_os: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireQueuedSubmission {
    id: String,
    client_user_message_id: String,
    input: Vec<Value>,
}

impl WireQueuedSubmission {
    fn into_public(self, limits: &AppServerLimits) -> Result<QueueSubmission, AppServerError> {
        validate_opaque_id(&self.id, limits.max_opaque_id_bytes)?;
        validate_opaque_id(&self.client_user_message_id, limits.max_opaque_id_bytes)?;
        Ok(QueueSubmission {
            id: self.id,
            client_user_message_id: self.client_user_message_id,
            input_digest: InputDigest::from_input(&self.input, limits.max_json_depth)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireQueueListResponse {
    data: Vec<WireQueuedSubmission>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireQueueAddResponse {
    queued_submission: WireQueuedSubmission,
}

#[derive(Deserialize)]
struct WireQueueDeleteResponse {
    deleted: bool,
}

#[derive(Deserialize)]
struct WireQueueStartResponse {
    turn: WireTurn,
}

#[derive(Deserialize)]
struct WireTurn {
    id: String,
}

fn parse_initialize_response(value: Value) -> Result<ServerIdentity, AppServerError> {
    let wire: WireInitializeResponse = parse_typed(value)?;
    Ok(ServerIdentity {
        user_agent: sanitize_server_field(&wire.user_agent, 512)?,
        platform_family: sanitize_server_field(&wire.platform_family, 64)?,
        platform_os: sanitize_server_field(&wire.platform_os, 64)?,
    })
}

fn parse_typed<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, AppServerError> {
    serde_json::from_value(value)
        .map_err(|_| AppServerError::new(AppServerErrorKind::ProtocolViolation))
}

enum ParsedRpcMessage {
    Response(Value),
    RemoteError(Option<i64>),
    Notification,
    ServerRequest,
    Invalid,
}

fn classify_rpc_message(value: &Value, expected_id: u64) -> ParsedRpcMessage {
    let Some(object) = value.as_object() else {
        return ParsedRpcMessage::Invalid;
    };
    let id = object.get("id");
    let method = object.get("method").and_then(Value::as_str);
    let result = object.get("result");
    let error = object.get("error");

    if id.is_none() && method.is_some() && result.is_none() && error.is_none() {
        return ParsedRpcMessage::Notification;
    }
    if id.is_some() && method.is_some() && result.is_none() && error.is_none() {
        return ParsedRpcMessage::ServerRequest;
    }
    if id.and_then(Value::as_u64) != Some(expected_id) || method.is_some() {
        return ParsedRpcMessage::Invalid;
    }
    match (result, error) {
        (Some(result), None) => ParsedRpcMessage::Response(result.clone()),
        (None, Some(error)) => ParsedRpcMessage::RemoteError(
            error
                .as_object()
                .and_then(|body| body.get("code"))
                .and_then(Value::as_i64),
        ),
        _ => ParsedRpcMessage::Invalid,
    }
}

fn validate_endpoint(endpoint: &AppServerEndpoint) -> Result<(), AppServerError> {
    match endpoint {
        AppServerEndpoint::LoopbackWebSocket(url) => loopback_socket_address(url).map(|_| ()),
        AppServerEndpoint::UnixSocket(path) => {
            if !path.is_absolute() || path.as_os_str().is_empty() {
                return Err(AppServerError::new(
                    AppServerErrorKind::InvalidConfiguration,
                ));
            }
            Ok(())
        }
        AppServerEndpoint::UnixSocketWithRequestUri { path, request_uri } => {
            if !path.is_absolute()
                || path.as_os_str().is_empty()
                || !matches!(request_uri.scheme(), "ws" | "http")
                || request_uri.host().is_none()
                || !request_uri.username().is_empty()
                || request_uri.password().is_some()
                || request_uri.fragment().is_some()
            {
                return Err(AppServerError::new(
                    AppServerErrorKind::InvalidConfiguration,
                ));
            }
            Ok(())
        }
        AppServerEndpoint::JsonlProxy(command) => validate_proxy_command(command),
    }
}

fn loopback_socket_address(url: &Url) -> Result<SocketAddr, AppServerError> {
    if url.scheme() != "ws"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| AppServerError::new(AppServerErrorKind::InvalidConfiguration))?;
    let ip = match url
        .host()
        .ok_or_else(|| AppServerError::new(AppServerErrorKind::InvalidConfiguration))?
    {
        Host::Ipv4(address) if address.is_loopback() => std::net::IpAddr::V4(address),
        Host::Ipv6(address) if address.is_loopback() => std::net::IpAddr::V6(address),
        Host::Domain(domain) if domain.eq_ignore_ascii_case("localhost") => {
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)
        }
        Host::Ipv4(_) | Host::Ipv6(_) | Host::Domain(_) => {
            return Err(AppServerError::new(
                AppServerErrorKind::InvalidConfiguration,
            ));
        }
    };
    Ok(SocketAddr::new(ip, port))
}

fn validate_proxy_command(command: &ProxyCommand) -> Result<(), AppServerError> {
    if !command.program.is_absolute()
        || command.program.as_os_str().is_empty()
        || command.args.len() > 64
    {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    let total = os_len(command.program.as_os_str()).saturating_add(
        command
            .args
            .iter()
            .map(|arg| os_len(arg.as_os_str()))
            .sum::<usize>(),
    );
    if total > 16 * 1024
        || command
            .args
            .iter()
            .any(|arg| arg.to_string_lossy().contains('\0'))
    {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    Ok(())
}

fn os_len(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[cfg(unix)]
async fn validate_unix_socket(path: &Path) -> Result<(), AppServerError> {
    use std::os::unix::fs::FileTypeExt;

    if !path.is_absolute() {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| AppServerError::new(AppServerErrorKind::ConnectionFailed))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    Ok(())
}

fn validate_display_field(value: &str, max_bytes: usize) -> Result<(), AppServerError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value
            .chars()
            .any(|character| character.is_control() || is_bidi_override(character))
    {
        return Err(AppServerError::new(
            AppServerErrorKind::InvalidConfiguration,
        ));
    }
    Ok(())
}

fn validate_opaque_id(value: &str, max_bytes: usize) -> Result<(), AppServerError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value
            .chars()
            .any(|character| character.is_control() || is_bidi_override(character))
    {
        return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
    }
    Ok(())
}

fn sanitize_server_field(value: &str, max_bytes: usize) -> Result<String, AppServerError> {
    validate_opaque_id(value, max_bytes)?;
    Ok(value.to_owned())
}

fn is_bidi_override(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn json_depth(value: &Value, depth: usize) -> usize {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|child| json_depth(child, depth.saturating_add(1)))
            .max()
            .unwrap_or(depth),
        Value::Object(values) => values
            .values()
            .map(|child| json_depth(child, depth.saturating_add(1)))
            .max()
            .unwrap_or(depth),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => depth,
    }
}

fn write_canonical_json(
    value: &Value,
    output: &mut Vec<u8>,
    depth: usize,
    max_depth: usize,
) -> Result<(), AppServerError> {
    if depth > max_depth {
        return Err(AppServerError::new(AppServerErrorKind::ProtocolViolation));
    }
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(boolean) => {
            if *boolean {
                output.extend_from_slice(b"true");
            } else {
                output.extend_from_slice(b"false");
            }
        }
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(string) => serde_json::to_writer(&mut *output, string)
            .map_err(|_| AppServerError::new(AppServerErrorKind::ProtocolViolation))?,
        Value::Array(values) => {
            output.push(b'[');
            for (index, child) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(child, output, depth.saturating_add(1), max_depth)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key)
                    .map_err(|_| AppServerError::new(AppServerErrorKind::ProtocolViolation))?;
                output.push(b':');
                write_canonical_json(
                    values.get(key).ok_or_else(|| {
                        AppServerError::new(AppServerErrorKind::ProtocolViolation)
                    })?,
                    output,
                    depth.saturating_add(1),
                    max_depth,
                )?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> AppServerLimits {
        AppServerLimits::default()
    }

    fn submission(id: &str, client_id: &str, input: &[Value]) -> QueueSubmission {
        QueueSubmission {
            id: id.to_owned(),
            client_user_message_id: client_id.to_owned(),
            input_digest: InputDigest::from_input(input, 64).expect("digest"),
        }
    }

    #[test]
    fn canonical_digest_ignores_object_key_order() {
        use serde_json::Map;

        let first = vec![json!({"type":"text", "text":"hello", "text_elements":[]})];
        let mut object = Map::new();
        object.insert("text_elements".to_owned(), json!([]));
        object.insert("text".to_owned(), json!("hello"));
        object.insert("type".to_owned(), json!("text"));
        let second = vec![Value::Object(object)];
        assert_eq!(
            InputDigest::from_input(&first, 64).expect("first"),
            InputDigest::from_input(&second, 64).expect("second")
        );
    }

    #[test]
    fn ownership_requires_one_and_only_one_exact_row() {
        let input = text_queue_input("hello").expect("input");
        let ownership = QueueOwnership::pending("thread", "random-client", &input, &limits())
            .expect("ownership");
        let exact = submission("row-1", "random-client", &input);
        assert!(matches!(
            reconcile_ownership(std::slice::from_ref(&exact), &ownership),
            OwnershipResolution::Exact(_)
        ));
        assert_eq!(
            reconcile_ownership(&[exact.clone(), exact], &ownership),
            OwnershipResolution::Violation
        );
    }

    #[test]
    fn ownership_rejects_same_client_id_with_changed_input() {
        let input = text_queue_input("expected").expect("input");
        let changed = text_queue_input("attacker controlled").expect("changed");
        let ownership = QueueOwnership::pending("thread", "random-client", &input, &limits())
            .expect("ownership");
        assert_eq!(
            reconcile_ownership(
                &[submission("row-1", "random-client", &changed)],
                &ownership
            ),
            OwnershipResolution::Violation
        );
    }

    #[test]
    fn persisted_id_collision_is_an_ownership_violation() {
        let input = text_queue_input("hello").expect("input");
        let ownership = QueueOwnership::pending("thread", "random-client", &input, &limits())
            .expect("ownership")
            .with_submission_id("row-1".to_owned());
        let expected = submission("row-1", "random-client", &input);
        let colliding = submission("row-1", "different-client", &input);
        assert_eq!(
            reconcile_ownership(&[expected, colliding], &ownership),
            OwnershipResolution::Violation
        );
    }

    #[test]
    fn arbitrary_user_input_variants_are_rejected() {
        let image = vec![json!({"type": "localImage", "path": "/private/secret"})];
        let error = QueueOwnership::pending("thread", "random-client", &image, &limits())
            .expect_err("non-text input must be rejected");
        assert_eq!(error.kind(), AppServerErrorKind::InvalidConfiguration);
    }

    #[test]
    fn parser_tolerates_unknown_response_fields() {
        let message = json!({
            "id": 7,
            "result": {"data": [], "futureField": {"nested": true}},
            "anotherFutureField": 42
        });
        match classify_rpc_message(&message, 7) {
            ParsedRpcMessage::Response(value) => {
                let parsed: WireQueueListResponse = parse_typed(value).expect("typed result");
                assert!(parsed.data.is_empty());
            }
            _ => panic!("expected response"),
        }
    }

    #[test]
    fn remote_error_text_is_not_retained_or_displayed() {
        let message = json!({
            "id": 9,
            "error": {
                "code": -32000,
                "message": "SECRET attacker instruction"
            }
        });
        let error = match classify_rpc_message(&message, 9) {
            ParsedRpcMessage::RemoteError(code) => AppServerError::remote(code),
            _ => panic!("expected remote error"),
        };
        assert_eq!(error.remote_code(), Some(-32000));
        assert!(!error.to_string().contains("SECRET"));
    }

    #[test]
    fn failed_probe_report_contains_only_sanitized_error_fields() {
        let error = AppServerError::remote(Some(-32_000));
        let report = failed_probe_report(&error);
        let encoded = serde_json::to_value(report).expect("serialize report");
        assert_eq!(encoded["status"], "failed");
        assert_eq!(encoded["strictQueueReady"], false);
        assert_eq!(encoded["errorKind"], "remote_rejected");
        assert_eq!(encoded["remoteCode"], -32_000);
        assert!(encoded.get("endpoint").is_none());
        assert!(encoded.get("probeThreadId").is_none());
    }

    #[test]
    fn non_loopback_websocket_is_rejected() {
        let endpoint =
            AppServerEndpoint::LoopbackWebSocket(Url::parse("ws://192.0.2.1:8080/").expect("URL"));
        assert!(validate_endpoint(&endpoint).is_err());
    }

    #[test]
    fn verified_build_is_scoped_to_platform() {
        let build = VerifiedBuild {
            exact_user_agent: "codex-test/1".to_owned(),
            exact_platform_family: "unix".to_owned(),
            exact_platform_os: "linux".to_owned(),
            evidence_id: "evidence-1".to_owned(),
        };
        assert!(build.matches(&ServerIdentity {
            user_agent: "codex-test/1".to_owned(),
            platform_family: "unix".to_owned(),
            platform_os: "linux".to_owned(),
        }));
        assert!(!build.matches(&ServerIdentity {
            user_agent: "codex-test/1".to_owned(),
            platform_family: "unix".to_owned(),
            platform_os: "macos".to_owned(),
        }));
    }

    #[test]
    fn bounded_line_reader_rejects_oversized_unterminated_line() {
        let data = vec![b'x'; 33];
        let mut reader = BufReader::new(data.as_slice());
        assert!(matches!(
            read_bounded_line(&mut reader, 32),
            Err(ReadLineError::TooLarge)
        ));
    }

    #[test]
    fn bounded_line_reader_rejects_partial_line_at_eof() {
        let mut reader = BufReader::new(&b"partial"[..]);
        assert!(matches!(
            read_bounded_line(&mut reader, 32),
            Err(ReadLineError::Io)
        ));
    }

    #[test]
    fn server_request_is_not_treated_as_notification() {
        let request = json!({"id": 4, "method": "item/commandExecution/requestApproval"});
        assert!(matches!(
            classify_rpc_message(&request, 4),
            ParsedRpcMessage::ServerRequest
        ));
    }
}
