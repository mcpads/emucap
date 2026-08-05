use super::*;

#[derive(Clone, Debug)]
pub(super) struct MediaDevice {
    id: String,
    kind: String,
    reset_on_load: bool,
    must_be_loaded: bool,
    mounted: bool,
    readonly: bool,
    path: Option<String>,
}

pub(super) struct MediaStatus {
    pub(super) devices: Value,
    pub(super) mounted: Value,
    entries: Vec<MediaDevice>,
}

impl MediaDevice {
    fn capability_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "reset_on_load": self.reset_on_load,
            "must_be_loaded": self.must_be_loaded,
            "supports_runtime_change": !self.reset_on_load,
            "supports_eject": !self.reset_on_load && !self.must_be_loaded,
        })
    }

    fn mounted_json(&self) -> Value {
        let mut value = json!({
            "device": self.id,
            "mounted": self.mounted,
        });
        if self.mounted {
            value["readonly"] = json!(self.readonly);
            value["path"] = json!(self.path);
        }
        value
    }
}

impl<G: GdbTransport> Bridge<G> {
    pub(super) fn media_status(&mut self) -> BridgeResult<MediaStatus> {
        let raw = self.lua_data_cmd_reply("mediastatus", None)?;
        let entries = parse_media_status(&raw)?;
        Ok(MediaStatus {
            devices: Value::Array(entries.iter().map(MediaDevice::capability_json).collect()),
            mounted: Value::Array(entries.iter().map(MediaDevice::mounted_json).collect()),
            entries,
        })
    }

    pub(super) fn change_media(&mut self, params: &Value) -> BridgeResult<Value> {
        if !self.frozen {
            return Err(BridgeError::BadState(
                "change_media requires a frozen emulator; call pause first".into(),
            ));
        }
        let device = required_str(params, "device")?.trim();
        if device.is_empty() {
            return Err(BridgeError::BadParams("device must not be empty".into()));
        }
        let eject = params
            .get("eject")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let requested_path = params.get("path").and_then(Value::as_str);
        if eject == requested_path.is_some() {
            return Err(BridgeError::BadParams(
                "provide exactly one of path or eject=true".into(),
            ));
        }

        let staged = if let Some(path) = requested_path {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(BridgeError::BadParams(
                    "change_media path must be absolute".into(),
                ));
            }
            if !path.is_file() {
                return Err(BridgeError::BadParams(format!(
                    "media image not found: {}",
                    path.display()
                )));
            }
            let path = path.canonicalize()?;
            let sha1 = sha1_file(&path)?;
            if let Some(expected) = params.get("expected_sha1").and_then(Value::as_str) {
                if !expected.eq_ignore_ascii_case(&sha1) {
                    return Err(BridgeError::BadParams(format!(
                        "media SHA-1 mismatch: expected {expected}, got {sha1}"
                    )));
                }
            }
            let path_text = path
                .to_str()
                .ok_or_else(|| BridgeError::BadParams("media path must be valid UTF-8".into()))?;
            Some((path_text.to_string(), sha1, path.metadata()?.len()))
        } else {
            None
        };

        let before = self.media_status()?;
        let previous = before
            .entries
            .iter()
            .find(|entry| entry.id == device)
            .cloned()
            .ok_or_else(|| {
                BridgeError::BadParams(format!(
                    "unknown media device {device}; use status.media_devices"
                ))
            })?;
        if previous.reset_on_load {
            return Err(BridgeError::BadState(format!(
                "media device {device} requires reset on load"
            )));
        }
        if eject && previous.must_be_loaded {
            return Err(BridgeError::BadState(format!(
                "media device {device} must remain loaded"
            )));
        }

