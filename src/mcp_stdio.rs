//! Bounded stdio transport with a recoverable MCP admission phase.
//!
//! rmcp treats a non-initialize first request without the required modern request metadata as a
//! service-initialization error. For a long-lived local stdio server, one malformed request must
//! not consume the process that the client is about to reuse. This transport rejects malformed
//! pre-admission messages itself, then hands the first valid legacy initialize or modern request
//! to rmcp unchanged.

use rmcp::{
    model::{ClientJsonRpcMessage, ErrorData, RequestId, ServerJsonRpcMessage},
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
    RoleServer,
};
use serde_json::Value;
use std::{future::Future, io, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::Mutex,
};

/// Generous enough for the bounded inline write surface while preventing an unterminated stdin
/// line from growing process memory without limit.
pub(crate) const MAX_MCP_STDIN_LINE_BYTES: usize = 1024 * 1024;

const PROTOCOL_VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const UTF8_BOM: &[u8; 3] = b"\xEF\xBB\xBF";

enum BoundedLine {
    Line,
    Oversized,
    Eof,
}

/// The production transport used by both MCP binaries.
pub fn bounded_stdio() -> impl Transport<RoleServer, Error = io::Error> {
    BoundedStdioTransport::new(tokio::io::stdin(), tokio::io::stdout())
}

pub(crate) struct BoundedStdioTransport<R, W> {
    read: BufReader<R>,
    line: Vec<u8>,
    discarding_oversized: bool,
    write: Arc<Mutex<Option<W>>>,
    admitted: bool,
}

impl<R, W> BoundedStdioTransport<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(crate) fn new(read: R, write: W) -> Self {
        Self {
            read: BufReader::new(read),
            line: Vec::new(),
            discarding_oversized: false,
            write: Arc::new(Mutex::new(Some(write))),
            admitted: false,
        }
    }

    async fn read_bounded_line(&mut self) -> io::Result<BoundedLine> {
        loop {
            let available = self.read.fill_buf().await?;
            if available.is_empty() {
                if self.discarding_oversized {
                    self.discarding_oversized = false;
                    return Ok(BoundedLine::Oversized);
                }
                self.line.clear();
                return Ok(BoundedLine::Eof);
            }

            if self.discarding_oversized {
                if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                    self.read.consume(newline + 1);
                    self.discarding_oversized = false;
                    return Ok(BoundedLine::Oversized);
                }
                let consumed = available.len();
                self.read.consume(consumed);
                continue;
            }

            if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                if self.line.len().saturating_add(newline) > MAX_MCP_STDIN_LINE_BYTES {
                    self.line.clear();
                    self.read.consume(newline + 1);
                    return Ok(BoundedLine::Oversized);
                }
                self.line.extend_from_slice(&available[..newline]);
                self.read.consume(newline + 1);
                return Ok(BoundedLine::Line);
            }

            if self.line.len().saturating_add(available.len()) > MAX_MCP_STDIN_LINE_BYTES {
                self.line.clear();
                self.discarding_oversized = true;
                let consumed = available.len();
                self.read.consume(consumed);
                continue;
            }
            self.line.extend_from_slice(available);
            let consumed = available.len();
            self.read.consume(consumed);
        }
    }

    async fn write_error(&mut self, error: ErrorData, id: Option<RequestId>) -> io::Result<()> {
        write_message(&self.write, ServerJsonRpcMessage::error(error, id)).await
    }

    async fn reject_initial_message(
        &mut self,
        message: String,
        id: Option<RequestId>,
        data: Option<Value>,
    ) -> io::Result<()> {
        self.write_error(ErrorData::invalid_request(message, data), id)
            .await
    }

    async fn parse_line(&mut self) -> io::Result<Option<(ClientJsonRpcMessage, Value)>> {
        let line = self
            .line
            .strip_suffix(b"\r")
            .unwrap_or(&self.line)
            .strip_prefix(UTF8_BOM.as_slice())
            .unwrap_or_else(|| self.line.strip_suffix(b"\r").unwrap_or(&self.line));
        if line.is_empty() {
            return Ok(None);
        }

        let raw = match serde_json::from_slice::<Value>(line) {
            Ok(raw) => raw,
            Err(error) => {
                self.write_error(
                    ErrorData::parse_error("Parse error", Some(Value::String(error.to_string()))),
                    None,
                )
                .await?;
                return Ok(None);
            }
        };
        match serde_json::from_value::<ClientJsonRpcMessage>(raw.clone()) {
            Ok(message) => Ok(Some((message, raw))),
            Err(error) => {
                self.write_error(
                    ErrorData::invalid_request(
                        "Invalid request",
                        Some(Value::String(error.to_string())),
                    ),
                    request_id(&raw),
                )
                .await?;
                Ok(None)
            }
        }
    }

    async fn admit_initial(
        &mut self,
        message: ClientJsonRpcMessage,
        raw: &Value,
    ) -> io::Result<Option<ClientJsonRpcMessage>> {
        let ClientJsonRpcMessage::Request(request) = &message else {
            self.reject_initial_message(
                "Expected an initialize, ping, or modern request".to_string(),
                request_id(raw),
                None,
            )
            .await?;
            return Ok(None);
        };
        let method = raw
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if method == "ping" {
            return Ok(Some(message));
        }
        if method == "initialize" {
            self.admitted = true;
            return Ok(Some(message));
        }

        let meta = raw.pointer("/params/_meta");
        let mut missing = Vec::new();
        if meta
            .and_then(|value| value.get(PROTOCOL_VERSION_KEY))
            .and_then(Value::as_str)
            .is_none()
        {
            missing.push(PROTOCOL_VERSION_KEY);
        }
        if meta
            .and_then(|value| value.get(CLIENT_CAPABILITIES_KEY))
            .is_none()
        {
            missing.push(CLIENT_CAPABILITIES_KEY);
        }
        if !missing.is_empty() {
            self.reject_initial_message(
                "Missing required modern MCP request metadata".to_string(),
                Some(request.id.clone()),
                Some(serde_json::json!({ "missing": missing })),
            )
            .await?;
            return Ok(None);
        }

        self.admitted = true;
        Ok(Some(message))
    }
}

