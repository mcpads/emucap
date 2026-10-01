//! Internal transport lifecycle. Producers opt in before a broker emits these envelopes.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LIFECYCLE_FIELD: &str = "control_session_lifecycle";
pub const EVENT_FIELD: &str = "_control_session";
pub const ATTACHMENT_FIELD: &str = "_control_attachment";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub broker_instance: String,
    pub registration: u64,
    pub session: u64,
}
impl Attachment {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.broker_instance.is_empty()
            || self.broker_instance.len() > 256
            || self.registration == 0
            || self.session == 0
        {
            Err("invalid control attachment")
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Attach,
    Detach,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEvent {
    pub kind: EventKind,
    pub runtime: String,
    pub attachment: Attachment,
}
impl SessionEvent {
    pub fn from_envelope(value: &Value) -> Result<Option<Self>, String> {
        let Some(event) = value.get(EVENT_FIELD) else {
            return Ok(None);
        };
        if value.as_object().is_none_or(|object| object.len() != 1) {
            return Err("lifecycle envelope has unrelated fields".into());
        }
        let event: Self = serde_json::from_value(event.clone()).map_err(|e| e.to_string())?;
        event.attachment.validate().map_err(str::to_owned)?;
        if event.runtime.is_empty() || event.runtime.len() > 256 {
            return Err("invalid lifecycle runtime".into());
        }
        Ok(Some(event))
    }
    pub fn envelope(&self) -> Value {
        serde_json::json!({EVENT_FIELD:self})
    }
}

/// None is legacy transport. Malformed opt-in is refused before registration.
pub fn advertised_runtime(hello: &Value) -> Result<Option<String>, &'static str> {
    match hello.get(LIFECYCLE_FIELD) {
        None => Ok(None),
        Some(Value::Bool(true)) => hello
            .get("launch_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .map(|s| Some(s.to_owned()))
            .ok_or("control lifecycle requires a runtime identity"),
        _ => Err("invalid control lifecycle advertisement"),
    }
}

pub fn has_reserved_fields(value: &Value) -> bool {
    value.get(EVENT_FIELD).is_some()
        || value.get(ATTACHMENT_FIELD).is_some()
        || value.get("method").and_then(Value::as_str) == Some(EVENT_FIELD)
        || value.get("params").is_some_and(|params| {
            params.get(EVENT_FIELD).is_some() || params.get(ATTACHMENT_FIELD).is_some()
        })
}
