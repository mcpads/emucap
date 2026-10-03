use std::collections::VecDeque;
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::http::Uri;
use tungstenite::{ClientRequestBuilder, Message, WebSocket};

use super::{BridgeError, BridgeResult, PPSSPP_SUBPROTOCOL};

pub trait WsTransport {
    /// Whether this transport can no longer preserve request/reply ordering for the current
    /// backend generation. Test doubles default to recoverable; the real socket sets this after
    /// EOF, close, or a non-timeout I/O failure.
    fn is_terminal(&self) -> bool {
        false
    }

    /// A negotiated deferred native frame operation. Implementations without that
    /// protocol reject before issuing a guest effect.
    fn frame_step_cancellable(
        &mut self,
        _count: u64,
        _timeout: Duration,
        _cancellation: &crate::live::link::RequestCancellation,
    ) -> BridgeResult<Value> {
        Err(BridgeError::Unsupported("owned frame cancellation".into()))
    }

    /// Send `{"event": event, ...params}` and block for the reply carrying the same event name and
    /// request identity. A correlated `{"event":"error", ...}` reply becomes `Err`. Any other
    /// event observed while waiting (a spontaneous notification or a late reply to an earlier
    /// request) is queued rather than dropped, so `drain_events` can surface it later.
    fn call(&mut self, event: &str, params: Value) -> Result<Value, BridgeError>;
    /// Send `{"event": event, ...params}` for a command whose acknowledgement is a *differently
    /// named* spontaneous event — PPSSPP's `cpu.stepInto`/`stepOver`/`stepOut`/`runUntil`/`nextHLE`
    /// have no reply of their own name; they ack via a `cpu.stepping` event once the step completes
    /// (`SteppingSubscriber.cpp`). Blocks until an event named `expect_event` arrives; anything else
    /// observed meanwhile is queued exactly like `call`. The PPSSPP fork echoes the request ticket
    /// on these asynchronous acknowledgements so a late completion cannot satisfy a later command.
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
    /// Like `call`, but uses a caller-supplied `ticket`. This is needed by timed
    /// `input.buttons.press`, whose recovery path refers to the exact press identity. Ordinary
    /// calls mint their ticket inside the transport. Off-ticket replies seen while waiting are
    /// queued as spontaneous events, exactly like `call`.
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
    terminal: bool,
    /// The default per-read socket timeout set at connect, restored after any `call_with_timeout`
    /// widens it for a single slow exchange.
    default_timeout: Duration,
    /// Monotonic request identity. PPSSPP's direct responses and errors echo arbitrary `ticket`
    /// values; the emucap fork does the same for asynchronous CPU stepping/resume acknowledgements.
    next_ticket: u64,
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
            terminal: false,
            default_timeout: timeout,
            next_ticket: 1,
        })
    }
}

impl TungsteniteWs {
    fn finish_transport<T>(&mut self, outcome: BridgeResult<T>) -> BridgeResult<T> {
        if outcome.as_ref().is_err_and(transport_error_is_terminal) {
            self.terminal = true;
        }
        outcome
    }

    fn mint_ticket(&mut self) -> String {
        let ticket = format!("emucap-ws-{}", self.next_ticket);
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        ticket
    }

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

