//! Opt-in request executor. The caller owns admission and native retirement; the worker owns
//! native I/O and must return only after its bounded stop/cleanup path has finished.
use super::*;
use crate::live::control_session::{Attachment, EventKind, SessionEvent, ATTACHMENT_FIELD};
use crate::live::link::RequestCancellation;
use crate::live::protocol::read_ndjson_chunk as read_control_chunk;
use serde::{Deserialize, Serialize};
use std::time::Instant;

const CONTROL_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationKey {
    pub runtime: String,
    pub owner_id: String,
    pub operation_id: String,
}

impl OperationKey {
    pub fn validate(&self) -> io::Result<()> {
        if [&self.runtime, &self.owner_id, &self.operation_id]
            .iter()
            .any(|value| value.is_empty() || value.len() > 256)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid operation identity",
            ));
        }
        Ok(())
    }
}

/// Runtime identity comes from the producer, not the incoming request.
pub struct TemporalAdmission {
    pub runtime: String,
    pub methods: Vec<String>,
}

impl TemporalAdmission {
    pub(super) fn admit(&self, request: &Request) -> Result<Option<OperationKey>, String> {
        let Some(envelope) = request.params.get("_control") else {
            return Ok(None);
        };
        if request.v != crate::live::protocol::PROTOCOL_VERSION {
            return Err("unsupported protocol version".into());
        }
        if !self.methods.contains(&request.method) {
            return Err("temporal cancellation is unavailable for this method".into());
        }
        let key: OperationKey =
            serde_json::from_value(envelope.clone()).map_err(|e| e.to_string())?;
        key.validate().map_err(|e| e.to_string())?;
        if key.runtime != self.runtime || self.runtime.is_empty() {
            return Err("cancellation runtime does not match this producer".into());
        }
        Ok(Some(key))
    }
}

pub struct CancellableCompletion {
    pub directive: BridgeDirective,
    pub transport_error: Option<io::Error>,
    /// Process this detach with the native owner after the active child has joined.
    pub session_event: Option<SessionEvent>,
}

/// Execute one already-admitted operation while receiving control on its existing reader.
/// Keep this reader and `pending` for the complete session, including any partial next frame.
/// `key` must have been bound to the admitted producer generation by the caller.
///
/// A cancel reply acknowledges intent only. The original reply remains authoritative. EOF,
/// malformed control or a failed write cancels the worker and joins cleanup before returning.
/// This executor does not impose a native-stop timeout by abandoning the worker: its handler
/// must enforce that deadline and return `Terminate` if native control becomes uncertain.
#[allow(clippy::too_many_arguments)]
pub fn execute_cancellable<F>(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    pending: &mut Vec<u8>,
    request: Request,
    key: &OperationKey,
    working_interval: Duration,
    handle: F,
) -> io::Result<CancellableCompletion>
where
    F: FnOnce(Request, RequestCancellation) -> BridgeReply + Send,
{
    execute_cancellable_attached(
        reader,
        writer,
        pending,
        request,
        key,
        None,
        working_interval,
        handle,
    )
}

