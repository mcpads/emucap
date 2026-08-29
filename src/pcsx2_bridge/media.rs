use super::*;

use sha1::{Digest as _, Sha1};

const MEMORY_CARD_SLOTS: usize = 2;
const MAX_MEMORY_CARD_BYTES: u64 = 72 * 1024 * 1024;

const CHANGE_COMPLETED: u32 = 0;
const CHANGE_BUSY: u32 = 1;
const CHANGE_INVALID_SLOT: u32 = 2;
const CHANGE_INVALID_NAME: u32 = 3;
const CHANGE_INVALID_CARD: u32 = 4;
const CHANGE_FAILED_ROLLED_BACK: u32 = 5;
const CHANGE_FAILED_ROLLBACK_FAILED: u32 = 6;

#[derive(Clone, Debug)]
struct MemoryCardSlotStatus {
    index: usize,
    enabled: bool,
    present: bool,
    card_type: u32,
    guest_eject_remaining_frames: u32,
    filename: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct MemoryCardStatus {
    busy_remaining_frames: u32,
    slots: Vec<MemoryCardSlotStatus>,
}

struct StagedMemoryCard {
    filename: String,
    path: PathBuf,
    sha1: String,
    size: u64,
}

pub(super) fn memory_card_devices_json() -> Value {
    Value::Array(
        (0..MEMORY_CARD_SLOTS)
            .map(|index| {
                json!({
                    "id": format!("mcd{}", index + 1),
                    "kind": "memory_card",
                    "reset_on_load": false,
                    "must_be_loaded": false,
                    "supports_runtime_change": true,
                    "supports_eject": true,
                    "writable": true,
                    "managed_root": true,
                    "supported_media": ["ps2_memory_card_file"],
                })
            })
            .collect(),
    )
}

impl MemoryCardStatus {
    fn parse(payload: &[u8]) -> BridgeResult<Self> {
        let mut reader = SliceCursor::new(payload);
        let busy_remaining_frames = reader.u32()?;
        let slot_count = reader.u32()? as usize;
        if slot_count != MEMORY_CARD_SLOTS {
            return Err(Pcsx2BridgeError::Protocol(format!(
                "PCSX2 returned {slot_count} memory-card slots, expected {MEMORY_CARD_SLOTS}"
            )));
        }
        let mut slots = Vec::with_capacity(slot_count);
        for expected_index in 0..slot_count {
            let index = reader.u32()? as usize;
            if index != expected_index {
                return Err(Pcsx2BridgeError::Protocol(format!(
                    "PCSX2 memory-card slot order mismatch: got {index}, expected {expected_index}"
                )));
            }
            let enabled = read_protocol_bool(&mut reader, "enabled")?;
            let present = read_protocol_bool(&mut reader, "present")?;
            let card_type = reader.u32()?;
            if card_type > 2 {
                return Err(Pcsx2BridgeError::Protocol(format!(
                    "PCSX2 returned unknown memory-card type {card_type}"
                )));
            }
            let guest_eject_remaining_frames = reader.u32()?;
            let filename_length = reader.u32()? as usize;
            if filename_length > 255 {
                return Err(Pcsx2BridgeError::Protocol(
                    "PCSX2 memory-card filename exceeds 255 bytes".into(),
                ));
            }
            let filename = if filename_length == 0 {
                None
            } else {
                let raw = reader.bytes(filename_length)?;
                Some(
                    std::str::from_utf8(raw)
                        .map_err(|error| {
                            Pcsx2BridgeError::Protocol(format!(
                                "PCSX2 memory-card filename is not UTF-8: {error}"
                            ))
                        })?
                        .to_owned(),
                )
            };
            slots.push(MemoryCardSlotStatus {
                index,
                enabled,
                present,
                card_type,
                guest_eject_remaining_frames,
                filename,
            });
        }
        if !reader.is_empty() {
            return Err(Pcsx2BridgeError::Protocol(
                "PCSX2 memory-card status has trailing bytes".into(),
            ));
        }
        Ok(Self {
            busy_remaining_frames,
            slots,
        })
    }

    pub(super) fn activity_json(&self) -> Value {
        json!({
            "memory_card_busy": self.busy_remaining_frames > 0,
            "safe_after_frames": self.busy_remaining_frames,
            "unit": "frames",
        })
    }

    pub(super) fn mounted_media_json(&self, root: Option<&Path>) -> BridgeResult<Value> {
        self.slots
            .iter()
            .map(|slot| slot.mounted_json(root))
            .collect::<BridgeResult<Vec<_>>>()
            .map(Value::Array)
    }

    fn slot(&self, index: usize) -> BridgeResult<&MemoryCardSlotStatus> {
        self.slots.get(index).ok_or_else(|| {
            Pcsx2BridgeError::Protocol(format!("PCSX2 omitted memory-card slot {index}"))
        })
    }

