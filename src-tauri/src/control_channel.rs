//! Bidirectional Control Channel (WebSocket).
//!
//! This is more than health: it carries telemetry, commands, logs, config and
//! diagnostics between Galahad Encoder and the control plane. The wire types and
//! command parsing are pure and unit tested; the async runtime wraps them.

use serde::{Deserialize, Serialize};

use crate::error::{EncoderError, Result};
use crate::stream_state::StreamState;

/// Telemetry the client pushes to the control plane (superset of the plan's
/// health payload, with identity + capability summary + state).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Telemetry {
    pub player_id: String,
    pub session_id: String,
    pub display_name: String,
    pub source: Option<String>,
    pub streaming: bool,
    pub state: StreamState,
    pub fps: u32,
    pub bitrate: u32,
    pub dropped_frames: u64,
    pub encoder: Option<String>,
    pub capture_backend: Option<String>,
    pub last_error: Option<String>,
}

/// Message envelope the client sends. `type` discriminates telemetry vs acks vs
/// log uploads so the control plane can route them.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        player_id: String,
        session_id: String,
        display_name: String,
    },
    Telemetry(Telemetry),
    CommandAck(CommandAck),
    LogUpload {
        session_id: String,
        lines: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandAck {
    pub action: String,
    pub ok: bool,
    pub detail: Option<String>,
}

/// Commands the operator / control plane can send to the client.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ServerCommand {
    StartStream,
    StopStream,
    RestartStream,
    SwitchQuality { preset: String },
    RequestLogUpload,
    PushConfig { config: serde_json::Value },
}

impl ServerCommand {
    pub fn action_name(&self) -> &'static str {
        match self {
            ServerCommand::StartStream => "start_stream",
            ServerCommand::StopStream => "stop_stream",
            ServerCommand::RestartStream => "restart_stream",
            ServerCommand::SwitchQuality { .. } => "switch_quality",
            ServerCommand::RequestLogUpload => "request_log_upload",
            ServerCommand::PushConfig { .. } => "push_config",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).map_err(|e| {
            EncoderError::Control(format!("unrecognized command '{raw}': {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_restart_stream() {
        let cmd = ServerCommand::parse(r#"{"action":"restart_stream"}"#).unwrap();
        assert_eq!(cmd, ServerCommand::RestartStream);
        assert_eq!(cmd.action_name(), "restart_stream");
    }

    #[test]
    fn parses_switch_quality_with_preset() {
        let cmd = ServerCommand::parse(r#"{"action":"switch_quality","preset":"720p30"}"#).unwrap();
        assert_eq!(
            cmd,
            ServerCommand::SwitchQuality {
                preset: "720p30".into()
            }
        );
    }

    #[test]
    fn parses_request_log_upload() {
        let cmd = ServerCommand::parse(r#"{"action":"request_log_upload"}"#).unwrap();
        assert_eq!(cmd, ServerCommand::RequestLogUpload);
    }

    #[test]
    fn parses_push_config_payload() {
        let cmd =
            ServerCommand::parse(r#"{"action":"push_config","config":{"presetId":"1080p60"}}"#)
                .unwrap();
        match cmd {
            ServerCommand::PushConfig { config } => {
                assert_eq!(config["presetId"], "1080p60");
            }
            _ => panic!("expected push_config"),
        }
    }

    #[test]
    fn rejects_unknown_action() {
        assert!(ServerCommand::parse(r#"{"action":"explode"}"#).is_err());
    }

    #[test]
    fn missing_preset_is_error() {
        assert!(ServerCommand::parse(r#"{"action":"switch_quality"}"#).is_err());
    }

    #[test]
    fn ack_serializes_camel_case() {
        let ack = CommandAck {
            action: "switch_quality".into(),
            ok: true,
            detail: None,
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert!(json.contains("\"action\":\"switch_quality\""));
        assert!(json.contains("\"ok\":true"));
    }
}
