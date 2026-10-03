//! Native parent cleanup; the transport must authenticate the supplied attachment.
use super::control::{normalize_buttons, require_port_zero};
use super::*;
use crate::live::{
    control_session::{Attachment, EventKind, SessionEvent},
    link::RequestCancellation,
    reconnect::cancellation::OperationKey,
    temporal::owner::{CleanupEvidence, CleanupPlan, OwnershipError, ProducerOwnership},
};

fn ownership_error(error: OwnershipError) -> N64Error {
    match error {
        OwnershipError::Busy => N64Error::Busy,
        _ => N64Error::BadState(error.to_string()),
    }
}
fn terminal(plan: &CleanupPlan) -> Value {
    let mut value = json!({"status":"completed","parent":plan.key,"cleanup_verified":true,"released_ports":plan.input_ports,"effects_started":plan.effects_started});
    if plan.effects_started {
        value["state"] = json!("frozen");
    }
    value
}
impl Mupen64PlusHost {
    fn parent_owner(&mut self) -> N64Result<&mut ProducerOwnership> {
        self.producer_ownership
            .as_mut()
            .ok_or_else(|| N64Error::BadState("producer attachment is not admitted".into()))
    }
    pub fn apply_control_session(&mut self, event: SessionEvent) -> N64Result<()> {
        if self.launch_id.as_deref() != Some(&event.runtime) {
            return Err(N64Error::BadState(
                "control lifecycle runtime mismatch".into(),
            ));
        }
        event
            .attachment
            .validate()
            .map_err(|e| N64Error::BadParams(e.into()))?;
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
    ) -> N64Result<()> {
        self.require_connected()?;
        self.parent_owner()?
            .begin(attachment, key)
            .map_err(ownership_error)
    }
    pub fn finish_temporal_operation(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
    ) -> N64Result<Value> {
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
    ) -> N64Result<Response> {
        self.require_connected()?;
        if cancellation.is_cancelled() {
            return Err(N64Error::Cancelled);
        }
        if crate::live::continuity::is_read_only(&request.method) {
            self.parent_owner()?
                .authorize_observation(attachment, key)
                .map_err(ownership_error)?;
            return Ok(self.handle_request_cancellable(request, cancellation));
        }
        let input = match request.method.as_str() {
            "set_input" => {
                require_port_zero(&request.params)?;
                normalize_buttons(request.params.get("buttons"))?;
                Some(0)
            }
            "pause" => None,
            "step" => {
                if request
                    .params
                    .get("unit")
                    .and_then(Value::as_str)
                    .unwrap_or("frames")
                    != "frames"
                {
                    return Err(N64Error::Unsupported(
                        "cancellation requires frame stepping".into(),
                    ));
                }
                let child: OperationKey =
                    serde_json::from_value(request.params.get("_control").cloned().ok_or_else(
                        || N64Error::BadParams("controlled step identity is required".into()),
                    )?)
                    .map_err(|e| N64Error::BadParams(e.to_string()))?;
                child
                    .validate()
                    .map_err(|e| N64Error::BadParams(e.to_string()))?;
                if child.runtime != key.runtime
                    || child.owner_id != key.owner_id
                    || child.operation_id == key.operation_id
                {
                    return Err(N64Error::BadParams(
                        "child identity does not belong to parent".into(),
                    ));
                }
                None
            }
            _ => {
                return Err(N64Error::Unsupported(
                    "method is outside the N64 parent operation".into(),
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
    fn clean_parent(&mut self, plan: CleanupPlan) -> N64Result<Value> {
        let mut evidence = CleanupEvidence::default();
        let cleanup = (|| -> N64Result<()> {
            if plan.effects_started {
                self.drain_debug_update()?;
                if !(self.frozen && self.is_frozen_boundary()) {
                    let before = UPDATE_COUNT.load(Ordering::Acquire);
                    let stopped = frame::park_after_cancellation(before, RECOVERY_DEADLINE,
                        || check_core("DebugSetRunState(parent cleanup)", unsafe {
                            (self.api.debug_set_run_state)(M64P_DBG_RUNSTATE_PAUSED)
                        }),
                        || unsafe { (self.api.debug_get_state)(M64P_DBG_RUN_STATE) } == M64P_DBG_RUNSTATE_PAUSED)?;
                    self.frame_paused = matches!(stopped, FrameWaitOutcome::Frame(_));
                    self.frozen = true;
                }
                evidence.stop_verified = true;
                for port in &plan.input_ports {
                    self.set_input(&json!({"port":port,"buttons":[]}))?;
                    if !self.held_buttons.is_empty() {
                        return Err(N64Error::BadState(
                            "owned N64 input was not released".into(),
                        ));
                    }
                    evidence.released_ports.insert(*port);
                }
            }
            Ok(())
        })();
        let finished = self.parent_owner()?.finish_cleanup(&plan, evidence);
        if let Err(error) = cleanup {
            return Err(self.stop_generation_with_unresolved_effect("parent cleanup", &error));
        }
        if let Err(error) = finished {
            return Err(self.stop_generation_with_unresolved_effect(
                "parent cleanup",
                &ownership_error(error),
            ));
        }
        Ok(terminal(&plan))
    }
}

impl crate::live::reconnect::owned::OwnedHandler for Mupen64PlusHost {
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
        let result = (|| -> N64Result<Response> {
            let parent_key = |value: Option<&Value>| -> N64Result<OperationKey> {
                serde_json::from_value(
                    value
                        .cloned()
                        .ok_or_else(|| N64Error::BadParams("parent identity is required".into()))?,
                )
                .map_err(|e| N64Error::BadParams(e.to_string()))
            };
            let value = match request.method.as_str() {
                "begin_temporal_operation" => {
                    if cancellation.is_cancelled() {
                        return Err(N64Error::Cancelled);
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
                            if self.display {
                                result["methods"]
                                    .as_array_mut()
                                    .expect("session methods")
                                    .push(json!("cancel_operation"));
                                result["temporal_cancellation_capability"] = json!({
                                    "methods":["step"], "control_service_ms":25, "stop_host_ms":2000
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
        if Self::terminal_reason().is_some() {
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
        Ok(if Self::terminal_reason().is_some() {
            crate::live::reconnect::BridgeDirective::Terminate
        } else {
            crate::live::reconnect::BridgeDirective::Continue
        })
    }
}

impl crate::live::reconnect::owned::OwnedHandler
    for std::sync::Arc<std::sync::Mutex<Mupen64PlusHost>>
{
    fn active_parent_key(&self) -> Option<OperationKey> {
        self.lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_parent_key()
    }
    fn request(
        &mut self,
        attachment: &Attachment,
        request: Request,
        cancellation: RequestCancellation,
    ) -> crate::live::reconnect::BridgeReply {
        self.lock()
            .unwrap_or_else(|e| e.into_inner())
            .request(attachment, request, cancellation)
    }
    fn lifecycle(
        &mut self,
        event: SessionEvent,
    ) -> std::io::Result<crate::live::reconnect::BridgeDirective> {
        self.lock()
            .unwrap_or_else(|e| e.into_inner())
            .lifecycle(event)
    }
}