        let action = if eject { "eject" } else { "mount" };
        let path = staged.as_ref().map(|entry| entry.0.as_str()).unwrap_or("");
        let spec = format!(
            "{}|{action}|{}",
            hex::encode(device.as_bytes()),
            hex::encode(path.as_bytes())
        );
        let raw = self.lua_data_cmd_reply("mediachange", Some(&spec))?;
        let current = parse_media_change_reply(&raw)?;
        if current.id != device {
            return Err(BridgeError::Emulator(format!(
                "MAME changed unexpected media device {}; requested {device}",
                current.id
            )));
        }
        if eject {
            if current.mounted {
                return Err(BridgeError::Emulator(format!(
                    "MAME reported media change success but {device} remains mounted"
                )));
            }
        } else {
            let expected = staged.as_ref().expect("mount action has staged media");
            let observed = current.path.as_deref().ok_or_else(|| {
                BridgeError::Emulator(format!(
                    "MAME reported media change success but {device} has no mounted path"
                ))
            })?;
            let observed_path = Path::new(observed).canonicalize().map_err(|error| {
                BridgeError::Emulator(format!(
                    "MAME mounted path cannot be verified: {observed}: {error}"
                ))
            })?;
            if observed_path != Path::new(&expected.0) {
                return Err(BridgeError::Emulator(format!(
                    "MAME mounted {}, expected {}",
                    observed_path.display(),
                    expected.0
                )));
            }
        }

        let mut result = json!({
            "status": "completed",
            "action": action,
            "device": device,
            "state": "frozen",
            "previous": previous.mounted_json(),
            "current": current.mounted_json(),
        });
        if let Some((path, sha1, size)) = staged {
            result["media"] = json!({ "path": path, "sha1": sha1, "size": size });
        }
        Ok(result)
    }
}

fn parse_media_status(raw: &str) -> BridgeResult<Vec<MediaDevice>> {
    let payload = raw.strip_prefix("MEDIA:").ok_or_else(|| {
        BridgeError::Emulator(format!("invalid MAME media status response: {raw}"))
    })?;
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    payload.split(';').map(parse_media_descriptor).collect()
}

fn parse_media_change_reply(raw: &str) -> BridgeResult<MediaDevice> {
    if let Some(payload) = raw.strip_prefix("MEDIAOK|") {
        return parse_media_descriptor(payload);
    }
    if let Some(payload) = raw.strip_prefix("MEDIAERR|") {
        let mut fields = payload.splitn(3, '|');
        let reason = decode_text(fields.next().unwrap_or(""), "media error")?;
        let rollback = fields.next().unwrap_or("unknown");
        let current = fields.next().unwrap_or("");
        let current = if current.is_empty() {
            "unavailable".to_string()
        } else {
            parse_media_descriptor(current)?.mounted_json().to_string()
        };
        return Err(BridgeError::Emulator(format!(
            "MAME media change failed: {reason}; rollback={rollback}; current={current}"
        )));
    }
    Err(BridgeError::Emulator(format!(
        "invalid MAME media change response: {raw}"
    )))
}

fn parse_media_descriptor(raw: &str) -> BridgeResult<MediaDevice> {
    let fields = raw.split('|').collect::<Vec<_>>();
    if fields.len() != 7 {
        return Err(BridgeError::Emulator(format!(
            "invalid MAME media descriptor: {raw}"
        )));
    }
    let flag = |index: usize, name: &str| match fields[index] {
        "0" => Ok(false),
        "1" => Ok(true),
        value => Err(BridgeError::Emulator(format!(
            "invalid MAME media {name} flag: {value}"
        ))),
    };
    let path = decode_text(fields[6], "media path")?;
    Ok(MediaDevice {
        id: decode_text(fields[0], "media device")?,
        kind: decode_text(fields[1], "media kind")?,
        reset_on_load: flag(2, "reset_on_load")?,
        must_be_loaded: flag(3, "must_be_loaded")?,
        mounted: flag(4, "mounted")?,
        readonly: flag(5, "readonly")?,
        path: if path.is_empty() { None } else { Some(path) },
    })
}

fn decode_text(raw: &str, label: &str) -> BridgeResult<String> {
    let bytes = hex::decode(raw)
        .map_err(|_| BridgeError::Emulator(format!("invalid hex in MAME {label} response")))?;
    String::from_utf8(bytes)
        .map_err(|_| BridgeError::Emulator(format!("invalid UTF-8 in MAME {label} response")))
}