    /// Block until a reply carries both the expected event name and this request's identity. A late
    /// reply or error for another ticket is queued, not attributed to the command now in flight.
    fn read_until_ticketed(&mut self, expect: &str, ticket: &str) -> BridgeResult<Value> {
        loop {
            match self.socket.read()? {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(text.as_str())?;
                    let seen = value.get("event").and_then(Value::as_str).unwrap_or("");
                    let ticket_matches =
                        value.get("ticket").and_then(Value::as_str) == Some(ticket);
                    if seen == "error" && ticket_matches {
                        let message = value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("PPSSPP debugger returned an error")
                            .to_string();
                        return Err(BridgeError::Emulator(message));
                    }
                    if seen == expect && ticket_matches {
                        return Ok(value);
                    }
                    self.pending_events.push_back(value);
                }
                Message::Close(_) => {
                    self.terminal = true;
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
                Err(rollback_error) => {
                    self.terminal = true;
                    Err(BridgeError::Emulator(format!(
                        "failed to set the WebSocket write timeout: {error}; additionally failed to restore the read timeout: {rollback_error}"
                    )))
                }
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
        let restore_failed = restore.is_err();
        let combined =
            crate::live::temporal::finish_with_cleanup(outcome, restore, |primary, cleanup| {
                match primary {
                    Some(primary) => BridgeError::Emulator(format!(
                    "{primary}; additionally failed to restore the WebSocket timeout: {cleanup}"
                )),
                    None => BridgeError::Emulator(format!(
                    "backend call completed but failed to restore the WebSocket timeout: {cleanup}"
                )),
                }
            });
        if restore_failed {
            self.terminal = true;
        }
        combined
    }
}

impl WsTransport for TungsteniteWs {
    fn frame_step_cancellable(
        &mut self,
        count: u64,
        timeout: Duration,
        cancellation: &crate::live::link::RequestCancellation,
    ) -> BridgeResult<Value> {
        if self.terminal {
            return Err(BridgeError::BadState("native transport is retired".into()));
        }
        if count == 0 || count > crate::live::temporal::MAX_SYNC_ADVANCE_COUNT {
            return Err(BridgeError::BadParams("invalid frame count".into()));
        }
        if cancellation.is_cancelled() || timeout.is_zero() {
            return Err(BridgeError::BadState(
                "frame operation cancelled before admission".into(),
            ));
        }
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| BridgeError::BadParams("frame deadline overflow".into()))?;
        // Probe before mutation. An old host's unknown-event response is a safe rejection.
        let capability = self.call_with_timeout(
            "emucap.frameStep.capability",
            json!({}),
            timeout.min(Duration::from_secs(2)),
        )?;
        if capability
            .get("memory_park_version")
            .and_then(Value::as_u64)
            != Some(1)
            || capability.get("version").and_then(Value::as_u64) != Some(1)
            || capability.get("connection_owned").and_then(Value::as_bool) != Some(true)
        {
            return Err(BridgeError::Unsupported(
                "native owned frame protocol v1".into(),
            ));
        }
        if cancellation.is_cancelled() || std::time::Instant::now() >= deadline {
            return Err(BridgeError::BadState(
                "frame operation cancelled before admission".into(),
            ));
        }
        let ticket = self.mint_ticket();
        let cancel_ticket = self.mint_ticket();
        let operation = crate::live::temporal::fresh_identity();
        let outcome = self.with_socket_timeout(Duration::from_millis(25), |transport| {
            transport.send_request(
                "emucap.frameStep",
                json!({"count":count,"ticket":ticket,"operation_id":operation}),
            )?;
            let mut cancel_deadline = None;
            let mut host_deadline = false;
            let mut terminal = None;
            let mut cancel_ack = false;
            loop {
                let now = std::time::Instant::now();
                if cancel_deadline.is_none() && (cancellation.is_cancelled() || now >= deadline) {
                    host_deadline = now >= deadline;
                    transport.send_request(
                        "emucap.frameStep.cancel",
                        json!({"ticket":cancel_ticket,"operation_id":operation}),
                    )?;
                    cancel_deadline = Some(now + Duration::from_millis(2500));
                }
                if cancel_deadline.is_some_and(|until| now >= until) {
                    return Err(BridgeError::Emulator(
                        "frame cancellation stop was not verified within its deadline".into(),
                    ));
                }
                let message = match transport.socket.read() {
                    Ok(message) => message,
                    Err(tungstenite::Error::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) =>
                    {
                        continue
                    }
                    Err(error) => return Err(error.into()),
                };
                match message {
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(text.as_str())?;
                        let seen = value.get("ticket").and_then(Value::as_str);
                        let event = value.get("event").and_then(Value::as_str);
                        if seen == Some(ticket.as_str()) {
                            if event != Some("emucap.frameStep") || terminal.is_some() {
                                return Err(BridgeError::Emulator(
                                    "invalid native frame terminal".into(),
                                ));
                            }
                            let completed = value.get("completed").and_then(Value::as_u64);
                            let status = value.get("status").and_then(Value::as_str);
                            if value.get("operation_id").and_then(Value::as_str)
                                != Some(operation.as_str())
                                || value.get("state").and_then(Value::as_str) != Some("frozen")
                                || value.get("unit").and_then(Value::as_str) != Some("frames")
                                || value.get("count").and_then(Value::as_u64) != Some(count)
                                || !completed.is_some_and(|n| n <= count)
                                || !matches!(status, Some("completed" | "interrupted"))
                                || (status == Some("completed") && completed != Some(count))
                            {
                                return Err(BridgeError::Emulator(
                                    "unverified native frame terminal".into(),
                                ));
                            }
                            terminal = Some(value);
                        } else if seen == Some(cancel_ticket.as_str()) && cancel_deadline.is_some()
                        {
                            // Completion may have released the owner before cancel arrived.
                            // A matching error is safe only once the original stop terminal exists.
                            if cancel_ack
                                || !(event == Some("error")
                                    || (event == Some("emucap.frameStep.cancel")
                                        && value.get("status").and_then(Value::as_str)
                                            == Some("requested")))
                            {
                                return Err(BridgeError::Emulator(
                                    "invalid native cancellation acknowledgement".into(),
                                ));
                            }
                            cancel_ack = true;
                        } else {
                            transport.pending_events.push_back(value);
                        }
                        if terminal.is_some() && (cancel_deadline.is_none() || cancel_ack) {
                            let mut value = terminal.take().unwrap();
                            if host_deadline
                                && value["status"] == "interrupted"
                                && value["reason"] == "cancelled"
                            {
                                value["reason"] = json!("host_deadline");
                            }
                            return Ok(value);
                        }
                    }
                    Message::Close(_) => {
                        return Err(BridgeError::Emulator(
                            "native frame connection closed before terminal".into(),
                        ))
                    }
                    _ => (),
                }
            }
        });
        if outcome.is_err() {
            // After dispatch, uncertainty retires the channel and invokes native EOF cleanup.
            self.terminal = true;
            let _ = self.socket.get_ref().shutdown(std::net::Shutdown::Both);
        }
        outcome
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn call(&mut self, event: &str, params: Value) -> BridgeResult<Value> {
        let ticket = self.mint_ticket();
        self.call_ticketed(event, params, &ticket)
    }

    fn call_and_wait_for(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
    ) -> BridgeResult<Value> {
        let outcome = (|| {
            let ticket = self.mint_ticket();
            let mut obj = object_params(event, params)?;
            obj.insert("ticket".into(), json!(ticket));
            self.send_request(event, Value::Object(obj))?;
            self.read_until_ticketed(expect_event, &ticket)
        })();
        self.finish_transport(outcome)
    }

    fn call_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        timeout: Duration,
    ) -> BridgeResult<Value> {
        // Bound both the write and read. Applying the timeout only after send_request would let a
        // blocked socket write outlive the caller's operation deadline.
        let outcome = (|| {
            let ticket = self.mint_ticket();
            let mut obj = object_params(event, params)?;
            obj.insert("ticket".into(), json!(ticket));
            self.with_socket_timeout(timeout, |transport| {
                transport
                    .send_request(event, Value::Object(obj))
                    .and_then(|()| transport.read_until_ticketed(event, &ticket))
            })
        })();
        self.finish_transport(outcome)
    }