    fn guest_transition_remaining_frames(&self) -> u32 {
        self.slots
            .iter()
            .map(|slot| slot.guest_eject_remaining_frames)
            .max()
            .unwrap_or(0)
    }
}

impl MemoryCardSlotStatus {
    fn mounted_json(&self, root: Option<&Path>) -> BridgeResult<Value> {
        let mut value = json!({
            "device": format!("mcd{}", self.index + 1),
            "mounted": self.present,
            "enabled": self.enabled,
            "readonly": false,
            "guest_present": self.present && self.guest_eject_remaining_frames == 0,
            "media_type": match self.card_type {
                0 => "empty",
                1 => "file",
                2 => "folder",
                _ => unreachable!("validated memory-card type"),
            },
        });
        if let Some(filename) = self.filename.as_deref() {
            value["filename"] = json!(filename);
            // Runtime replacement supports regular file cards only. Existing folder cards and
            // configured-but-unavailable files must remain observable without making `status`
            // depend on file-card path validation.
            if self.present && self.card_type == 1 {
                if let Some(root) = root {
                    value["path"] = json!(managed_card_path(root, filename)?.display().to_string());
                }
            } else if self.card_type == 2 {
                value["runtime_change_supported"] = json!(false);
            }
        }
        if self.present && self.guest_eject_remaining_frames > 0 {
            value["guest_transition"] = json!({
                "state": "pending_reinsert",
                "target": "present",
                "remaining_frames": self.guest_eject_remaining_frames,
                "unit": "frames",
            });
        }
        Ok(value)
    }
}

impl<T: PineTransport> Pcsx2Bridge<T> {
    pub(super) fn memory_card_status(&mut self) -> BridgeResult<MemoryCardStatus> {
        let payload = self.command(MSG_EMUCAP_MEMORY_CARD_STATUS, &[])?;
        MemoryCardStatus::parse(&payload)
    }

