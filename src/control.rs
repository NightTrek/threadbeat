use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};
use uuid::Uuid;

pub const CONTROL_PROTOCOL_VERSION: u16 = 1;
pub const MAX_CONTROL_FRAME_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OriginContext {
    pub thread_id: String,
    pub call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequest {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub origin: OriginContext,
    pub tool: String,
    pub arguments: Value,
}

impl ControlRequest {
    pub fn new(origin: OriginContext, tool: impl Into<String>, arguments: Value) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            origin,
            tool: tool.into(),
            arguments,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResponse {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub result: Option<Value>,
    pub error: Option<ControlError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct ControlError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Option<Value>,
}

impl ControlResponse {
    pub fn success(request_id: Uuid, result: Value) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(request_id: Uuid, error: ControlError) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id,
            result: None,
            error: Some(error),
        }
    }
}

pub async fn call_daemon(
    socket_path: &Path,
    request: &ControlRequest,
    request_timeout: Duration,
) -> Result<ControlResponse> {
    let mut stream = timeout(request_timeout, UnixStream::connect(socket_path))
        .await
        .context("timed out connecting to Threadbeat daemon")?
        .with_context(|| format!("failed to connect to {}", socket_path.display()))?;

    let mut encoded = serde_json::to_vec(request).context("failed to encode daemon request")?;
    if encoded.len() > MAX_CONTROL_FRAME_BYTES {
        bail!("control request exceeds maximum frame size");
    }
    encoded.push(b'\n');
    timeout(request_timeout, stream.write_all(&encoded))
        .await
        .context("timed out writing daemon request")??;
    stream
        .shutdown()
        .await
        .context("failed to finish daemon request")?;

    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    timeout(
        request_timeout,
        (&mut reader)
            .take((MAX_CONTROL_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut response),
    )
    .await
    .context("timed out reading daemon response")??;

    if response.len() > MAX_CONTROL_FRAME_BYTES || !response.ends_with(b"\n") {
        bail!("invalid or oversized daemon response");
    }
    response.pop();
    let response: ControlResponse =
        serde_json::from_slice(&response).context("failed to decode daemon response")?;
    if response.protocol_version != CONTROL_PROTOCOL_VERSION {
        bail!("daemon control protocol mismatch");
    }
    if response.request_id != request.request_id {
        bail!("daemon response request ID mismatch");
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let request = ControlRequest::new(
            OriginContext {
                thread_id: "thread-1".to_owned(),
                call_id: Some("call-1".to_owned()),
            },
            "automation_list",
            serde_json::json!({"limit": 10}),
        );
        let encoded = serde_json::to_vec(&request).expect("serialize request");
        let decoded: ControlRequest = serde_json::from_slice(&encoded).expect("decode request");
        assert_eq!(decoded.tool, "automation_list");
        assert_eq!(decoded.origin.thread_id, "thread-1");
    }
}
