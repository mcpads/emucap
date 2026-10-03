//! Native parent cleanup; the transport must authenticate the supplied attachment.
use super::*;
use crate::live::{
    control_session::{Attachment, EventKind, SessionEvent},
    link::RequestCancellation,
    reconnect::cancellation::OperationKey,
    temporal::owner::{CleanupEvidence, CleanupPlan, OwnershipError, ProducerOwnership},
};

fn ownership_error(error: OwnershipError) -> Pcsx2BridgeError {
    match error {
        OwnershipError::Busy => Pcsx2BridgeError::Busy,
        _ => Pcsx2BridgeError::BadState(error.to_string()),
    }
}
fn terminal(plan: &CleanupPlan) -> Value {
    let mut value = json!({"status":"completed","parent":plan.key,"cleanup_verified":true,"released_ports":plan.input_ports,"effects_started":plan.effects_started});
    if plan.effects_started {
        value["state"] = json!("frozen");
    }
    value
}
impl<T: PineTransport> Pcsx2Bridge<T> {
    fn parent_owner(&mut self) -> BridgeResult<&mut ProducerOwnership> {
        self.producer_ownership
            .as_mut()
            .ok_or_else(|| Pcsx2BridgeError::BadState("producer attachment is not admitted".into()))
    }
    pub fn apply_control_session(&mut self, event: SessionEvent) -> BridgeResult<()> {
        if self.launch_id.as_deref() != Some(&event.runtime) {
            return Err(Pcsx2BridgeError::BadState(
                "control lifecycle runtime mismatch".into(),
            ));
        }
        event
            .attachment
            .validate()
            .map_err(|e| Pcsx2BridgeError::BadParams(e.into()))?;
        if self.producer_ownership.is_none() {
            self.producer_ownership =
                Some(ProducerOwnership::new(event.runtime).map_err(ownership_error)?);
        }
        match event.kind {
            EventKind::Attach => self
                .parent_owner()?
                .attach(event.attachment)
                .map_err(ownership_error),
            EventKind::Detach => {
                if let Some(plan) = self
                    .parent_owner()?
                    .detach(&event.attachment)
                    .map_err(ownership_error)?
                {
                    self.clean_parent(plan)?;
                }
                Ok(())
            }
        }
    }
    pub fn begin_temporal_operation(
        &mut self,
        attachment: &Attachment,
        key: OperationKey,
    ) -> BridgeResult<()> {
        self.require_owned_healthy()?;
        let fresh = self.parent_owner()?.active_parent_key().is_none();
        let result = self
            .parent_owner()?
            .begin(attachment, key)
            .map_err(ownership_error);
        if result.is_ok() && fresh {
            self.native_stop_deadline = None;
        }
        result
    }
    pub fn finish_temporal_operation(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
    ) -> BridgeResult<Value> {
        if let Some(closed) = self.parent_owner()?.terminal(attachment, key) {
            return Ok(terminal(closed));
        }
        let plan = self
            .parent_owner()?
            .start_cleanup(attachment, key)
            .map_err(ownership_error)?;
        self.clean_parent(plan)
    }
    /// Every admitted parent effect uses this path, before entering ordinary native handlers.
    pub fn handle_parent_request(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
        request: Request,
        cancellation: RequestCancellation,
    ) -> BridgeResult<Response> {
        self.require_owned_healthy()?;
        if cancellation.is_cancelled() {
            self.parent_owner()?
                .authorize_observation(attachment, key)
                .map_err(ownership_error)?;
            if let Some(origin) = cancellation.cancelled_at() {
                let deadline = origin + owned_frame::STOP_BUDGET;
                self.native_stop_deadline = Some(
                    self.native_stop_deadline
                        .map_or(deadline, |previous| previous.min(deadline)),
                );
            }
            return Err(Pcsx2BridgeError::Cancelled);
        }
        if crate::live::continuity::is_read_only(&request.method) {
            self.parent_owner()?
                .authorize_observation(attachment, key)
                .map_err(ownership_error)?;
            return Ok(self.handle_request_cancellable(request, cancellation));
        }
        let input = match request.method.as_str() {
            "set_input" => {
                input::require_port_zero(&request.params)?;
                input::buttons_to_mask(request.params.get("buttons"))?;
                Some(0)
            }
            "pause" => None,
            "step" => {
                let frame_step = match request.params.get("unit").and_then(Value::as_str) {
                    Some("frames") => true,
                    None => request.params.get("frames").is_some(),
                    _ => false,
                };
                if !frame_step {
                    return Err(Pcsx2BridgeError::Unsupported(
                        "cancellation requires frame stepping".into(),
                    ));
                }
                let child: OperationKey = serde_json::from_value(
                    request.params.get("_control").cloned().ok_or_else(|| {
                        Pcsx2BridgeError::BadParams("controlled step identity is required".into())
                    })?,
                )
                .map_err(|e| Pcsx2BridgeError::BadParams(e.to_string()))?;
                child
                    .validate()
                    .map_err(|e| Pcsx2BridgeError::BadParams(e.to_string()))?;
                if child.runtime != key.runtime
                    || child.owner_id != key.owner_id
                    || child.operation_id == key.operation_id
                {
                    return Err(Pcsx2BridgeError::BadParams(
                        "child identity does not belong to parent".into(),
                    ));
                }
                None
            }
            _ => {
                return Err(Pcsx2BridgeError::Unsupported(
                    "method is outside the PCSX2 parent operation".into(),
                ))
            }
        };
        if let Some(port) = input {
            self.parent_owner()?
                .record_input_attempt(attachment, key, port)
                .map_err(ownership_error)?;
        } else {
            self.parent_owner()?
                .record_effect(attachment, key)
                .map_err(ownership_error)?;
        }
        Ok(self.handle_request_cancellable(request, cancellation))
    }
    fn require_owned_healthy(&self) -> BridgeResult<()> {
        if self.backend_terminal() || !self.owned_control {
            return Err(Pcsx2BridgeError::BadState(
                "owned native control is unavailable".into(),
            ));
        }
        Ok(())
    }

