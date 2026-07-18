use std::collections::VecDeque;
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::http::Uri;
use tungstenite::{ClientRequestBuilder, Message, WebSocket};

use super::{BridgeError, BridgeResult, PPSSPP_SUBPROTOCOL};

pub trait WsTransport {
    /// Send `{"event": event, ...params}` and block for the reply carrying the same event name.
    /// A `{"event":"error", ...}` reply becomes `Err`. Any other event observed while waiting (a
    /// spontaneous log/breakpoint notification) is queued rather than dropped, so `drain_events`
    /// can surface it later.
    fn call(&mut self, event: &str, params: Value) -> Result<Value, BridgeError>;
    /// Send `{"event": event, ...params}` for a command whose acknowledgement is a *differently
    /// named* spontaneous event — PPSSPP's `cpu.stepInto`/`stepOver`/`stepOut`/`runUntil`/`nextHLE`
    /// have no reply of their own name; they ack via a `cpu.stepping` event once the step completes
    /// (`SteppingSubscriber.cpp`). Blocks until an event named `expect_event` arrives; anything else
    /// observed meanwhile is queued exactly like `call`.
    fn call_and_wait_for(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
    ) -> Result<Value, BridgeError>;
    /// Like `call_and_wait_for`, but bounds this exchange by the operation's remaining wall-clock
    /// budget. Implementations that can change their transport timeout must override this method.
    fn call_and_wait_for_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
        timeout: Duration,
    ) -> Result<Value, BridgeError>;
    /// Like `call`, but with a per-call read budget overriding the transport's default read timeout
    /// for just this exchange — for a command whose reply is legitimately slow (`save_state`/
    /// `load_state`, whose fork handler waits up to 15s). Restores the default afterward so ordinary
    /// reads still fail fast.
    fn call_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError>;
    /// Like `call`, but tags the request with a `ticket` and only accepts a reply whose `event`
    /// name AND echoed `ticket` both match. PPSSPP echoes any `ticket` a request carried back on its
    /// reply (`WebSocketUtils`/`InputSubscriber.cpp`), so this is how a timed `input.buttons.press`
    /// is correlated: a stale ack from an *earlier* press that only released once the CPU resumed
    /// (its own ack was stranded when a breakpoint halted the core mid-press) carries the earlier
    /// ticket and is queued/ignored here rather than misattributed to this call. Off-ticket replies
    /// seen while waiting are queued as spontaneous events, exactly like `call`.
    fn call_ticketed(
        &mut self,
        event: &str,
        params: Value,
        ticket: &str,
    ) -> Result<Value, BridgeError>;
    /// Return, without blocking, any spontaneous events queued since the last drain (from `call`'s
    /// queueing above, plus anything read off the socket right now).
    fn drain_events(&mut self) -> Vec<Value>;
}

/// Real transport: a `tungstenite` WebSocket connected to PPSSPP's debugger endpoint.
pub struct TungsteniteWs {
    socket: WebSocket<TcpStream>,
    pending_events: VecDeque<Value>,
    /// The default per-read socket timeout set at connect, restored after any `call_with_timeout`
    /// widens it for a single slow exchange.
    default_timeout: Duration,
}

impl TungsteniteWs {
    /// Connect to `ws://127.0.0.1:<port>/debugger` with the `debugger.ppsspp.org` subprotocol.
    /// `timeout` bounds the initial TCP connect/handshake and every subsequent blocking read/write.
    pub fn connect(port: u16, timeout: Duration) -> BridgeResult<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| {
            BridgeError::Emulator(format!(
                "connect PPSSPP debugger websocket at 127.0.0.1:{port}: {err}"
            ))
        })?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(timeout)).ok();
        stream.set_write_timeout(Some(timeout)).ok();

        let uri: Uri = format!("ws://127.0.0.1:{port}/debugger")
            .parse()
            .map_err(|err| BridgeError::Emulator(format!("invalid PPSSPP debugger URL: {err}")))?;
        let request = ClientRequestBuilder::new(uri).with_sub_protocol(PPSSPP_SUBPROTOCOL);
        let (socket, _response) = tungstenite::client(request, stream).map_err(|err| {
            BridgeError::Emulator(format!(
                "websocket handshake with PPSSPP debugger failed: {err}"
            ))
        })?;
        Ok(Self {
            socket,
            pending_events: VecDeque::new(),
            default_timeout: timeout,
        })
    }
}

