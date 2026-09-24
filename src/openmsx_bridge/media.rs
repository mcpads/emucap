//! Frozen drive-A replacement and recovery of guest-written sector bytes.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use super::{
    disk_state::StateScratch, tcl_utf8_value, BridgeResult, MediaKind, OpenMsxBridge,
    OpenMsxBridgeError, OpenMsxControl,
};

const MAX_DISK: u64 = 64 * 1024 * 1024;

fn image_bytes(path: &Path) -> BridgeResult<Vec<u8>> {
    let bytes = crate::path_safety::read_bounded_regular_file_no_follow(path, MAX_DISK)?;
    if bytes.is_empty() || !bytes.len().is_multiple_of(512) {
        return Err(OpenMsxBridgeError::BadParams(
            "expected a nonempty raw sector image".into(),
        ));
    }
    Ok(bytes)
}

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub(super) fn get_rom_info(&self) -> BridgeResult<Value> {
        Ok(json!({
            "system": self.session.system,
            "machine": self.session.machine,
            "machine_type": self.session.machine_type,
            "media": self.session.media.kind.as_str(),
            "path": self.session.media.source_path.display().to_string(),
            "sha1": self.session.media.source_sha1,
            "size": self.session.media.source_size,
            "mounted_path": if self.disk_ejected { None } else { Some(self.session.media.mounted_path.display().to_string()) },
            "firmware_manifest_sha256": self.session.firmware_manifest_sha256,
        }))
    }

    pub(super) fn media_devices(&self) -> Value {
        if self.session.media.kind != MediaKind::Disk {
            return json!([]);
        }
        json!([{"id":"diska", "kind":"floppy", "reset_on_load":false,
            "must_be_loaded":false, "supports_runtime_change":true, "supports_eject":true,
            "notes":"Insert imports the given bytes into a private copy. Reinsert previous.path to retain guest writes; copy it outside the generation before stop. Save states require an inserted disk."}])
    }

    pub(super) fn mounted_media(&mut self) -> BridgeResult<Value> {
        if self.session.media.kind != MediaKind::Disk {
            return Ok(json!([]));
        }
        self.require_runtime_identity("media status")?;
        Ok(json!([{"device":"diska", "mounted":!self.disk_ejected,
            "path":if self.disk_ejected { None } else { Some(&self.session.media.mounted_path) },
            "ownership":"generation", "identity_scope":"live mount; launch source identity is unchanged"}]))
    }

    /// Independent sector export: safe to copy or reinsert even after a later state load.
    pub(super) fn preserve_disk(&mut self) -> BridgeResult<Value> {
        if self.session.media.kind != MediaKind::Disk || self.disk_ejected {
            return Ok(json!({"device":"diska", "mounted":false}));
        }
        let mut scratch = StateScratch::new(&self.session)?;
        self.control.command(&format!(
            "diskmanipulator savedsk diska {}",
            tcl_utf8_value(&scratch.disk())?
        ))?;
        let bytes = image_bytes(&scratch.disk())?;
        let value = json!({"device":"diska", "mounted":true, "path":scratch.disk(),
            "sha1":hex::encode(Sha1::digest(&bytes)), "size":bytes.len(),
            "ownership":"generation", "bytes":"native sector export including guest writes"});
        scratch.retain();
        Ok(value)
    }

    pub(super) fn change_media(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("change_media")?;
        if params.get("device").and_then(Value::as_str) != Some("diska") {
            return Err(OpenMsxBridgeError::BadParams(
                "use device diska from status.media_devices".into(),
            ));
        }
        let eject = match params.get("eject") {
            None => false,
            Some(Value::Bool(value)) => *value,
            _ => {
                return Err(OpenMsxBridgeError::BadParams(
                    "eject must be boolean".into(),
                ))
            }
        };
        let path = params.get("path").and_then(Value::as_str);
        if eject == path.is_some() || (eject && params.get("expected_sha1").is_some()) {
            return Err(OpenMsxBridgeError::BadParams(
                "provide exactly one of path or eject=true; expected_sha1 applies to insertion"
                    .into(),
            ));
        }
        let mut staged = None;
        let mut inserted = Value::Null;
        if let Some(path) = path {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(OpenMsxBridgeError::BadParams(
                    "media path must be absolute".into(),
                ));
            }
            let bytes = image_bytes(&path)?;
            let digest = hex::encode(Sha1::digest(&bytes));
            if let Some(expected) = params.get("expected_sha1") {
                if !expected
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&digest))
                {
                    return Err(OpenMsxBridgeError::BadParams(
                        "media SHA-1 precondition failed".into(),
                    ));
                }
            }
            let scratch = StateScratch::new(&self.session)?;
            fs::write(scratch.disk(), &bytes)?;
            inserted = json!({"path":scratch.disk(), "source_path":path, "sha1":digest,
                "size":bytes.len(), "identity_scope":"inserted bytes"});
            staged = Some(scratch);
        }
        self.require_runtime_identity("change_media preflight")?;
        let previous = self.preserve_disk()?;
        let frame = self.current_frame()?;
        let command = if let Some(scratch) = &staged {
            format!("diska insert {}", tcl_utf8_value(&scratch.disk())?)
        } else {
            "diska eject".into()
        };
        if let Err(primary) = self.control.command(&command) {
            // Native insertion constructs the new Disk before swapping ownership.
            // Readback proves whether that strong exception guarantee still holds.
            let verified = self
                .require_stop_conjunction("change_media rejection")
                .and_then(|()| self.require_runtime_identity("change_media rejection"))
                .and_then(|()| {
                    if self.current_frame()? == frame {
                        Ok(())
                    } else {
                        Err(OpenMsxBridgeError::Emulator("frame changed".into()))
                    }
                });
            if let Err(error) = verified {
                if let Some(scratch) = &mut staged {
                    scratch.retain();
                }
                return self.fail_debugger(format!(
                    "media change outcome unverified: {primary}; current_media=unverified; rollback_failure={error}; previous={previous}"
                ));
            }
            return Err(OpenMsxBridgeError::Emulator(format!(
                "media change rejected; original retained: {primary}; previous={previous}"
            )));
        }
        if let Some(scratch) = &mut staged {
            self.session.media.mounted_path = scratch.disk();
            scratch.retain();
        }
        self.disk_ejected = eject;
        let verified = self
            .require_stop_conjunction("change_media")
            .and_then(|()| self.require_runtime_identity("change_media"))
            .and_then(|()| {
                if self.current_frame()? == frame {
                    Ok(())
                } else {
                    Err(OpenMsxBridgeError::Emulator(
                        "media change advanced guest frame".into(),
                    ))
                }
            });
        if let Err(error) = verified {
            return self.fail_debugger(format!(
                "media change readback failed; current_media=unverified; rollback_failure={error}; previous={previous}"
            ));
        }
        Ok(
            json!({"status":"completed", "state":"frozen", "frame":frame,
            "device":"diska", "action":if eject {"eject"} else {"mount"},
            "previous":previous, "current":self.mounted_media()?[0], "media":inserted}),
        )
    }
}
