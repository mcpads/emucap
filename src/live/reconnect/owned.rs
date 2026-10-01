//! Session lifetime owns parent cleanup, including EOF while no child request is running.
use super::*;
use crate::live::{
    control_session::{Attachment, EventKind, SessionEvent, ATTACHMENT_FIELD, EVENT_FIELD},
    link::RequestCancellation,
};
use cancellation::{OperationKey, TemporalAdmission};
use serde_json::Value;

pub trait OwnedHandler: Send {
    fn active_parent_key(&self) -> Option<OperationKey> {
        None
    }
    fn request(
        &mut self,
        attachment: &Attachment,
        request: Request,
        cancellation: RequestCancellation,
    ) -> BridgeReply;
    fn lifecycle(&mut self, event: SessionEvent) -> io::Result<BridgeDirective>;
}

pub fn serve_reconnecting_owned<H, P>(
    port: u16,
    label: &str,
    mut handler: H,
    mut probe: P,
    admission: TemporalAdmission,
) -> io::Result<()>
where
    H: OwnedHandler,
    P: FnMut() -> Option<String>,
{
    if admission.runtime.is_empty() {
        return Err(invalid("owned sessions require a runtime identity"));
    }
    reconnect_sessions(port, label, &mut probe, None, |stream, probe| {
        serve_one(stream, &mut handler, probe, &admission)
    })
}
fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
struct Route {
    runtime: String,
    current: Option<Attachment>,
    broker: Option<(String, u64)>,
}
impl Route {
    fn event(&self, kind: EventKind, attachment: Attachment) -> SessionEvent {
        SessionEvent {
            kind,
            runtime: self.runtime.clone(),
            attachment,
        }
    }
    fn apply<H: OwnedHandler>(&mut self, handler: &mut H, event: SessionEvent) -> io::Result<()> {
        if event.runtime != self.runtime {
            return Err(invalid("lifecycle runtime mismatch"));
        }
        event.attachment.validate().map_err(invalid)?;
        if event.kind == EventKind::Attach {
            if self.broker.is_none() {
                self.close(handler)?;
                self.broker = Some((
                    event.attachment.broker_instance.clone(),
                    event.attachment.registration,
                ));
            }
            if self.broker.as_ref()
                != Some(&(
                    event.attachment.broker_instance.clone(),
                    event.attachment.registration,
                ))
            {
                return Err(invalid("producer transport registration changed"));
            }
            if self
                .current
                .as_ref()
                .is_some_and(|current| current != &event.attachment)
            {
                return Err(invalid("attachment replaced without detach"));
            }
            lifecycle(handler, event.clone())?;
            self.current = Some(event.attachment);
        } else if self.current.as_ref() == Some(&event.attachment) {
            self.close(handler)?;
        }
        Ok(())
    }
    fn close<H: OwnedHandler>(&mut self, handler: &mut H) -> io::Result<()> {
        if let Some(attachment) = self.current.take() {
            lifecycle(handler, self.event(EventKind::Detach, attachment))?;
        }
        Ok(())
    }
    fn request_attachment(&self, request: &mut Request) -> io::Result<Attachment> {
        let current = self
            .current
            .as_ref()
            .ok_or_else(|| invalid("no attached control session"))?;
        let stamp = request
            .params
            .as_object_mut()
            .and_then(|params| params.remove(ATTACHMENT_FIELD));
        match (&self.broker, stamp) {
            (Some(_), Some(value)) => {
                let observed: Attachment = serde_json::from_value(value).map_err(invalid)?;
                if &observed != current {
                    return Err(invalid("request attachment mismatch"));
                }
            }
            (None, None) => (),
            _ => return Err(invalid("invalid transport attachment metadata")),
        }
        Ok(current.clone())
    }
}
fn lifecycle<H: OwnedHandler>(handler: &mut H, event: SessionEvent) -> io::Result<()> {
    match handler.lifecycle(event)? {
        BridgeDirective::Continue => Ok(()),
        BridgeDirective::Terminate => Err(invalid("native lifecycle cleanup retired control")),
    }
}