    fn clean_parent(&mut self, plan: CleanupPlan) -> BridgeResult<Value> {
        let mut evidence = CleanupEvidence::default();
        let deadline = self.cleanup_deadline();
        let previous_deadline = self.io_deadline.replace(deadline);
        let cleanup = (|| -> BridgeResult<()> {
            self.require_owned_healthy()?;
            if plan.effects_started {
                self.verify_owned_pause(deadline)?;
                evidence.stop_verified = true;
                if !plan.input_ports.is_empty() {
                    if !self
                        .command(MSG_EMUCAP_SET_INPUT, &0u32.to_le_bytes())?
                        .is_empty()
                    {
                        return Err(Pcsx2BridgeError::Protocol(
                            "invalid native input release acknowledgement".into(),
                        ));
                    }
                    let input = self.command(MSG_EMUCAP_INPUT_STATUS, &[])?;
                    if input != [0; 16] {
                        return Err(Pcsx2BridgeError::Protocol(
                            "native input release was not verified".into(),
                        ));
                    }
                    evidence
                        .released_ports
                        .extend(plan.input_ports.iter().copied());
                }
            }
            owned_frame::remaining(deadline)?;
            Ok(())
        })();
        self.io_deadline = previous_deadline;
        if cleanup.is_err() {
            evidence = CleanupEvidence::default();
        }
        let finished = self
            .parent_owner()?
            .finish_cleanup(&plan, evidence)
            .map_err(ownership_error);
        if let Err(error) = cleanup.and(finished) {
            self.control_unverified = true;
            return Err(error);
        }
        self.native_stop_deadline = None;
        Ok(terminal(&plan))
    }
}

impl<T: PineTransport + Send> crate::live::reconnect::owned::OwnedHandler for Pcsx2Bridge<T> {
    fn active_parent_key(&self) -> Option<OperationKey> {
        self.producer_ownership
            .as_ref()
            .and_then(ProducerOwnership::active_parent_key)
    }
    fn request(
        &mut self,
        attachment: &Attachment,
        request: Request,
        cancellation: RequestCancellation,
    ) -> crate::live::reconnect::BridgeReply {
        let id = request.id;
        let result = (|| -> BridgeResult<Response> {
            let parent_key = |value: Option<&Value>| -> BridgeResult<OperationKey> {
                serde_json::from_value(value.cloned().ok_or_else(|| {
                    Pcsx2BridgeError::BadParams("parent identity is required".into())
                })?)
                .map_err(|e| Pcsx2BridgeError::BadParams(e.to_string()))
            };
            let value = match request.method.as_str() {
                "begin_temporal_operation" => {
                    if cancellation.is_cancelled() {
                        return Err(Pcsx2BridgeError::Cancelled);
                    }
                    let key = parent_key(request.params.get("parent"))?;
                    self.begin_temporal_operation(attachment, key.clone())?;
                    json!({"status":"admitted","parent":key})
                }
                "finish_temporal_operation" => self.finish_temporal_operation(
                    attachment,
                    &parent_key(request.params.get("parent"))?,
                )?,
                _ => {
                    if let Some(parent) = request.params.get("_temporal_owner") {
                        return self.handle_parent_request(
                            attachment,
                            &parent_key(Some(parent))?,
                            request,
                            cancellation,
                        );
                    }
                    self.parent_owner()?
                        .authorize_unscoped(
                            attachment,
                            crate::live::continuity::is_read_only(&request.method),
                        )
                        .map_err(ownership_error)?;
                    let describes_session = matches!(request.method.as_str(), "hello" | "status");
                    let mut response = self.handle_request_cancellable(request, cancellation);
                    if describes_session && response.ok {
                        if let Some(result) = response.result.as_mut() {
                            result[crate::live::control_session::LIFECYCLE_FIELD] = json!(true);
                            if self.owned_control {
                                if let Some(methods) =
                                    result.get_mut("methods").and_then(Value::as_array_mut)
                                {
                                    methods.push(json!("cancel_operation"));
                                }
                                result["temporal_cancellation_capability"] = json!({
                                    "methods":["step"], "control_service_ms":50, "stop_host_ms":5000
                                });
                            }
                        }
                    }
                    return Ok(response);
                }
            };
            Ok(Response {
                id,
                ok: true,
                result: Some(value),
                error: None,
            })
        })();
        let response = result.unwrap_or_else(|error| Response {
            id,
            ok: false,
            result: None,
            error: Some(ProtocolError {
                kind: error_kind(&error).into(),
                message: error.to_string(),
            }),
        });
        if self.backend_terminal() {
            crate::live::reconnect::BridgeReply::terminate_with(response)
        } else {
            crate::live::reconnect::BridgeReply::continue_with(response)
        }
    }
    fn lifecycle(
        &mut self,
        event: SessionEvent,
    ) -> std::io::Result<crate::live::reconnect::BridgeDirective> {
        self.apply_control_session(event)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(if self.backend_terminal() {
            crate::live::reconnect::BridgeDirective::Terminate
        } else {
            crate::live::reconnect::BridgeDirective::Continue
        })
    }
}
