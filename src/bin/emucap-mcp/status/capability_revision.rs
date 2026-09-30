//! Revision of the connected adapter's capability surface and unchanged-catalog omission.
use super::json_revision;

pub(super) const CAPABILITY_FIELDS: &[&str] = &[
    "methods",
    "memory_types",
    "memory_regions",
    "state_groups",
    "cpu_targets",
    "media_devices",
    "breakpoint_kinds",
    "input_buttons",
    "input_axes",
    "contracts",
    "capability_notes",
    "execution_limits",
    "freeze_policy",
    "bank_tagging",
    "recording_capability",
    "snapshot_capability",
    "memory_batch_capability",
    "execution_speed_capability",
];

pub(super) fn capability_revision(value: &serde_json::Value) -> String {
    let mut snapshot = serde_json::Map::new();
    snapshot.insert("schema".into(), serde_json::json!(1));
    for field in CAPABILITY_FIELDS {
        if let Some(field_value) = value.get(field) {
            snapshot.insert((*field).into(), field_value.clone());
        }
    }
    for field in [
        "server_build",
        "emulator_build",
        "emulator_identity",
        "protocol_version",
    ] {
        if let Some(field_value) = value.get(field) {
            snapshot.insert(field.into(), field_value.clone());
        }
    }
    json_revision(&serde_json::Value::Object(snapshot))
}

fn remove_capability_fields(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for field in CAPABILITY_FIELDS {
        object.remove(*field);
    }
}

pub(crate) fn apply_capability_revision(
    value: &mut serde_json::Value,
    known_revision: Option<&str>,
) -> String {
    let revision = capability_revision(value);
    let unchanged = known_revision == Some(revision.as_str());
    if unchanged {
        remove_capability_fields(value);
    }
    value["capability_revision"] = serde_json::json!(revision);
    value["capability_snapshot"] = serde_json::json!(if unchanged { "unchanged" } else { "full" });
    revision
}