    pub(super) fn change_media(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("change_media")?;
        let device = required_str(params, "device")?;
        let slot_index = match device {
            "mcd1" => 0,
            "mcd2" => 1,
            _ => {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "unknown media device {device}; use status.media_devices"
                )))
            }
        };
        let eject = params
            .get("eject")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let requested_path = params.get("path").and_then(Value::as_str);
        if eject == requested_path.is_some() {
            return Err(Pcsx2BridgeError::BadParams(
                "provide exactly one of path or eject=true".into(),
            ));
        }

        let before = self.memory_card_status()?;
        if before.busy_remaining_frames > 0 {
            return Err(Pcsx2BridgeError::BadState(format!(
                "memory cards are busy; advance {} guest frames before retrying",
                before.busy_remaining_frames
            )));
        }
        let transition_remaining = before.guest_transition_remaining_frames();
        if transition_remaining > 0 {
            return Err(Pcsx2BridgeError::BadState(format!(
                "memory-card guest transition is pending; advance {transition_remaining} guest frames before retrying"
            )));
        }
        let previous_slot = before.slot(slot_index)?;
        if previous_slot.card_type == 2 {
            return Err(Pcsx2BridgeError::Unsupported(format!(
                "runtime replacement does not support the folder card in {device}"
            )));
        }
        let previous = previous_slot.mounted_json(self.memory_card_dir.as_deref())?;
        let staged = requested_path
            .map(|path| self.stage_memory_card(path, params))
            .transpose()?;

        let filename = staged
            .as_ref()
            .map(|card| card.filename.as_bytes())
            .unwrap_or_default();
        let mut request = Vec::with_capacity(12 + filename.len());
        request.extend_from_slice(&(slot_index as u32).to_le_bytes());
        request.extend_from_slice(&(u32::from(!eject)).to_le_bytes());
        request.extend_from_slice(&(filename.len() as u32).to_le_bytes());
        request.extend_from_slice(filename);
        let payload = self.command(MSG_EMUCAP_CHANGE_MEMORY_CARD, &request)?;
        if payload.len() < 4 {
            return Err(Pcsx2BridgeError::Protocol(
                "PCSX2 memory-card change reply is shorter than its outcome".into(),
            ));
        }
        let outcome = u32::from_le_bytes(payload[..4].try_into().expect("four bytes"));
        let after = MemoryCardStatus::parse(&payload[4..])?;
        let current_slot = after.slot(slot_index)?;
        let current = current_slot.mounted_json(self.memory_card_dir.as_deref())?;

        match outcome {
            CHANGE_COMPLETED => {}
            CHANGE_BUSY => {
                let transition_remaining = after.guest_transition_remaining_frames();
                let wait = after.busy_remaining_frames.max(transition_remaining);
                if wait == 0 {
                    return Err(Pcsx2BridgeError::Protocol(
                        "PCSX2 rejected a memory-card change as busy without reporting a wait"
                            .into(),
                    ));
                }
                return Err(Pcsx2BridgeError::BadState(format!(
                    "memory-card change became unsafe before mutation; safe_after_frames={wait}"
                )));
            }
            CHANGE_INVALID_SLOT => {
                return Err(Pcsx2BridgeError::Protocol(
                    "PCSX2 rejected the validated memory-card slot".into(),
                ))
            }
            CHANGE_INVALID_NAME | CHANGE_INVALID_CARD => {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "PCSX2 rejected the memory-card file; current={current}"
                )))
            }
            CHANGE_FAILED_ROLLED_BACK => {
                return Err(Pcsx2BridgeError::Emulator(format!(
                    "PCSX2 failed to change memory card and restored the previous slot; current={current}"
                )))
            }
            CHANGE_FAILED_ROLLBACK_FAILED => {
                return Err(Pcsx2BridgeError::Emulator(format!(
                    "PCSX2 failed to change memory card and rollback failed; current={current}"
                )))
            }
            value => {
                return Err(Pcsx2BridgeError::Protocol(format!(
                    "PCSX2 returned unknown memory-card change outcome {value}"
                )))
            }
        }

        if eject {
            if current_slot.present || current_slot.filename.is_some() {
                return Err(Pcsx2BridgeError::Emulator(format!(
                    "PCSX2 reported eject completion but {device} remains mounted; current={current}"
                )));
            }
        } else if !current_slot.present
            || current_slot.filename.as_deref()
                != staged.as_ref().map(|card| card.filename.as_str())
        {
            return Err(Pcsx2BridgeError::Emulator(format!(
                "PCSX2 reported mount completion without the requested card; current={current}"
            )));
        }

        let mut result = json!({
            "status": "completed",
            "action": if eject { "eject" } else { "mount" },
            "device": device,
            "state": "frozen",
            "guest_frames_advanced": 0,
            "previous": previous,
            "current": current,
        });
        if let Some(card) = staged {
            result["media"] = json!({
                "path": card.path.display().to_string(),
                "sha1_at_attach": card.sha1,
                "size": card.size,
                "writable": true,
            });
        }
        if let Some(transition) = result["current"].get("guest_transition").cloned() {
            result["guest_transition"] = transition;
        }
        Ok(result)
    }

    fn stage_memory_card(&self, requested: &str, params: &Value) -> BridgeResult<StagedMemoryCard> {
        let root = self.memory_card_dir.as_deref().ok_or_else(|| {
            Pcsx2BridgeError::Unsupported(
                "change_media requires the managed PCSX2 launch path".into(),
            )
        })?;
        let requested = Path::new(requested);
        if !requested.is_absolute() {
            return Err(Pcsx2BridgeError::BadParams(
                "change_media path must be absolute".into(),
            ));
        }
        let filename = requested
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Pcsx2BridgeError::BadParams(
                    "memory-card path must end in a valid UTF-8 filename".into(),
                )
            })?;
        if !crate::path_safety::is_portable_file_name(filename, 255)
            || !filename.to_ascii_lowercase().ends_with(".ps2")
        {
            return Err(Pcsx2BridgeError::BadParams(
                "PCSX2 runtime insertion supports one portable .ps2 card file in the managed memory-card directory"
                    .into(),
            ));
        }
        let requested_canonical = requested.canonicalize().map_err(|error| {
            Pcsx2BridgeError::BadParams(format!(
                "memory-card file cannot be resolved: {}: {error}",
                requested.display()
            ))
        })?;
        let canonical_root = root.canonicalize().map_err(|error| {
            Pcsx2BridgeError::BadParams(format!(
                "managed memory-card directory cannot be resolved: {}: {error}",
                root.display()
            ))
        })?;
        if requested_canonical.parent() != Some(canonical_root.as_path()) {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "memory-card file must be a direct member of the managed directory: {}",
                root.display()
            )));
        }
        let path = managed_card_path(root, filename)?;
        if requested_canonical != path {
            return Err(Pcsx2BridgeError::BadParams(
                "memory-card path changed while it was being validated".into(),
            ));
        }
        let mut file = crate::path_safety::open_regular_member_no_follow(root, filename)
            .map_err(|error| Pcsx2BridgeError::BadParams(error.to_string()))?;
        let size = file.metadata()?.len();
        if size > MAX_MEMORY_CARD_BYTES {
            return Err(Pcsx2BridgeError::BadParams(format!(
                "memory-card file exceeds the {MAX_MEMORY_CARD_BYTES}-byte safety bound"
            )));
        }
        let mut hasher = Sha1::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let sha1 = hex::encode(hasher.finalize());
        if let Some(expected) = params.get("expected_sha1").and_then(Value::as_str) {
            if expected.len() != 40
                || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !expected.eq_ignore_ascii_case(&sha1)
            {
                return Err(Pcsx2BridgeError::BadParams(format!(
                    "memory-card SHA-1 mismatch: expected {expected}, got {sha1}"
                )));
            }
        }
        Ok(StagedMemoryCard {
            filename: filename.to_owned(),
            path,
            sha1,
            size,
        })
    }
}

fn managed_card_path(root: &Path, filename: &str) -> BridgeResult<PathBuf> {
    crate::path_safety::regular_member_path(root, filename)
        .map_err(|error| Pcsx2BridgeError::BadParams(error.to_string()))
}

fn read_protocol_bool(reader: &mut SliceCursor<'_>, field: &str) -> BridgeResult<bool> {
    match reader.u32()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(Pcsx2BridgeError::Protocol(format!(
            "PCSX2 memory-card {field} flag is {value}, expected 0 or 1"
        ))),
    }
}
