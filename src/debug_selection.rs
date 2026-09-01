use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuTarget {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub default: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disassembly_modes: Vec<String>,
}

pub fn parse_debug_capabilities(hello: &Value) -> Result<(Vec<String>, Vec<CpuTarget>), String> {
    let state_groups = parse_name_list(hello.get("state_groups"), "state_groups")?;
    let Some(value) = hello.get("cpu_targets") else {
        return Ok((state_groups, Vec::new()));
    };
    let values = value
        .as_array()
        .ok_or_else(|| "cpu_targets must be an array".to_string())?;
    let mut targets = Vec::with_capacity(values.len());
    let mut names = BTreeSet::new();
    let mut default_count = 0usize;
    for value in values {
        let target: CpuTarget = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid cpu_targets entry: {error}"))?;
        validate_token(&target.id, "cpu target id")?;
        if target.default {
            default_count += 1;
        }
        for name in std::iter::once(&target.id).chain(target.aliases.iter()) {
            validate_token(name, "cpu target name")?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(format!(
                    "cpu target name is advertised more than once: {name}"
                ));
            }
        }
        let mut modes = BTreeSet::new();
        for mode in &target.disassembly_modes {
            validate_token(mode, "disassembly mode")?;
            if !modes.insert(mode.to_ascii_lowercase()) {
                return Err(format!(
                    "disassembly mode is advertised more than once for {}: {mode}",
                    target.id
                ));
            }
        }
        targets.push(target);
    }
    if !targets.is_empty() && default_count != 1 {
        return Err(format!(
            "cpu_targets must contain exactly one default target, found {default_count}"
        ));
    }
    Ok((state_groups, targets))
}

pub fn resolve_cpu_target(
    targets: &[CpuTarget],
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    if targets.is_empty() {
        return Ok(Some(requested.to_string()));
    }
    targets
        .iter()
        .find(|target| {
            target.id.eq_ignore_ascii_case(requested)
                || target
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(requested))
        })
        .map(|target| Some(target.id.clone()))
        .ok_or_else(|| {
            format!(
                "unknown cpu target '{requested}'; available: {}",
                targets
                    .iter()
                    .map(|target| target.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

pub fn resolve_disassembly_mode(
    targets: &[CpuTarget],
    selected_cpu: Option<&str>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    if targets.is_empty() {
        return Ok(Some(requested.to_string()));
    }
    let target = match selected_cpu {
        Some(cpu) => targets.iter().find(|target| target.id == cpu),
        None => targets.iter().find(|target| target.default),
    }
    .ok_or_else(|| "the selected cpu target is not advertised".to_string())?;
    target
        .disassembly_modes
        .iter()
        .find(|mode| mode.eq_ignore_ascii_case(requested))
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            format!(
                "unknown disassembly mode '{requested}' for cpu target '{}'; available: {}",
                target.id,
                target.disassembly_modes.join(", ")
            )
        })
}

pub fn project_state_groups(
    mut response: Value,
    requested: &[String],
    advertised: &[String],
) -> Result<Value, String> {
    if requested.is_empty() {
        return Ok(response);
    }
    let discovered = if advertised.is_empty() {
        discover_state_groups(&response)?
    } else {
        advertised.to_vec()
    };
    let applied = resolve_group_names(requested, &discovered)?;
    let object = response
        .as_object_mut()
        .ok_or_else(|| "get_state response must be an object".to_string())?;
    let has_cpu_identity = object.get("cpu").and_then(Value::as_str).is_some();

    if let Some(state) = object.get_mut("state") {
        let state = state
            .as_object_mut()
            .ok_or_else(|| "get_state.state must be an object".to_string())?;
        let has_explicit_groups = state
            .keys()
            .any(|key| state_key_group_for_available(key, &discovered).is_some());
        if has_explicit_groups {
            state.retain(|key, _| {
                state_key_group_for_available(key, &discovered)
                    .is_some_and(|group| contains_name(&applied, group))
            });
        } else {
            let fallback = fallback_state_group(has_cpu_identity, &discovered)?;
            if !contains_name(&applied, fallback) {
                state.clear();
            }
        }
    } else {
        let group_keys = top_level_group_keys(object);
        if group_keys.is_empty() {
            return Err("get_state response does not expose projectable state groups".into());
        }
        object.retain(|key, _| {
            !group_keys.iter().any(|group| group == key) || contains_name(&applied, key)
        });
    }
    object.insert("groups_applied".into(), serde_json::json!(applied));
    Ok(response)
}

pub fn resolve_state_groups(
    requested: &[String],
    advertised: &[String],
) -> Result<Vec<String>, String> {
    resolve_group_names(requested, advertised)
}

fn parse_name_list(value: Option<&Value>, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{field} must be an array"))?;
    let mut names = Vec::with_capacity(values.len());
    let mut seen = BTreeSet::new();
    for value in values {
        let name = value
            .as_str()
            .ok_or_else(|| format!("{field} entries must be strings"))?;
        validate_name(name, field)?;
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(format!(
                "{field} entry is advertised more than once: {name}"
            ));
        }
        names.push(name.to_string());
    }
    Ok(names)
}

fn validate_token(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(format!(
            "{field} must be a 1..=64 byte ASCII identifier: {value:?}"
        ));
    }
    Ok(())
}

