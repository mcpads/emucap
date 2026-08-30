use super::*;

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn get_rom_info(&self) -> XemuResult<Value> {
        let content = self
            .current_disc
            .as_ref()
            .ok_or_else(|| XemuBridgeError::BadState("no Xbox disc is currently mounted".into()))?;
        let (size, sha1) = sha1_regular_file(content)?;
        Ok(json!({
            "system":"xbox", "adapter":"xemu-rust-qmp-gdb",
            "name":content.file_name().and_then(|name| name.to_str()).unwrap_or(""),
            "path":content.display().to_string(), "sha1":sha1, "size":size,
            "media_type":content.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase(),
            "machine_inputs": self.machine_identity.value(),
        }))
    }

    pub(super) fn change_media(&mut self, params: &Value) -> XemuResult<Value> {
        let device = params
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("dvd");
        if !matches!(device, "dvd" | "dvd0" | "disc") {
            return Err(XemuBridgeError::BadParams(format!(
                "unsupported Xbox media device: {device}; valid: dvd"
            )));
        }
        if self.is_running()? {
            return Err(XemuBridgeError::BadState(
                "Xbox change_media requires a frozen VM; pause first".into(),
            ));
        }
        let previous = self
            .current_disc
            .as_ref()
            .map(|path| path.display().to_string());
        if params
            .get("eject")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.qmp.execute("xemu-emucap-eject-disc", None)?;
            self.current_disc = None;
            self.state_media_identity_cache = None;
            return Ok(json!({
                "status":"completed", "state":"frozen", "device":"dvd",
                "action":"eject", "previous":previous, "current":null
            }));
        }

        let path = PathBuf::from(required_str(params, "path")?);
        if !path.is_absolute() {
            return Err(XemuBridgeError::BadParams(
                "Xbox replacement disc path must be absolute".into(),
            ));
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "iso" | "xiso") {
            return Err(XemuBridgeError::BadParams(
                "Xbox replacement media must be a raw .iso or .xiso file".into(),
            ));
        }
        let (size, sha1) = sha1_regular_file(&path)?;
        if let Some(expected) = params.get("expected_sha1").and_then(Value::as_str) {
            if !expected.eq_ignore_ascii_case(&sha1) {
                return Err(XemuBridgeError::BadParams(format!(
                    "replacement disc SHA-1 mismatch: expected {expected}, got {sha1}"
                )));
            }
        }
        self.qmp.execute(
            "xemu-emucap-load-disc",
            Some(json!({"path":path.display().to_string()})),
        )?;
        self.current_disc = Some(path.clone());
        self.state_media_identity_cache = None;
        Ok(json!({
            "status":"completed", "state":"frozen", "device":"dvd", "action":"mount",
            "previous":previous,
            "current":{"path":path.display().to_string(), "sha1":sha1, "size":size, "readonly":true}
        }))
    }
}