    fn call_and_wait_for_with_timeout(
        &mut self,
        event: &str,
        params: Value,
        expect_event: &str,
        timeout: Duration,
    ) -> BridgeResult<Value> {
        let outcome = (|| {
            let ticket = self.mint_ticket();
            let mut obj = object_params(event, params)?;
            obj.insert("ticket".into(), json!(ticket));
            self.with_socket_timeout(timeout, |transport| {
                transport
                    .send_request(event, Value::Object(obj))
                    .and_then(|()| transport.read_until_ticketed(expect_event, &ticket))
            })
        })();
        self.finish_transport(outcome)
    }

    fn call_ticketed(&mut self, event: &str, params: Value, ticket: &str) -> BridgeResult<Value> {
        let outcome = (|| {
            let mut obj = object_params(event, params)?;
            obj.insert("ticket".into(), json!(ticket));
            self.send_request(event, Value::Object(obj))?;
            self.read_until_ticketed(event, ticket)
        })();
        self.finish_transport(outcome)
    }

    fn drain_events(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending_events.drain(..).collect();
        if self.socket.get_mut().set_nonblocking(true).is_err() {
            self.terminal = true;
            return out;
        }
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(value) = serde_json::from_str::<Value>(text.as_str()) {
                        out.push(value);
                    }
                }
                Ok(Message::Close(_)) => {
                    self.terminal = true;
                    break;
                }
                Ok(_) => continue,
                Err(error) => {
                    if tungstenite_error_is_terminal(&error) {
                        self.terminal = true;
                    }
                    break;
                }
            }
        }
        if self.socket.get_mut().set_nonblocking(false).is_err() {
            self.terminal = true;
        }
        out
    }
}

fn transport_error_is_terminal(error: &BridgeError) -> bool {
    match error {
        BridgeError::Io(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        BridgeError::Ws(error) => tungstenite_error_is_terminal(error),
        _ => false,
    }
}

fn tungstenite_error_is_terminal(error: &tungstenite::Error) -> bool {
    match error {
        tungstenite::Error::Io(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        _ => true,
    }
}

fn object_params(event: &str, params: Value) -> BridgeResult<serde_json::Map<String, Value>> {
    match params {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(serde_json::Map::new()),
        other => Err(BridgeError::BadParams(format!(
            "params for {event} must be a JSON object, got {other}"
        ))),
    }
}
