//! Native parent cleanup; the transport must authenticate the supplied attachment.
use super::*;
use crate::live::{
    control_session::{Attachment, EventKind, SessionEvent},
    link::RequestCancellation,
    reconnect::cancellation::OperationKey,
    temporal::owner::{CleanupEvidence, CleanupPlan, OwnershipError, ProducerOwnership},
};
use std::time::{Duration, Instant};

fn ownership_error(error: OwnershipError) -> OpenMsxBridgeError {
    match error {
        OwnershipError::Busy => OpenMsxBridgeError::Busy,
        _ => OpenMsxBridgeError::BadState(error.to_string()),
    }
}
fn terminal(plan: &CleanupPlan) -> Value {
    let mut value = json!({"status":"completed","parent":plan.key,"cleanup_verified":true,"released_ports":plan.input_ports,"effects_started":plan.effects_started});
    if plan.effects_started {
        value["state"] = json!("frozen");
    }
    value
}
impl<C: OpenMsxControl> OpenMsxBridge<C> {
    fn parent_owner(&mut self) -> BridgeResult<&mut ProducerOwnership> {
        self.producer_ownership.as_mut().ok_or_else(|| {
            OpenMsxBridgeError::BadState("producer attachment is not admitted".into())
        })
    }
    pub fn apply_control_session(&mut self, event: SessionEvent) -> BridgeResult<()> {
        if self.launch_id.as_deref() != Some(&event.runtime) {
            return Err(OpenMsxBridgeError::BadState(
                "control lifecycle runtime mismatch".into(),
            ));
        }
        event
            .attachment
            .validate()
            .map_err(|e| OpenMsxBridgeError::BadParams(e.into()))?;
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
        self.require_debugger_healthy()?;
        self.parent_owner()?
            .begin(attachment, key)
            .map_err(ownership_error)
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
        self.require_debugger_healthy()?;
        if cancellation.is_cancelled() {
            return Err(OpenMsxBridgeError::Cancelled);
        }
        if crate::live::continuity::is_read_only(&request.method) {
            self.parent_owner()?
                .authorize_observation(attachment, key)
                .map_err(ownership_error)?;
            return Ok(self.handle_request_cancellable(request, cancellation));
        }
        let input = match request.method.as_str() {
            "set_input" => Some(match input_port(&request.params)? {
                InputPort::Keyboard => {
                    normalize_keyboard_buttons(request.params.get("buttons"))?;
                    0
                }
                InputPort::Joystick(index) => {
                    normalize_joystick_buttons(request.params.get("buttons"))?;
                    (index + 1) as u64
                }
            }),
            "pause" => None,
            "step" => {
                let child: OperationKey = serde_json::from_value(
                    request.params.get("_control").cloned().ok_or_else(|| {
                        OpenMsxBridgeError::BadParams("controlled step identity is required".into())
                    })?,
                )
                .map_err(|e| OpenMsxBridgeError::BadParams(e.to_string()))?;
                child
                    .validate()
                    .map_err(|e| OpenMsxBridgeError::BadParams(e.to_string()))?;
                if child.runtime != key.runtime
                    || child.owner_id != key.owner_id
                    || child.operation_id == key.operation_id
                {
                    return Err(OpenMsxBridgeError::BadParams(
                        "child identity does not belong to parent".into(),
                    ));
                }
                None
            }
            _ => {
                return Err(OpenMsxBridgeError::Unsupported(
                    "method is outside the openMSX parent operation".into(),
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
    fn clean_parent(&mut self, plan: CleanupPlan) -> BridgeResult<Value> {
        let mut evidence = CleanupEvidence::default();
        let mut failure = None;
        self.control
            .set_command_deadline(Some(Instant::now() + Duration::from_millis(500)));
        if plan.effects_started {
            let stopped = self
                .control
                .command("::emucap::cancel_frame")
                .and_then(|_| self.control.command("set pause on"))
                .and_then(|_| self.control.command("debug break"))
                .and_then(|_| self.require_stop_conjunction("parent cleanup"));
            match stopped {
                Ok(()) => evidence.stop_verified = true,
                Err(error) => failure = Some(error.to_string()),
            }
            for port in &plan.input_ports {
                let released = self
                    .set_input(&json!({"port":port,"buttons":[]}))
                    .and_then(|_| self.verify_released_port(*port));
                match released {
                    Ok(()) => {
                        evidence.released_ports.insert(*port);
                    }
                    Err(error) => {
                        failure.get_or_insert_with(|| error.to_string());
                    }
                }
            }
        }
        self.control.set_command_deadline(None);
        let finished = self.parent_owner()?.finish_cleanup(&plan, evidence);
        if let Some(error) = failure {
            return self.fail_debugger(format!("parent cleanup failed: {error}"));
        }
        if let Err(error) = finished {
            return self.fail_debugger(format!("parent cleanup unverified: {error}"));
        }
        Ok(terminal(&plan))
    }
    fn verify_released_port(&mut self, port: u64) -> BridgeResult<()> {
        let observed = input::parse_input_observation(
            &self.control.command(input::INPUT_OBSERVATION_COMMAND)?,
        )?;
        let released = match port {
            0 => row_masks(KEYBOARD_BUTTONS.iter().copied())
                .into_iter()
                .all(|(row, mask)| {
                    observed.matrix[row.to_string()]
                        .as_u64()
                        .is_some_and(|value| value & mask as u64 == mask as u64)
                }),
            1 | 2 => observed.owners[(port - 1) as usize].is_none(),
            _ => false,
        };
        if released {
            Ok(())
        } else {
            Err(OpenMsxBridgeError::Emulator(format!(
                "input port {port} release did not read back"
            )))
        }
    }
}

impl<C: OpenMsxControl + Send> crate::live::reconnect::owned::OwnedHandler for OpenMsxBridge<C> {
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
                    OpenMsxBridgeError::BadParams("parent identity is required".into())
                })?)
                .map_err(|e| OpenMsxBridgeError::BadParams(e.to_string()))
            };
            let value = match request.method.as_str() {
                "begin_temporal_operation" => {
                    if cancellation.is_cancelled() {
                        return Err(OpenMsxBridgeError::Cancelled);
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
                            // Native pipe I/O has bounded cancellation on Unix. Advertise
                            // only through the dispatcher that also owns parent cleanup.
                            #[cfg(unix)]
                            {
                                result["methods"]
                                    .as_array_mut()
                                    .expect("session methods")
                                    .push(json!("cancel_operation"));
                                result["temporal_cancellation_capability"] = json!({
                                    "methods": ["step"],
                                    "control_service_ms": 25,
                                    "stop_host_ms": 1000
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