fn validate_name(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(format!(
            "{field} entries must be non-empty names without control characters"
        ));
    }
    Ok(())
}

fn resolve_group_names(requested: &[String], available: &[String]) -> Result<Vec<String>, String> {
    let mut resolved = Vec::with_capacity(requested.len());
    for requested in requested {
        validate_name(requested, "groups")?;
        let canonical = available
            .iter()
            .find(|available| available.eq_ignore_ascii_case(requested))
            .ok_or_else(|| {
                format!(
                    "unknown state group '{requested}'; available: {}",
                    available.join(", ")
                )
            })?;
        if !contains_name(&resolved, canonical) {
            resolved.push(canonical.clone());
        }
    }
    Ok(resolved)
}

fn discover_state_groups(response: &Value) -> Result<Vec<String>, String> {
    let object = response
        .as_object()
        .ok_or_else(|| "get_state response must be an object".to_string())?;
    if let Some(state) = object.get("state") {
        let state = state
            .as_object()
            .ok_or_else(|| "get_state.state must be an object".to_string())?;
        let mut groups = Vec::new();
        for key in state.keys() {
            if let Some(group) = state_key_group(key) {
                if !contains_name(&groups, group) {
                    groups.push(group.to_string());
                }
            }
        }
        if groups.is_empty() && (!state.is_empty() || object.get("cpu").is_some()) {
            groups.push("cpu".into());
        }
        if groups.is_empty() {
            return Err("get_state response has no discoverable state groups".into());
        }
        return Ok(groups);
    }
    let groups = top_level_group_keys(object);
    if groups.is_empty() {
        Err("get_state response has no discoverable state groups".into())
    } else {
        Ok(groups)
    }
}

fn fallback_state_group(has_cpu_identity: bool, available: &[String]) -> Result<&str, String> {
    if has_cpu_identity
        && available
            .iter()
            .any(|group| group.eq_ignore_ascii_case("cpu"))
    {
        return Ok("cpu");
    }
    if available.len() == 1 {
        return Ok(&available[0]);
    }
    Err("unprefixed get_state keys cannot be assigned to more than one state group".into())
}

fn state_key_group(key: &str) -> Option<&str> {
    key.find(['.', '[']).map(|index| &key[..index])
}

fn state_key_group_for_available<'a>(key: &'a str, available: &'a [String]) -> Option<&'a str> {
    state_key_group(key).or_else(|| {
        available
            .iter()
            .find(|group| group.eq_ignore_ascii_case(key))
            .map(String::as_str)
    })
}

fn top_level_group_keys(object: &serde_json::Map<String, Value>) -> Vec<String> {
    const METADATA: &[&str] = &[
        "cpu",
        "frame",
        "state",
        "status",
        "groups_applied",
        "rendered_frame",
        "rendered_frame_observed",
        "rendered_frame_synchronized",
        "vi_count",
    ];
    object
        .iter()
        .filter(|(key, value)| {
            value.is_object() && !METADATA.iter().any(|metadata| metadata == key)
        })
        .map(|(key, _)| key.clone())
        .collect()
}

fn contains_name(values: &[String], wanted: &str) -> bool {
    values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(wanted))
}

#[cfg(test)]
#[path = "debug_selection_tests.rs"]
mod tests;