fn serve_one<H: OwnedHandler, P: FnMut() -> Option<String>>(
    stream: TcpStream,
    handler: &mut H,
    probe: &mut P,
    admission: &TemporalAdmission,
) -> io::Result<SessionEnd> {
    let direct = Attachment {
        broker_instance: format!("direct-{}", crate::live::temporal::fresh_identity()),
        registration: 1,
        session: 1,
    };
    let mut route = Route {
        runtime: admission.runtime.clone(),
        current: Some(direct.clone()),
        broker: None,
    };
    if let Err(error) = lifecycle(handler, route.event(EventKind::Attach, direct)) {
        return Ok(SessionEnd::DependencyTerminal(error.to_string()));
    }
    let result = run(stream, handler, probe, admission, &mut route);
    // Even failed reads/writes and idle EOF must finalize the parent before reconnect.
    if let Err(error) = route.close(handler) {
        return Ok(SessionEnd::DependencyTerminal(error.to_string()));
    }
    result
}
fn run<H: OwnedHandler, P: FnMut() -> Option<String>>(
    stream: TcpStream,
    handler: &mut H,
    probe: &mut P,
    admission: &TemporalAdmission,
    route: &mut Route,
) -> io::Result<SessionEnd> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut pending = vec![];
    loop {
        if let Some(reason) = probe() {
            return Ok(SessionEnd::DependencyTerminal(reason));
        }
        let line = match read_ndjson_frame(&mut reader, &mut pending) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(SessionEnd::FrontDisconnected),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(invalid)?;
        if let Some(event) = SessionEvent::from_envelope(&value).map_err(invalid)? {
            if let Err(error) = route.apply(handler, event) {
                return Ok(SessionEnd::DependencyTerminal(error.to_string()));
            }
            continue;
        }
        if value.get("method").and_then(Value::as_str) == Some(EVENT_FIELD)
            || value.get(ATTACHMENT_FIELD).is_some()
        {
            return Err(invalid("reserved lifecycle fields"));
        }
        let mut request: Request = serde_json::from_value(value).map_err(invalid)?;
        if request.v != crate::live::protocol::PROTOCOL_VERSION {
            return Err(invalid("unsupported protocol version"));
        }
        let attachment = route.request_attachment(&mut request)?;
        if request.method == "cancel_operation" {
            let target: OperationKey = serde_json::from_value(request.params).map_err(invalid)?;
            target.validate()?;
            write_response(
                &mut writer,
                &Response {
                    id: request.id,
                    ok: true,
                    error: None,
                    result: Some(serde_json::json!({"status":"not_active"})),
                },
            )?;
            continue;
        }
        let child = admission.admit(&request).map_err(invalid)?;
        let parent = request
            .params
            .get("_temporal_owner")
            .or_else(|| {
                matches!(
                    request.method.as_str(),
                    "begin_temporal_operation" | "finish_temporal_operation"
                )
                .then(|| request.params.get("parent"))
                .flatten()
            })
            .map(|value| serde_json::from_value::<OperationKey>(value.clone()).map_err(invalid))
            .transpose()?;
        let active_key = child.or(parent).or_else(|| handler.active_parent_key());
        if let Some(key) = active_key {
            key.validate()?;
            if key.runtime != admission.runtime {
                return Err(invalid("parent runtime mismatch"));
            }
            let completion = cancellation::execute_cancellable_attached(
                &mut reader,
                &mut writer,
                &mut pending,
                request,
                &key,
                route.broker.as_ref().map(|_| &attachment),
                WORKING_INTERVAL,
                |request, cancel| handler.request(&attachment, request, cancel),
            )?;
            if let Some(event) = completion.session_event {
                if let Err(error) = route.apply(handler, event) {
                    return Ok(SessionEnd::DependencyTerminal(error.to_string()));
                }
            }
            if completion.directive == BridgeDirective::Terminate {
                return Ok(SessionEnd::DependencyTerminal(
                    "native request retired control".into(),
                ));
            }
            if let Some(error) = completion.transport_error {
                return Err(error);
            }
        } else {
            let completion = serve_request_controlled(
                &mut writer,
                &mut |request| handler.request(&attachment, request, Default::default()),
                request,
                WORKING_INTERVAL,
            )?;
            if completion.directive == BridgeDirective::Terminate {
                return Ok(SessionEnd::DependencyTerminal(
                    "native request retired control".into(),
                ));
            }
            if let Some(error) = completion.write_error {
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests;
