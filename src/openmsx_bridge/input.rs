use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{optional_num, BridgeResult, OpenMsxBridgeError};

pub(super) const INPUT_BUTTONS: &[&str] = &[
    "space", "ctrl", "enter", "esc", "a", "b", "up", "down", "left", "right",
];

pub(super) fn require_port_zero(params: &Value) -> BridgeResult<()> {
    if optional_num(params, "port")?.unwrap_or(0) == 0 {
        Ok(())
    } else {
        Err(OpenMsxBridgeError::BadParams(
            "MSX keyboard injection supports port 0 only".into(),
        ))
    }
}

pub(super) fn normalize_buttons(value: Option<&Value>) -> BridgeResult<BTreeSet<String>> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| OpenMsxBridgeError::BadParams("buttons must be a list".into()))?;
    let mut buttons = BTreeSet::new();
    for value in values {
        let raw = value
            .as_str()
            .ok_or_else(|| OpenMsxBridgeError::BadParams("buttons must contain strings".into()))?;
        let lowercase = raw.to_ascii_lowercase();
        let normalized = match lowercase.as_str() {
            "start" | "return" => "enter",
            "fire1" => "space",
            "fire2" => "ctrl",
            other => other,
        };
        if !INPUT_BUTTONS.contains(&normalized) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "unknown MSX keyboard button `{raw}`; valid: {}",
                INPUT_BUTTONS.join(", ")
            )));
        }
        buttons.insert(normalized.to_string());
    }
    Ok(buttons)
}

pub(super) fn row_masks<'a>(buttons: impl IntoIterator<Item = &'a str>) -> BTreeMap<u8, u8> {
    let mut rows = BTreeMap::new();
    for button in buttons {
        let (row, mask) = button_position(button);
        *rows.entry(row).or_insert(0) |= mask;
    }
    rows
}

pub(super) fn button_position(button: &str) -> (u8, u8) {
    match button {
        "a" => (2, 0x40),
        "b" => (2, 0x80),
        "ctrl" => (6, 0x02),
        "enter" => (7, 0x80),
        "esc" => (7, 0x04),
        "space" => (8, 0x01),
        "left" => (8, 0x10),
        "up" => (8, 0x20),
        "down" => (8, 0x40),
        "right" => (8, 0x80),
        _ => unreachable!("normalized button"),
    }
}