impl TungsteniteWs {
    /// Send `{"event": event, ...params}` without waiting for any reply.
    fn send_request(&mut self, event: &str, params: Value) -> BridgeResult<()> {
        let mut obj = match params {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                return Err(BridgeError::BadParams(format!(
                    "params for {event} must be a JSON object, got {other}"
                )))
            }
        };
        obj.insert("event".into(), json!(event));
        let text = serde_json::to_string(&Value::Object(obj))?;
        self.socket.send(Message::from(text))?;
        Ok(())
    }

    /// Block until an event named `expect` arrives. A `{"event":"error", ...}` reply becomes
    /// `Err`. Any other event observed while waiting (a spontaneous log/breakpoint/stepping
    /// notification) is queued rather than dropped, so a later `drain_events` can surface it.
    fn read_until(&mut self, expect: &str) -> BridgeResult<Value> {
        loop {
            match self.socket.read()? {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(text.as_str())?;
                    let seen = value.get("event").and_then(Value::as_str).unwrap_or("");
                    if seen == "error" {
                        let message = value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("PPSSPP debugger returned an error")
                            .to_string();
                        return Err(BridgeError::Emulator(message));
                    }
                    if seen == expect {
                        return Ok(value);
                    }
                    // A spontaneous event (log line, breakpoint/stepping hit) arrived ahead of our
                    // reply — queue it instead of dropping it, so a later poll_events can surface it.
                    self.pending_events.push_back(value);
                }
                Message::Close(_) => {
                    return Err(BridgeError::Emulator(
                        "PPSSPP debugger closed the websocket".into(),
                    ));
                }
                // Ping/Pong/Frame carry no event payload; tungstenite answers pings automatically.
                _ => {}
            }
        }
    }

    /// Like `read_until`, but the reply must also echo back `ticket` — a reply named `expect` whose
    /// `ticket` differs (a stale ack from an earlier correlated request) is queued as a spontaneous
    /// event, not returned, so it can never satisfy the wrong call.
    fn read_until_ticketed(&mut self, expect: &str, ticket: &str) -> BridgeResult<Value> {
        loop {
            match self.socket.read()? {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(text.as_str())?;
                    let seen = value.get("event").and_then(Value::as_str).unwrap_or("");
                    if seen == "error" {
                        let message = value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("PPSSPP debugger returned an error")
                            .to_string();
                        return Err(BridgeError::Emulator(message));
                    }
                    let ticket_matches =
                        value.get("ticket").and_then(Value::as_str) == Some(ticket);
                    if seen == expect && ticket_matches {
                        return Ok(value);
                    }
                    self.pending_events.push_back(value);
                }
                Message::Close(_) => {
                    return Err(BridgeError::Emulator(
                        "PPSSPP debugger closed the websocket".into(),
                    ));
                }
                _ => {}
            }
        }
    }

    fn with_socket_timeout<T>(
        &mut self,
        timeout: Duration,
        operation: impl FnOnce(&mut Self) -> BridgeResult<T>,
    ) -> BridgeResult<T> {
        self.socket
            .get_ref()
            .set_read_timeout(Some(timeout))
            .map_err(BridgeError::from)?;
        if let Err(error) = self.socket.get_ref().set_write_timeout(Some(timeout)) {
            let rollback = self
                .socket
                .get_ref()
                .set_read_timeout(Some(self.default_timeout));
            return match rollback {
                Ok(()) => Err(BridgeError::from(error)),
                Err(rollback_error) => Err(BridgeError::Emulator(format!(
                    "failed to set the WebSocket write timeout: {error}; additionally failed to restore the read timeout: {rollback_error}"
                ))),
            };
        }

        let outcome = operation(self);
        let read_restore = self
            .socket
            .get_ref()
            .set_read_timeout(Some(self.default_timeout));
        let write_restore = self
            .socket
            .get_ref()
            .set_write_timeout(Some(self.default_timeout));
        let restore = match (read_restore, write_restore) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(BridgeError::from(error)),
            (Err(read_error), Err(write_error)) => Err(BridgeError::Emulator(format!(
                "failed to restore WebSocket read timeout: {read_error}; failed to restore write timeout: {write_error}"
            ))),
        };
        crate::live::temporal::finish_with_cleanup(outcome, restore, |primary, cleanup| {
            match primary {
                Some(primary) => BridgeError::Emulator(format!(
                    "{primary}; additionally failed to restore the WebSocket timeout: {cleanup}"
                )),
                None => BridgeError::Emulator(format!(
                    "backend call completed but failed to restore the WebSocket timeout: {cleanup}"
                )),
            }
        })
    }
}

impl WsTransport for TungsteniteWs {
    fn call(&mut self, event: &str, params: Value) -> BridgeResult<Value> {
        self.send_request(event, params)?;
        self.read_until(event)
    }

    fn call_and_wait_for(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
    ) -> BridgeResult<Value> {
        self.send_request(event, params)?;
        self.read_until(expect_event)
    }

    fn call_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        timeout: Duration,
    ) -> BridgeResult<Value> {
        // Bound both the write and read. Applying the timeout only after send_request would let a
        // blocked socket write outlive the caller's operation deadline.
        self.with_socket_timeout(timeout, |transport| {
            transport
                .send_request(event, params)
                .and_then(|()| transport.read_until(event))
        })
    }

    fn call_and_wait_for_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
        timeout: Duration,
    ) -> BridgeResult<Value> {
        self.with_socket_timeout(timeout, |transport| {
            transport
                .send_request(event, params)
                .and_then(|()| transport.read_until(expect_event))
        })
    }

    fn call_ticketed(&mut self, event: &str, params: Value, ticket: &str) -> BridgeResult<Value> {
        let mut obj = match params {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                return Err(BridgeError::BadParams(format!(
                    "params for {event} must be a JSON object, got {other}"
                )))
            }
        };
        obj.insert("ticket".into(), json!(ticket));
        self.send_request(event, Value::Object(obj))?;
        self.read_until_ticketed(event, ticket)
    }

    fn drain_events(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending_events.drain(..).collect();
        if self.socket.get_mut().set_nonblocking(true).is_err() {
            return out;
        }
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(value) = serde_json::from_str::<Value>(text.as_str()) {
                        out.push(value);
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let _ = self.socket.get_mut().set_nonblocking(false);
        out
    }
}