/// The attachment is supplied by the authenticated session, never by the child request.
#[allow(clippy::too_many_arguments)]
pub fn execute_cancellable_attached<F>(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    pending: &mut Vec<u8>,
    request: Request,
    key: &OperationKey,
    attachment: Option<&Attachment>,
    working_interval: Duration,
    handle: F,
) -> io::Result<CancellableCompletion>
where
    F: FnOnce(Request, RequestCancellation) -> BridgeReply + Send,
{
    key.validate()?;
    if let Some(attachment) = attachment {
        attachment
            .validate()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    }
    let previous_timeout = reader.get_ref().read_timeout()?;
    reader.get_ref().set_read_timeout(Some(CONTROL_POLL))?;
    let cancellation = RequestCancellation::default();
    let worker_cancel = cancellation.clone();
    let id = request.id;
    let result = std::thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel(1);
        scope.spawn(move || {
            let _ = tx.send(handle(request, worker_cancel));
        });
        let mut last_working = Instant::now();
        let mut transport_error = None;
        let mut session_event = None;
        loop {
            match rx.try_recv() {
                Ok(reply) => {
                    if transport_error.is_none() {
                        transport_error = write_response(writer, &reply.response).err();
                    }
                    return Ok(CancellableCompletion {
                        directive: reply.directive,
                        transport_error,
                        session_event,
                    });
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(io::Error::other(
                        "cancellable handler exited without a response",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if transport_error.is_some() || session_event.is_some() {
                // Cleanup remains worker-owned after the front connection is lost.
                std::thread::sleep(CONTROL_POLL);
                continue;
            }
            let incoming = read_control_chunk(reader, pending);
            let response = match incoming {
                Ok(Some(line)) if line.trim().is_empty() => None,
                Ok(Some(line)) => {
                    let parsed = (|| -> Result<Option<Request>, String> {
                        let mut value: serde_json::Value =
                            serde_json::from_str(&line).map_err(|e| e.to_string())?;
                        if let Some(event) = SessionEvent::from_envelope(&value)? {
                            let Some(active) = attachment else {
                                return Err("unnegotiated lifecycle envelope".into());
                            };
                            if event.runtime != key.runtime {
                                return Err("lifecycle runtime mismatch".into());
                            }
                            if event.kind != EventKind::Detach {
                                return Err(
                                    "attach arrived before active attachment detached".into()
                                );
                            }
                            if event.attachment == *active {
                                cancellation.cancel();
                                session_event = Some(event);
                            }
                            return Ok(None); // Lifecycle has no request response.
                        }
                        if let Some(active) = attachment {
                            let stamp = value
                                .get_mut("params")
                                .and_then(serde_json::Value::as_object_mut)
                                .and_then(|params| params.remove(ATTACHMENT_FIELD))
                                .ok_or_else(|| {
                                    "missing authenticated control attachment".to_string()
                                })?;
                            let stamp: Attachment =
                                serde_json::from_value(stamp).map_err(|e| e.to_string())?;
                            if stamp != *active {
                                return Err("control attachment mismatch".into());
                            }
                        }
                        serde_json::from_value(value)
                            .map(Some)
                            .map_err(|e| e.to_string())
                    })();
                    match parsed {
                        Ok(Some(control))
                            if control.v != crate::live::protocol::PROTOCOL_VERSION =>
                        {
                            Some(error_response(
                                control.id,
                                "protocol_error",
                                "unsupported protocol version",
                            ))
                        }
                        Ok(Some(control)) if control.method == "cancel_operation" => {
                            match serde_json::from_value::<OperationKey>(control.params) {
                                Ok(target) if target.validate().is_ok() => {
                                    let matches = target == *key;
                                    if matches {
                                        cancellation.cancel();
                                    }
                                    Some(Response {
                                        id: control.id,
                                        ok: true,
                                        error: None,
                                        result: Some(
                                            serde_json::json!({"status": if matches { "requested" } else { "not_active" }}),
                                        ),
                                    })
                                }
                                _ => Some(error_response(
                                    control.id,
                                    "bad_params",
                                    "invalid cancellation identity",
                                )),
                            }
                        }
                        Ok(Some(control)) => Some(error_response(
                            control.id,
                            "busy",
                            "native operation is active",
                        )),
                        Ok(None) => None,
                        Err(error) => {
                            transport_error =
                                Some(io::Error::new(io::ErrorKind::InvalidData, error));
                            cancellation.cancel();
                            None
                        }
                    }
                }
                Ok(None) => {
                    transport_error = Some(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "control connection closed",
                    ));
                    cancellation.cancel();
                    None
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    None
                }
                Err(error) => {
                    transport_error = Some(error);
                    cancellation.cancel();
                    None
                }
            };
            if let Some(response) = response {
                if let Err(error) = write_response(writer, &response) {
                    transport_error = Some(error);
                    cancellation.cancel();
                }
            }
            if transport_error.is_none() && last_working.elapsed() >= working_interval {
                let working = Response {
                    id,
                    ok: true,
                    error: None,
                    result: Some(serde_json::json!({"status":"working"})),
                };
                if let Err(error) = write_response(writer, &working) {
                    transport_error = Some(error);
                    cancellation.cancel();
                }
                last_working = Instant::now();
            }
        }
    });
    let restored = reader.get_ref().set_read_timeout(previous_timeout);
    match result {
        Err(error) => Err(error),
        Ok(mut completion) => {
            if completion.transport_error.is_none() {
                completion.transport_error = restored.err();
            }
            Ok(completion)
        }
    }
}

fn error_response(id: u64, kind: &str, message: &str) -> Response {
    Response {
        id,
        ok: false,
        result: None,
        error: Some(ProtocolError {
            kind: kind.into(),
            message: message.into(),
        }),
    }
}

#[cfg(test)]
mod tests;
