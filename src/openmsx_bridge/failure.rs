use super::*;

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub(super) fn persist_debugger_failure(&mut self, operation: &str) -> BridgeResult<()> {
        let Some(path) = self.failure_file.clone() else {
            return Ok(());
        };
        let launch_id = self.launch_id.clone().ok_or_else(|| {
            OpenMsxBridgeError::BadState(
                "failure artifact requires a managed launch identity".into(),
            )
        })?;
        let reason = self
            .debugger_fatal
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(4096)
            .collect::<String>();
        // These observations do not resume or repair the poisoned debugger. Do
        // not manufacture a frame or frozen assertion if native reads fail.
        let frame = self.current_frame()?;
        let execution_state = match self.query_stop_state() {
            Ok((true, true)) => "frozen",
            Ok((false, false)) => "running",
            _ => "unknown",
        };
        let observed_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| OpenMsxBridgeError::BadState(e.to_string()))?
            .as_millis() as u64;
        let artifact = json!({
            "schema_version":1, "launch_id":launch_id,
            "adapter":"openmsx-rust-xml", "kind":"adapter_internal_error",
            "operation":operation, "reason":reason, "active":true,
            "observed_at_unix_ms":observed_at_unix_ms, "frame":frame,
            "execution_state":execution_state,
            "disposition":"terminate_debugger_generation",
        });
        crate::path_safety::atomic_write_file(&path, artifact.to_string().as_bytes())?;
        Ok(())
    }
}
