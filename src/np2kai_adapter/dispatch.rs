//! Move control messages, never the libretro core, across the server/owner boundary.
use super::*;
use crate::live::{
    control_session::{Attachment, SessionEvent},
    link::RequestCancellation,
    reconnect::{cancellation::OperationKey, owned::OwnedHandler, BridgeDirective, BridgeReply},
};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::time::Instant;

type RequestReply = (BridgeReply, Option<OperationKey>);
type LifecycleReply = std::io::Result<(BridgeDirective, Option<OperationKey>)>;
pub enum Command {
    Request {
        attachment: Option<Attachment>,
        request: Request,
        cancellation: RequestCancellation,
        reply: SyncSender<RequestReply>,
    },
    Lifecycle {
        event: SessionEvent,
        reply: SyncSender<LifecycleReply>,
    },
}
pub struct Control {
    sender: Sender<Command>,
    parent: Option<OperationKey>,
    retired: bool,
}
fn wait_reply(
    receiver: mpsc::Receiver<RequestReply>,
    signal: &RequestCancellation,
    deadline: Instant,
    stop_budget: Duration,
) -> Option<RequestReply> {
    loop {
        // Once either deadline expires, a late response cannot revalidate the owner.
        if Instant::now() >= deadline
            || signal
                .cancelled_at()
                .is_some_and(|at| at.elapsed() >= stop_budget)
        {
            return None;
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(reply) => {
                if Instant::now() >= deadline
                    || signal
                        .cancelled_at()
                        .is_some_and(|at| at.elapsed() >= stop_budget)
                {
                    return None;
                }
                return Some(reply);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
    }
}

impl Control {
    pub fn new(sender: Sender<Command>) -> Self {
        Self {
            sender,
            parent: None,
            retired: false,
        }
    }
    fn call(
        &mut self,
        attachment: Option<Attachment>,
        request: Request,
        cancellation: RequestCancellation,
    ) -> BridgeReply {
        let id = request.id;
        let (reply, receiver) = mpsc::sync_channel(1);
        let signal = cancellation.clone();
        let deadline = Instant::now()
            + crate::live::temporal::MAX_SYNC_OPERATION_TIME
            + Duration::from_secs(5);
        if !self.retired
            && self
                .sender
                .send(Command::Request {
                    attachment,
                    request,
                    cancellation,
                    reply,
                })
                .is_ok()
        {
            if let Some((response, parent)) =
                wait_reply(receiver, &signal, deadline, Duration::from_secs(5))
            {
                self.parent = parent;
                return response;
            }
        }
        self.retired = true;
        BridgeReply::terminate_with(Response {
            id,
            ok: false,
            result: None,
            error: Some(ProtocolError {
                kind: "adapter_error".into(),
                message: "NP2kai owner reply unavailable within the control deadline".into(),
            }),
        })
    }
    pub fn legacy(&mut self, request: Request) -> BridgeReply {
        self.call(None, request, Default::default())
    }
}
impl OwnedHandler for Control {
    fn active_parent_key(&self) -> Option<OperationKey> {
        self.parent.clone()
    }
    fn request(
        &mut self,
        attachment: &Attachment,
        request: Request,
        cancellation: RequestCancellation,
    ) -> BridgeReply {
        self.call(Some(attachment.clone()), request, cancellation)
    }
    fn lifecycle(&mut self, event: SessionEvent) -> std::io::Result<BridgeDirective> {
        if self.retired {
            return Err(std::io::Error::other("NP2kai owner control retired"));
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Lifecycle { event, reply })
            .map_err(|_| std::io::Error::other("NP2kai owner ended"))?;
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((directive, parent))) => {
                self.parent = parent;
                Ok(directive)
            }
            result => {
                self.retired = true;
                Err(std::io::Error::other(format!(
                    "NP2kai lifecycle cleanup unverified: {result:?}"
                )))
            }
        }
    }
}
impl Np2kaiHost {
    pub fn process_command(&mut self, command: Command) {
        match command {
            Command::Request {
                attachment,
                request,
                cancellation,
                reply,
            } => {
                let response = if let Some(attachment) = attachment {
                    self.owned_request(&attachment, request, cancellation)
                } else {
                    let response = self.handle_request(request);
                    if self.backend_terminal() {
                        BridgeReply::terminate_with(response)
                    } else {
                        BridgeReply::continue_with(response)
                    }
                };
                if reply.send((response, self.active_parent_key())).is_err() {
                    self.control_unverified = true;
                }
            }
            Command::Lifecycle { event, reply } => {
                let result = self.owned_lifecycle(event);
                if result.is_err() {
                    self.control_unverified = true;
                }
                if reply
                    .send(result.map(|directive| (directive, self.active_parent_key())))
                    .is_err()
                {
                    self.control_unverified = true;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
