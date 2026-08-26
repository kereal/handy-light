//! WebSocket-proxy transcription backend.
//!
//! Forwards recorded audio (PCM Float32 LE / 16 kHz / mono) to a user-configured
//! WebSocket server and waits for the server to return the final text. Skips
//! Handy's local ASR stack entirely — no model is loaded, no post-processing is
//! applied to the result.
//!
//! Wire protocol (ligsai-compatible):
//! - Client → server binary frame: `&[f32]` samples little-endian (raw bytes).
//! - Client → server text frame: `{"cmd":"flush"}` to finalize the current
//!   segment. The server may emit one or more `{"text": "..."}` result messages
//!   before `{"status":"flushed"}`.
//!
//! This module deliberately exposes a tiny, blocking-ish API used only from the
//! `TranscribeAction::stop` task: the recording has already stopped and we own
//! the samples, so there is no need for a long-lived manager or shared state.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
/// never trips a protocol violation.
const MAX_BINARY_FRAME_BYTES: usize = 256 * 1024;
/// Time we are willing to wait for `connect_async` to complete.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time we are willing to wait for the server's final result after `flush`.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(30);

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// One end-of-pipeline result message from the server. Fields we don't consume
/// (`confidence`, `latency_ms`, …) are tolerated via `#[serde(default)]` and
/// ignored on the Rust side — they're useful to the UI only if the server
/// exposes them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ServerMessage {
    Result { text: String },
    Status { status: String },
    Error { error: String },
}

/// One full transcription session against a single WebSocket connection.
///
/// Usage:
/// 1. [`WsTranscriptionSession::connect`] — open the connection.
/// 2. Optionally call [`Self::send_samples`] zero or more times while
///    recording. Each call may emit several binary frames (samples are
///    chunked to fit the 256 KiB cap).
/// 3. Call [`Self::finalize`] — sends `flush`, drains every result until the
///    server emits `{"status":"flushed"}`, returns the concatenated text.
/// 4. The session is dropped; the socket closes.
pub struct WsTranscriptionSession {
    ws: Ws,
    /// Concatenated server-provided result text from previous `finalize`
    /// calls within the same connection (kept across flushes so multi-segment
    /// recordings land as one pasted string). Empty on a fresh session.
    accumulated: String,
}

impl WsTranscriptionSession {
    /// Dial `url` (with optional `Authorization: Bearer <token>` from the URL's
    /// `Authorization` header or a separate `bearer_token` argument) and
    /// return a session ready to stream samples.
    pub async fn connect(url: &str, bearer_token: Option<&str>) -> Result<Self> {
        if url.trim().is_empty() {
            bail!("WebSocket proxy URL is empty");
        }

        let mut request = url
            .into_client_request()
            .with_context(|| format!("invalid WebSocket URL: {url}"))?;
        if let Some(token) = bearer_token.filter(|t| !t.is_empty()) {
            let header_value = HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid bearer token")?;
            request.headers_mut().insert(
                tokio_tungstenite::tungstenite::http::HeaderName::from_static("authorization"),
                header_value,
            );
        }
        let (ws, _response) = timeout(CONNECT_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| {
                anyhow!("timed out connecting to WebSocket proxy after {CONNECT_TIMEOUT:?}")
            })?
            .with_context(|| format!("failed to connect to WebSocket proxy at {url}"))?;

        Ok(Self {
            ws,
            accumulated: String::new(),
        })
    }

