use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{optional_num, BridgeResult, OpenMsxBridgeError};

pub(super) const KEYBOARD_BUTTONS: &[&str] = &[
    "space", "ctrl", "enter", "esc", "a", "b", "up", "down", "left", "right",
];
pub(super) const JOYSTICK_BUTTONS: &[&str] = &["up", "down", "left", "right", "a", "b"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputPort {
    Keyboard,
    Joystick(usize),
}

pub(super) fn input_port(params: &Value) -> BridgeResult<InputPort> {
    match optional_num(params, "port")?.unwrap_or(0) {
        0 => Ok(InputPort::Keyboard),
        1 => Ok(InputPort::Joystick(0)),
        2 => Ok(InputPort::Joystick(1)),
        port => Err(OpenMsxBridgeError::BadParams(format!(
            "MSX input port must be 0 (keyboard), 1, or 2; got {port}"
        ))),
    }
}

fn button_list(value: Option<&Value>) -> BridgeResult<Vec<&str>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| OpenMsxBridgeError::BadParams("buttons must be a list".into()))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| OpenMsxBridgeError::BadParams("buttons must contain strings".into()))
        })
        .collect()
}

pub(super) fn normalize_keyboard_buttons(value: Option<&Value>) -> BridgeResult<BTreeSet<String>> {
    let mut buttons = BTreeSet::new();
    for raw in button_list(value)? {
        let lowercase = raw.to_ascii_lowercase();
        let normalized = match lowercase.as_str() {
            "start" | "return" => "enter",
            "fire1" => "space",
            "fire2" => "ctrl",
            other => other,
        };
        if !KEYBOARD_BUTTONS.contains(&normalized) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "unknown MSX keyboard button `{raw}`; valid: {}",
                KEYBOARD_BUTTONS.join(", ")
            )));
        }
        buttons.insert(normalized.to_string());
    }
    Ok(buttons)
}

pub(super) fn normalize_joystick_buttons(value: Option<&Value>) -> BridgeResult<BTreeSet<String>> {
    let mut buttons = BTreeSet::new();
    for raw in button_list(value)? {
        let lowercase = raw.to_ascii_lowercase();
        let normalized = match lowercase.as_str() {
            "fire1" | "button1" => "a",
            "fire2" | "button2" => "b",
            other => other,
        };
        if !JOYSTICK_BUTTONS.contains(&normalized) {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "unknown MSX joystick button `{raw}`; valid: {}",
                JOYSTICK_BUTTONS.join(", ")
            )));
        }
        buttons.insert(normalized.to_string());
    }
    if (buttons.contains("up") && buttons.contains("down"))
        || (buttons.contains("left") && buttons.contains("right"))
    {
        return Err(OpenMsxBridgeError::BadParams(
            "MSX joystick input cannot hold opposite directions".into(),
        ));
    }
    Ok(buttons)
}

pub(super) fn joystick_mask(buttons: &BTreeSet<String>) -> u8 {
    let mut mask = 0x3f;
    for button in buttons {
        let bit = match button.as_str() {
            "up" => 0,
            "down" => 1,
            "left" => 2,
            "right" => 3,
            "a" => 4,
            "b" => 5,
            _ => unreachable!("normalized joystick button"),
        };
        mask &= !(1 << bit);
    }
    mask
}

pub(super) fn joystick_buttons(mask: u8) -> Vec<&'static str> {
    JOYSTICK_BUTTONS
        .iter()
        .enumerate()
        .filter_map(|(bit, button)| (mask & (1 << bit) == 0).then_some(*button))
        .collect()
}

pub(super) fn joystick_mask_has_opposites(mask: u8) -> bool {
    mask & 0x03 == 0 || mask & 0x0c == 0
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