fn request_id(raw: &Value) -> Option<RequestId> {
    raw.get("id")
        .cloned()
        .and_then(|id| serde_json::from_value(id).ok())
}

async fn write_message<W>(
    writer: &Arc<Mutex<Option<W>>>,
    message: ServerJsonRpcMessage,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let bytes = serde_json::to_vec(&message).map_err(io::Error::other)?;
    let mut locked = writer.lock().await;
    let output = locked
        .as_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "transport is closed"))?;
    output.write_all(&bytes).await?;
    output.write_all(b"\n").await?;
    output.flush().await
}

impl<R, W> Transport<RoleServer> for BoundedStdioTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let writer = self.write.clone();
        async move { write_message(&writer, item).await }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            match self.read_bounded_line().await {
                Ok(BoundedLine::Eof) => return None,
                Ok(BoundedLine::Oversized) => {
                    if self
                        .reject_initial_message(
                            format!(
                                "MCP stdio line exceeds the {MAX_MCP_STDIN_LINE_BYTES}-byte limit"
                            ),
                            None,
                            None,
                        )
                        .await
                        .is_err()
                    {
                        return None;
                    }
                    continue;
                }
                Ok(BoundedLine::Line) => {}
                Err(_) => return None,
            }

            let parsed = self.parse_line().await;
            self.line.clear();
            let (message, raw) = match parsed {
                Ok(Some(parsed)) => parsed,
                Ok(None) => continue,
                Err(_) => return None,
            };
            if self.admitted {
                return Some(message);
            }
            match self.admit_initial(message, &raw).await {
                Ok(Some(message)) => return Some(message),
                Ok(None) => continue,
                Err(_) => return None,
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let mut locked = self.write.lock().await;
        if let Some(mut output) = locked.take() {
            output.shutdown().await?;
        }
        Ok(())
    }
}
