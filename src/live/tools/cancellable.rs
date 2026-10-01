//! A composed temporal tool retains the same input and generation cleanup paths as ordinary
//! tools. Parent effects, observations and advances use the admitted cancellation exchange.
use super::*;
use crate::live::link::{AbortRequest, Capabilities, ProgressCallControl, RequestCancellation};
use crate::live::reconnect::cancellation::OperationKey;
use crate::live::temporal::{CancellationCapability, MAX_SYNC_OPERATION_MS};

// Parent cleanup must run even when the originating tool was cancelled.
fn parent_control(
    key: &OperationKey,
    cancellation: RequestCancellation,
    budget: u64,
) -> Result<ProgressCallControl, LinkError> {
    Ok(ProgressCallControl {
        cancellation,
        abort: Some(AbortRequest {
            method: "cancel_operation".into(),
            params: serde_json::to_value(key).map_err(|e| LinkError::Protocol(e.to_string()))?,
        }),
        max_host_ms: Some(budget),
        temporal_stop_ms: Some(budget),
        temporal_deadline: Some(
            std::time::Instant::now() + std::time::Duration::from_millis(budget),
        ),
    })
}

struct ControlledTool<'a> {
    inner: &'a mut dyn EmulatorLink,
    capability: CancellationCapability,
    cancellation: RequestCancellation,
    runtime: String,
    owner: String,
    parent: OperationKey,
    effects_started: bool,
    stop_verified: bool,
    input_issued: bool,
    input_ports: std::collections::BTreeSet<u64>,
    advance_dispatched: bool,
    advances: Vec<Value>,
}
impl<'a> ControlledTool<'a> {
    fn admit(
        inner: &'a mut dyn EmulatorLink,
        method: &str,
        cancellation: RequestCancellation,
        owner: &str,
    ) -> Result<Self, LinkError> {
        let capability = inner
            .capabilities()
            .features
            .temporal_cancellation
            .clone()
            .filter(|c| c.methods.iter().any(|m| m == method))
            .ok_or_else(|| LinkError::Emulator {
                kind: "unsupported".into(),
                message: "temporal cancellation is unavailable for this step method".into(),
            })?;
        let runtime = inner
            .capabilities()
            .identity
            .launch_id
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                LinkError::Protocol(
                    "temporal cancellation requires a verified runtime generation".into(),
                )
            })?;
        OperationKey {
            runtime: runtime.clone(),
            owner_id: owner.into(),
            operation_id: "admission".into(),
        }
        .validate()
        .map_err(|e| LinkError::Protocol(e.to_string()))?;
        if cancellation.is_cancelled() {
            return Err(LinkError::Cancelled);
        }
        let parent = OperationKey {
            runtime: runtime.clone(),
            owner_id: owner.into(),
            operation_id: crate::live::temporal::fresh_identity(),
        };
        inner.begin_temporal_control(&parent)?;
        let parent_value =
            serde_json::to_value(&parent).map_err(|e| LinkError::Protocol(e.to_string()))?;
        let admitted = inner
            .call_with_progress(
                "begin_temporal_operation",
                json!({
                    "parent":parent_value, "_temporal_owner":parent_value
                }),
                &mut |_| Ok(()),
                &parent_control(&parent, cancellation.clone(), capability.stop_host_ms)?,
            )
            .and_then(|reply| {
                if reply["status"] != "admitted"
                    || reply["parent"] != parent_value
                    || inner.capabilities().identity.launch_id.as_deref() != Some(&runtime)
                {
                    return Err(LinkError::Protocol(
                        "producer parent admission was not verified".into(),
                    ));
                }
                Ok(())
            });
        if let Err(error) = admitted {
            // A lost admission reply may leave a live producer owner. Persist uncertainty.
            return super::finish_with_cleanup(
                Err(error),
                inner.finish_temporal_control(&parent, false),
                super::combine_temporal_cleanup_error,
            );
        }
        Ok(Self {
            inner,
            parent,
            effects_started: false,
            stop_verified: false,
            capability,
            cancellation,
            runtime,
            owner: owner.into(),
            input_issued: false,
            input_ports: Default::default(),
            advance_dispatched: false,
            advances: vec![],
        })
    }
    fn check_generation(&self) -> Result<(), LinkError> {
        if self.inner.capabilities().identity.launch_id.as_deref() != Some(&self.runtime) {
            return Err(LinkError::Protocol(
                "temporal runtime generation changed".into(),
            ));
        }
        Ok(())
    }
    fn advance(&mut self, method: &str, mut params: Value) -> Result<Value, LinkError> {
        self.check_generation()?;
        let (field, unit) = if method == "step" {
            ("frames", "frames")
        } else {
            ("count", "instructions")
        };
        let requested = params[field]
            .as_u64()
            .ok_or_else(|| LinkError::Protocol("temporal count missing".into()))?;
        let reply = if self.cancellation.is_cancelled() {
            // A cancellation between phases starts no extra release-edge/trailing advance.
            let status = self.call("status", json!({}))?;
            self.check_generation()?;
            if status["state"] != "frozen" {
                return Err(LinkError::Protocol(
                    "between-phase cancellation did not observe frozen state".into(),
                ));
            }
            let mut stopped = json!({"status":"interrupted","reason":"cancelled","state":"frozen",
                "unit":unit,"count":0,"requested":requested,"frame":status["frame"]});
            if let Some(pending) = status
                .get("event_pending")
                .and_then(Value::as_bool)
                .or_else(|| {
                    status
                        .get("queued_events")
                        .and_then(Value::as_u64)
                        .map(|n| n != 0)
                })
            {
                stopped["event_pending"] = json!(pending);
            }
            stopped
        } else {
            // Two independent ULIDs supply 160 random bits; their time prefix is not used as entropy.
            let operation_id = crate::live::temporal::fresh_identity();
            let key = serde_json::to_value(OperationKey {
                runtime: self.runtime.clone(),
                owner_id: self.owner.clone(),
                operation_id,
            })
            .map_err(|e| LinkError::Protocol(e.to_string()))?;
            params["_control"] = key.clone();
            params["_temporal_owner"] = serde_json::to_value(&self.parent)
                .map_err(|e| LinkError::Protocol(e.to_string()))?;
            let control = ProgressCallControl {
                cancellation: self.cancellation.clone(),
                abort: Some(AbortRequest {
                    method: "cancel_operation".into(),
                    params: key,
                }),
                max_host_ms: Some(MAX_SYNC_OPERATION_MS),
                temporal_stop_ms: Some(self.capability.stop_host_ms),
                temporal_deadline: Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_millis(MAX_SYNC_OPERATION_MS),
                ),
            };
            self.advance_dispatched = true;
            let old_stop = self.stop_verified;
            let old_effects = self.effects_started;
            self.effects_started = true;
            self.stop_verified = false;
            let result = self
                .inner
                .call_with_progress(method, params, &mut |_| Ok(()), &control);
            if matches!(result, Err(LinkError::Cancelled)) {
                // This wire error is reserved for cancellation before dispatch.
                self.stop_verified = old_stop;
                self.effects_started = old_effects;
            }
            result?
        };
        self.check_generation()?;
        let count = reply["count"].as_u64();
        let completed = reply["status"] == "completed";
        let interrupted = reply["status"] == "interrupted" && reply["reason"].as_str().is_some();
        if reply["state"] != "frozen"
            || reply["unit"] != unit
            || !count.is_some_and(|n| n <= requested && (!completed || n == requested))
            || !(completed || interrupted)
        {
            return Err(LinkError::Protocol(
                "invalid temporal terminal boundary or progress".into(),
            ));
        }
        self.stop_verified = true;
        self.advances.push(
            json!({"unit":unit,"requested":requested,"count":count,"status":reply["status"]}),
        );
        Ok(reply)
    }
    fn finish(&mut self, outcome: Result<ToolOutput, LinkError>) -> Result<ToolOutput, LinkError> {
        let producer = self.finish_producer();
        let verified =
            (!self.effects_started || self.stop_verified) && !self.input_issued && producer.is_ok();
        let persisted = super::finish_with_cleanup(
            producer,
            self.inner.finish_temporal_control(&self.parent, verified),
            super::combine_temporal_cleanup_error,
        );
        let cleanup = persisted.and_then(|()| {
            if verified {
                Ok(())
            } else {
                Err(LinkError::Emulator {
                    kind: "temporal_unverified".into(),
                    message:
                        "temporal stop or input release was not verified; runtime is quarantined"
                            .into(),
                })
            }
        });
        let result =
            super::finish_with_cleanup(outcome, cleanup, super::combine_temporal_cleanup_error)?;
        Ok(self.attach_progress(result))
    }
    fn remaining_parent_budget(&mut self) -> Result<u64, LinkError> {
        let budget = self.capability.stop_host_ms;
        if let Some(cancelled) = self.cancellation.cancelled_at() {
            let remaining = (cancelled + std::time::Duration::from_millis(budget))
                .saturating_duration_since(std::time::Instant::now())
                .as_millis() as u64;
            if remaining == 0 {
                // Closing the exact attachment starts producer-side owner-loss cleanup.
                self.inner.prepare_reconnect();
                return Err(LinkError::Emulator {
                    kind: "temporal_unverified".into(),
                    message: "composed cancellation cleanup deadline expired".into(),
                });
            }
            Ok(remaining)
        } else {
            Ok(budget)
        }
    }
    fn parent_request(
        &mut self,
        method: &str,
        mut params: Value,
        cleanup: bool,
    ) -> Result<Value, LinkError> {
        self.check_generation()?;
        let remaining = self.remaining_parent_budget()?;
        params["_temporal_owner"] =
            serde_json::to_value(&self.parent).map_err(|e| LinkError::Protocol(e.to_string()))?;
        let cancellation = if cleanup {
            RequestCancellation::default()
        } else {
            self.cancellation.clone()
        };
        let mut control = parent_control(&self.parent, cancellation, self.capability.stop_host_ms)?;
        control.max_host_ms = Some(remaining);
        if let Some(cancelled) = self.cancellation.cancelled_at() {
            let deadline =
                cancelled + std::time::Duration::from_millis(self.capability.stop_host_ms);
            control.temporal_deadline = Some(control.temporal_deadline.unwrap().min(deadline));
        }
        self.inner
            .call_with_progress(method, params, &mut |_| Ok(()), &control)
    }
    fn finish_producer(&mut self) -> Result<(), LinkError> {
        self.check_generation()?;
        let parent =
            serde_json::to_value(&self.parent).map_err(|e| LinkError::Protocol(e.to_string()))?;
        let reply =
            self.parent_request("finish_temporal_operation", json!({"parent":parent}), true)?;
        self.check_generation()?;
        let ports = reply["released_ports"]
            .as_array()
            .and_then(|values| values.iter().map(Value::as_u64).collect::<Option<Vec<_>>>());
        let exact_ports = ports.is_some_and(|ports| {
            ports.len() == self.input_ports.len()
                && ports.into_iter().collect::<std::collections::BTreeSet<_>>() == self.input_ports
        });
        let effects = reply["effects_started"].as_bool();
        if reply["status"] != "completed"
            || reply["parent"] != parent
            || reply["cleanup_verified"] != true
            || !exact_ports
            || effects.is_none()
            || (self.effects_started && effects != Some(true))
            || (effects == Some(true) && reply["state"] != "frozen")
        {
            return Err(LinkError::Protocol(
                "producer parent cleanup was not verified".into(),
            ));
        }
        Ok(())
    }
    fn attach_progress(&self, mut output: ToolOutput) -> ToolOutput {
        if let ToolOutput::Json(value) = &mut output {
            value["advances"] = json!(self.advances);
        }
        output
    }
}
impl EmulatorLink for ControlledTool<'_> {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        if matches!(method, "step" | "step_instructions") {
            return self.advance(method, params);
        }
        if method == "set_input" {
            let old_input = self.input_issued;
            let old_effects = self.effects_started;
            let old_ports = self.input_ports.clone();
            let release = params["buttons"].as_array().is_some_and(Vec::is_empty);
            if release && !self.input_issued {
                return Ok(json!({"status":"completed"}));
            }
            if !release {
                self.check_generation()?;
                if self.cancellation.is_cancelled() {
                    return Err(LinkError::Cancelled);
                }
                self.input_issued = true; // A lost acknowledgement still requires cleanup.
                self.effects_started = true;
            }
            self.input_ports
                .insert(params.get("port").and_then(Value::as_u64).unwrap_or(0));
            let result = self.parent_request(method, params, release);
            if matches!(result, Err(LinkError::Cancelled)) {
                self.input_issued = old_input;
                self.effects_started = old_effects;
                self.input_ports = old_ports;
            }
            if release && result.is_ok() {
                self.input_issued = false;
            }
            return result;
        }
        let old_effects = self.effects_started;
        let old_stop = self.stop_verified;
        if method == "pause" {
            self.effects_started = true;
            self.stop_verified = false;
        }
        let cleanup = self.cancellation.is_cancelled();
        let result = self.parent_request(method, params, cleanup);
        if matches!(result, Err(LinkError::Cancelled)) {
            self.effects_started = old_effects;
            self.stop_verified = old_stop;
        }
        if matches!(method, "pause" | "status")
            && self.check_generation().is_ok()
            && result.as_ref().is_ok_and(|v| v["state"] == "frozen")
        {
            self.stop_verified = true;
        }
        result
    }
    fn supports_session_reconnect(&self) -> bool {
        false // Parent cleanup is bound to its original attachment.
    }
}

pub fn step_with_cancellation(
    link: &mut dyn EmulatorLink,
    count: u64,
    unit: StepUnit,
    cpu: Option<&str>,
    cancellation: RequestCancellation,
    owner: &str,
) -> Result<ToolOutput, LinkError> {
    let mut scope = ControlledTool::admit(link, unit.capability_method(), cancellation, owner)?;
    let result = super::step(&mut scope, count, unit, cpu);
    let result = if scope.advance_dispatched && !matches!(result, Err(LinkError::Cancelled)) {
        let runtime = scope.runtime.clone();
        super::finish_frozen(&mut scope, Some(&runtime), result)
    } else {
        result
    };
    scope.finish(result)
}

pub fn tap_with_cancellation(
    link: &mut dyn EmulatorLink,
    port: u64,
    buttons: &[String],
    press_frames: u64,
    after_frames: u64,
    cancellation: RequestCancellation,
    owner: &str,
) -> Result<ToolOutput, LinkError> {
    let mut scope = ControlledTool::admit(link, "step", cancellation, owner)?;
    let result = super::tap(&mut scope, port, buttons, press_frames, after_frames);
    scope.finish(result)
}

#[cfg(test)]
mod tests;
