//! The temporal wire exchange is shared by direct and broker links. Cancellation intent never
//! substitutes for an original terminal; both request IDs are drained before stream reuse.
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::live::link::{LinkError, ProgressCallControl};
use crate::live::protocol::{parse_response, read_ndjson_chunk, to_line, Request, Response};
use crate::live::reconnect::cancellation::OperationKey;

const POLL: Duration = Duration::from_millis(25);

struct SocketTimeouts {
    read: Option<Duration>,
    write: Option<Duration>,
}
impl SocketTimeouts {
    fn capture(reader: &TcpStream, writer: &TcpStream) -> Result<Self, LinkError> {
        Ok(Self {
            read: reader.read_timeout().map_err(unverified)?,
            write: writer.write_timeout().map_err(unverified)?,
        })
    }
    fn clamp(
        &self,
        reader: &TcpStream,
        writer: &TcpStream,
        deadline: Instant,
    ) -> Result<(), LinkError> {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        reader
            .set_read_timeout(Some(remaining.min(POLL)))
            .map_err(unverified)?;
        writer
            .set_write_timeout(Some(remaining.min(self.write.unwrap_or(POLL))))
            .map_err(unverified)
    }
    fn restore(&self, reader: &TcpStream, writer: &TcpStream) -> Result<(), LinkError> {
        let read = reader.set_read_timeout(self.read);
        let write = writer.set_write_timeout(self.write);
        read.and(write).map_err(unverified)
    }
}

fn unverified(error: impl std::fmt::Display) -> LinkError {
    LinkError::Emulator {
        kind: "temporal_unverified".into(),
        message: format!("native terminal was not verified: {error}"),
    }
}

/// A drained terminal preserves stream identity, including a producer rejection.
/// Transport, deadline, malformed-terminal and abort-drain failures remain outer errors.
#[derive(Debug)]
pub(crate) struct TerminalResponse {
    pub result: Result<Value, LinkError>,
}

/// The caller retires the connection only when the exchange itself is unverified.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exchange(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    pending: &mut Vec<u8>,
    request: Request,
    abort_id: u64,
    control: &ProgressCallControl,
    max_host: Duration,
) -> Result<TerminalResponse, LinkError> {
    let timeouts = SocketTimeouts::capture(reader.get_ref(), writer)?;
    let result = exchange_inner(
        reader, writer, pending, request, abort_id, control, max_host, &timeouts,
    );
    timeouts.restore(reader.get_ref(), writer)?;
    result
}

#[allow(clippy::too_many_arguments)]
fn exchange_inner(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    pending: &mut Vec<u8>,
    request: Request,
    abort_id: u64,
    control: &ProgressCallControl,
    max_host: Duration,
    timeouts: &SocketTimeouts,
) -> Result<TerminalResponse, LinkError> {
    let stop_ms = control
        .temporal_stop_ms
        .filter(|ms| *ms > 0 && *ms <= 300_000)
        .ok_or_else(|| LinkError::Protocol("invalid temporal stop budget".into()))?;
    let abort = control
        .abort
        .as_ref()
        .filter(|a| a.method == "cancel_operation")
        .ok_or_else(|| LinkError::Protocol("temporal call requires cancel_operation".into()))?;
    let key: OperationKey = serde_json::from_value(abort.params.clone())
        .map_err(|e| LinkError::Protocol(e.to_string()))?;
    key.validate()
        .map_err(|e| LinkError::Protocol(e.to_string()))?;
    let bound_key = if matches!(
        request.method.as_str(),
        "begin_temporal_operation" | "finish_temporal_operation"
    ) {
        if request.params.get("_temporal_owner") != Some(&abort.params) {
            return Err(LinkError::Protocol(
                "parent ownership and cancellation identities disagree".into(),
            ));
        }
        request.params.get("parent")
    } else {
        request
            .params
            .get("_control")
            .or_else(|| request.params.get("_temporal_owner"))
    };
    if bound_key != Some(&abort.params) || request.id == abort_id {
        return Err(LinkError::Protocol(
            "temporal request and cancellation identities disagree".into(),
        ));
    }
    if control.cancellation.is_cancelled() {
        return Err(LinkError::Cancelled);
    }
    let operation_budget = control
        .max_host_ms
        .map(Duration::from_millis)
        .unwrap_or(max_host)
        .min(max_host);
    let relative_deadline = Instant::now() + operation_budget;
    let operation_deadline = control
        .temporal_deadline
        .map_or(relative_deadline, |deadline| {
            deadline.min(relative_deadline)
        });
    if Instant::now() >= operation_deadline {
        return Err(unverified("temporal deadline expired before dispatch"));
    }
    timeouts.clamp(reader.get_ref(), writer, operation_deadline)?;
    writer
        .write_all(to_line(&request).as_bytes())
        .map_err(unverified)?;
    let mut stop_deadline: Option<Instant> = None;
    let mut acknowledgement = None;
    let mut terminal: Option<Response> = None;
    let mut mismatches = 0;
    loop {
        let deadline = stop_deadline.map_or(operation_deadline, |at| at.min(operation_deadline));
        if Instant::now() >= deadline {
            return Err(unverified(
                "host deadline expired before terminal/abort drain",
            ));
        }
        timeouts.clamp(reader.get_ref(), writer, deadline)?;
        if stop_deadline.is_none() || acknowledgement.is_some() {
            if let Some(response) = terminal.take() {
                if let Some(Err(error)) = acknowledgement {
                    return Err(error);
                }
                let result = if response.ok {
                    Ok(response.result.unwrap_or(Value::Null))
                } else {
                    let error = response.error.ok_or_else(|| {
                        LinkError::Protocol("terminal error omitted details".into())
                    })?;
                    Err(LinkError::Emulator {
                        kind: error.kind,
                        message: error.message,
                    })
                };
                return Ok(TerminalResponse { result });
            }
        }
        if stop_deadline.is_none() && control.cancellation.is_cancelled() {
            let at = control
                .cancellation
                .cancelled_at()
                .expect("cancellation observed")
                + Duration::from_millis(stop_ms);
            if Instant::now() >= at {
                return Err(unverified("shared cancellation deadline already expired"));
            }
            stop_deadline = Some(at);
            timeouts.clamp(reader.get_ref(), writer, at.min(operation_deadline))?;
            writer
                .write_all(
                    to_line(&Request::new(abort_id, &abort.method, abort.params.clone()))
                        .as_bytes(),
                )
                .map_err(unverified)?;
        }
        let line = match read_ndjson_chunk(reader, pending) {
            Ok(Some(line)) => line,
            Ok(None) => return Err(unverified("connection closed")),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(error) => return Err(unverified(error)),
        };
        let response = parse_response(line.trim()).map_err(unverified)?;
        if response.id == abort_id && stop_deadline.is_some() {
            if acknowledgement.is_some() {
                return Err(unverified("duplicate abort acknowledgement"));
            }
            acknowledgement = Some(
                if response.ok
                    && matches!(
                        response
                            .result
                            .as_ref()
                            .and_then(|v| v.get("status"))
                            .and_then(Value::as_str),
                        Some("requested" | "not_active")
                    )
                {
                    Ok(())
                } else {
                    Err(unverified("invalid abort acknowledgement"))
                },
            );
        } else if response.id == request.id {
            if terminal.is_some() {
                return Err(unverified("response after original terminal"));
            }
            if response.ok
                && response
                    .result
                    .as_ref()
                    .is_some_and(|v| v["status"] == "working")
            {
                continue;
            }
            terminal = Some(response);
        } else {
            mismatches += 1;
            if mismatches > 256 {
                return Err(unverified("too many unrelated responses"));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
