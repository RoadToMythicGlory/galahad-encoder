//! Async runtime for the Control Channel WebSocket.
//!
//! Connects to the control plane, announces identity, streams telemetry, and
//! receives operator commands. Like the media pipeline, the connection itself
//! auto-reconnects with capped backoff; a dropped control link never tears down
//! an active stream.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::control_channel::{ClientMessage, ServerCommand};

/// Handle used by the rest of the app to talk to the control plane.
#[derive(Clone)]
pub struct ControlHandle {
    outbound: UnboundedSender<ClientMessage>,
}

impl ControlHandle {
    /// Queue a message for delivery. Drops silently if the channel is closed
    /// (the runtime logs the disconnect separately).
    pub fn send(&self, message: ClientMessage) {
        let _ = self.outbound.send(message);
    }
}

/// Callback invoked for each parsed operator command.
pub type CommandSink = Arc<dyn Fn(ServerCommand) + Send + Sync>;

/// Spawn the control-channel runtime. Returns a handle for sending messages.
///
/// `url` is the control-plane WebSocket endpoint. `on_command` is called for each
/// inbound operator command (already parsed). The future runs until the process
/// exits; it reconnects forever with capped backoff.
pub fn spawn(url: String, hello: ClientMessage, on_command: CommandSink) -> ControlHandle {
    let (tx, rx) = unbounded_channel::<ClientMessage>();
    tauri::async_runtime::spawn(run(url, hello, rx, on_command));
    ControlHandle { outbound: tx }
}

async fn run(
    url: String,
    hello: ClientMessage,
    mut rx: UnboundedReceiver<ClientMessage>,
    on_command: CommandSink,
) {
    let mut attempt: u32 = 0;
    loop {
        match connect_async(url.as_str()).await {
            Ok((mut ws, _resp)) => {
                attempt = 0;
                log::info!("control channel connected to {url}");

                // Announce identity immediately on (re)connect.
                if let Ok(text) = serde_json::to_string(&hello) {
                    if ws.send(Message::Text(text)).await.is_err() {
                        log::warn!("control channel: failed to send hello");
                        continue;
                    }
                }

                loop {
                    tokio::select! {
                        outbound = rx.recv() => {
                            match outbound {
                                Some(msg) => {
                                    match serde_json::to_string(&msg) {
                                        Ok(text) => {
                                            if ws.send(Message::Text(text)).await.is_err() {
                                                log::warn!("control channel: send failed, reconnecting");
                                                break;
                                            }
                                        }
                                        Err(e) => log::warn!("control channel: serialize failed: {e}"),
                                    }
                                }
                                None => {
                                    // Outbound side dropped -> app shutting down.
                                    let _ = ws.close(None).await;
                                    return;
                                }
                            }
                        }
                        inbound = ws.next() => {
                            match inbound {
                                Some(Ok(Message::Text(text))) => {
                                    match ServerCommand::parse(&text) {
                                        Ok(cmd) => {
                                            log::info!("control channel: command {}", cmd.action_name());
                                            on_command(cmd);
                                        }
                                        Err(e) => log::warn!("control channel: {e}"),
                                    }
                                }
                                Some(Ok(Message::Ping(p))) => {
                                    let _ = ws.send(Message::Pong(p)).await;
                                }
                                Some(Ok(Message::Close(_))) | None => {
                                    log::warn!("control channel closed by server, reconnecting");
                                    break;
                                }
                                Some(Err(e)) => {
                                    log::warn!("control channel error: {e}, reconnecting");
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("control channel connect failed: {e}");
            }
        }

        let delay = crate::stream_state::backoff_delay(attempt);
        attempt = attempt.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
}