    /// Forward one batch of PCM Float32 mono samples to the server. The buffer
    /// is split into chunks of at most [`MAX_BINARY_FRAME_BYTES`].
    pub async fn send_samples(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        // 4 bytes per sample. We pad the cap down slightly so a chunk never
        // lands exactly on the limit after an alignment rounding.
        let samples_per_chunk = (MAX_BINARY_FRAME_BYTES / 4).saturating_sub(16).max(1);
        for chunk in samples.chunks(samples_per_chunk) {
            // SAFETY: `f32` is `Copy` and `Pod` on every platform rustc
            // supports; we re-interpret the slice as bytes for the binary
            // frame. Endianness is fixed to little-endian on the wire because
            // the recorder emits native-endian f32 — modern Handy's only
            // build targets (x86_64, aarch64) are all little-endian, and the
            // server spec also mandates LE.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    chunk.as_ptr() as *const u8,
                    std::mem::size_of_val(chunk),
                )
            };
            // Copy into an owned buffer so the borrowed `bytes` does not
            // outlive the local `chunk` once `to_vec()`'s allocation could
            // in principle be elided. `to_owned()` is the safest spelling.
            let frame = bytes.to_owned();
            self.ws
                .send(Message::Binary(frame))
                .await
                .context("failed to send audio frame to WebSocket proxy")?;
        }
        Ok(())
    }

    /// Tell the server to flush, then collect every `text` result until the
    /// `{"status":"flushed"}` marker arrives. Returns the concatenated text
    /// from this finalize call. Multiple calls within the same session keep
    /// accumulating into the connection-wide buffer.
    pub async fn finalize(&mut self) -> Result<String> {
        self.ws
            .send(Message::Text(r#"{"cmd":"flush"}"#.to_owned()))
            .await
            .context("failed to send flush to WebSocket proxy")?;

        let mut this_segment_text = String::new();
        let deadline = tokio::time::Instant::now() + FINALIZE_TIMEOUT;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                bail!(
                    "timed out waiting for flushed marker after {:?}",
                    FINALIZE_TIMEOUT
                );
            }
            let remaining = deadline - now;

            let next = timeout(remaining, self.ws.next())
                .await
                .map_err(|_| {
                    anyhow!("timed out waiting for flushed marker after {FINALIZE_TIMEOUT:?}")
                })?
                .ok_or_else(|| anyhow!("WebSocket proxy closed the connection before flushing"))?
                .context("WebSocket proxy transport error while finalizing")?;

            match next {
                Message::Text(text) => {
                    let msg: ServerMessage = serde_json::from_str(&text)
                        .with_context(|| format!("malformed JSON from WebSocket proxy: {text}"))?;
                    match msg {
                        ServerMessage::Result { text, .. } => {
                            if !this_segment_text.is_empty() && !text.is_empty() {
                                this_segment_text.push(' ');
                            }
                            this_segment_text.push_str(&text);
                        }
                        ServerMessage::Status { status } if status == "flushed" => {
                            // Done with this segment. Append to the
                            // connection-wide buffer; the caller may invoke
                            // finalize again later for another segment.
                            if !this_segment_text.is_empty() {
                                if !self.accumulated.is_empty() {
                                    self.accumulated.push(' ');
                                }
                                self.accumulated.push_str(&this_segment_text);
                            }
                            return Ok(self.accumulated.clone());
                        }
                        ServerMessage::Status { status } => {
                            log::debug!("WebSocket proxy status: {status}");
                        }
                        ServerMessage::Error { error } => {
                            bail!("WebSocket proxy reported error: {error}");
                        }
                    }
                }
                Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => {
                    // Audio frames or keep-alive; ignore. A binary frame here
                    // would be protocol-shaped wrong but the server spec
                    // allows it to be silently dropped.
                }
                Message::Close(frame) => {
                    bail!("WebSocket proxy closed before flushed: {:?}", frame);
                }
                Message::Frame(_) => {
                    // Raw continuation frames aren't produced by tokio-tungstenite
                    // at this layer; ignore defensively.
                }
            }
        }
    }
}

/// One-shot helper used by [`TranscribeAction::stop`]: connect, send the
/// captured samples as a single batch, flush, return the server's text.
pub async fn transcribe_over_websocket(
    url: &str,
    bearer_token: Option<&str>,
    samples: &[f32],
) -> Result<String> {
    let mut session = WsTranscriptionSession::connect(url, bearer_token).await?;
    session.send_samples(samples).await?;
    session.finalize().await
}
