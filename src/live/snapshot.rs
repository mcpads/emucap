//! Instruction snapshot orchestration. Native capture supplies facts and bytes in one response.
use super::link::{ProgressCallControl, RequestCancellation};
use super::snapshot_store::{self, Request, Store};
use super::{
    link::{EmulatorLink, LinkError},
    runtime::RuntimeStore,
    tools::ToolOutput,
};
use crate::bundle::{
    recording_manifest::StateArtifactIdentity,
    snapshot::{Halt, Receipt, Snapshot, CAPABILITY, MAX_SNAPSHOT_BYTES},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{Duration, Instant};

pub const MAX_HOST_MS: u64 = 30_000;
struct Budget {
    deadline: Instant,
    cancellation: RequestCancellation,
}
impl Budget {
    fn check(&self) -> Result<u64, LinkError> {
        if self.cancellation.is_cancelled() {
            return Err(LinkError::Cancelled);
        }
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining == 0 {
            return Err(LinkError::Timeout);
        }
        Ok(remaining)
    }
    fn call(&self, link: &mut dyn EmulatorLink, method: &str) -> Result<Value, LinkError> {
        let control = ProgressCallControl {
            cancellation: self.cancellation.clone(),
            abort: None,
            max_host_ms: Some(self.check()?),
        };
        link.call_with_progress(method, json!({}), &mut |_| Ok(()), &control)
    }
}

fn error(message: impl std::fmt::Display) -> LinkError {
    LinkError::Protocol(message.to_string())
}
pub fn supported(link: &dyn EmulatorLink) -> bool {
    link.capabilities()
        .identity
        .host_features
        .iter()
        .any(|s| s == CAPABILITY)
}
fn check_boundary(
    link: &mut dyn EmulatorLink,
    halt: &Halt,
    launch: &str,
    budget: &Budget,
) -> Result<(), LinkError> {
    let value = budget.call(link, "observe_snapshot_halt")?;
    let observed: Halt = serde_json::from_value(value["halt"].clone()).map_err(error)?;
    if &observed != halt
        || link.capabilities().identity.launch_id.as_deref() != Some(launch)
        || link.continuity().runtime_binding.state != super::continuity::RuntimeBindingState::Bound
    {
        return Err(error("snapshot boundary or runtime binding changed"));
    }
    Ok(())
}

pub fn save(link: &mut dyn EmulatorLink, path: &str, key: &str) -> Result<ToolOutput, LinkError> {
    save_in_store(link, path, key, &RuntimeStore::discover())
}
pub(crate) fn save_in_store(
    link: &mut dyn EmulatorLink,
    path: &str,
    key: &str,
    runtime: &RuntimeStore,
) -> Result<ToolOutput, LinkError> {
    save_with_cancellation(link, path, key, runtime, RequestCancellation::default())
}

pub fn save_with_cancellation(
    link: &mut dyn EmulatorLink,
    path: &str,
    key: &str,
    runtime: &RuntimeStore,
    cancellation: RequestCancellation,
) -> Result<ToolOutput, LinkError> {
    let budget = Budget {
        deadline: Instant::now() + Duration::from_millis(MAX_HOST_MS),
        cancellation,
    };
    budget.check()?;
    if !supported(link) {
        return Err(error("instruction snapshot capture is not advertised"));
    }
    if !Path::new(path).is_absolute() {
        return Err(error("snapshot destination must be absolute"));
    }
    let port = link
        .endpoint_port()
        .ok_or_else(|| error("snapshot requires a managed runtime"))?;
    budget.call(link, "status")?;
    let identity = link.capabilities().identity.clone();
    let current = runtime
        .read_current(port)
        .map_err(error)?
        .ok_or_else(|| error("runtime generation missing"))?;
    if identity.launch_id.as_deref() != Some(&current.launch_id)
        || identity.content.as_deref() != Some(&current.content)
        || link.continuity().runtime_binding.state != super::continuity::RuntimeBindingState::Bound
    {
        return Err(error("snapshot requires exact runtime binding"));
    }
    snapshot_store::validate_destination(runtime, Path::new(path)).map_err(error)?;
    let (store, fresh) = Store::open(runtime, key, true).map_err(error)?;
    if !fresh {
        let previous = store
            .request()
            .map_err(|e| error(format!("snapshot key is indeterminate: {e}")))?;
        if previous.destination != path || previous.source.launch_id != current.launch_id {
            return Err(error(
                "snapshot_key conflicts with its reserved launch or destination",
            ));
        }
        return Ok(ToolOutput::Json(observe(&store, None, None)?));
    }
    let source = super::evidence_identity::runtime_identity(
        &identity,
        &current,
        CAPABILITY,
        64 * 1024 * 1024,
    )
    .map_err(error)?;
    let request = Request {
        destination: path.into(),
        source,
        snapshot_id: format!(
            "snapshot-{}",
            ulid::Ulid::generate().to_string().to_lowercase()
        ),
    };
    store.reserve(&request).map_err(error)?;
    let result = capture_and_publish(link, &store, &request, &budget);
    let mut outcome = match result {
        Ok(value) => value,
        Err(e) => json!({"status":"failed", "snapshot_key":key,
            "receipt_issued":if store.acquire().is_ok() { Some(true) } else if std::fs::symlink_metadata(store.directory.join("published")).is_ok() { None } else { Some(false) }, "error":e.to_string(),
            "next_action":"Inspect snapshot_receipt with this key; do not repeat serialization."}),
    };
    outcome["snapshot_key"] = json!(key);
    // Publication remains authoritative if this terminal update cannot be persisted.
    store.terminal(&outcome).map_err(error)?;
    Ok(ToolOutput::Json(outcome))
}
fn capture_and_publish(
    link: &mut dyn EmulatorLink,
    store: &Store,
    request: &Request,
    budget: &Budget,
) -> Result<Value, LinkError> {
    let loaded = budget.call(link, "get_rom_info")?;
    if loaded["sha1"].as_str().is_none_or(|sha| {
        request
            .source
            .content
            .sha1
            .as_deref()
            .is_none_or(|expected| !sha.eq_ignore_ascii_case(expected))
    }) {
        return Err(error("loaded artifact differs before snapshot capture"));
    }
    let captured = budget.call(link, "capture_snapshot")?;
    let halt: Halt = serde_json::from_value(captured["halt"].clone()).map_err(error)?;
    halt.validate().map_err(error)?;
    if captured["state"] != "frozen"
        || captured["boundary"] != "instruction_boundary"
        || captured["format"].as_str().is_none_or(str::is_empty)
        || captured["loaded_artifact_sha1"].as_str() != request.source.content.sha1.as_deref()
    {
        return Err(error("capture lacks exact frozen artifact binding"));
    }
    let encoded = captured["hex"]
        .as_str()
        .ok_or_else(|| error("capture lacks snapshot bytes"))?;
    if encoded.len() as u64 > MAX_SNAPSHOT_BYTES * 2 {
        return Err(error("snapshot exceeds byte bound"));
    }
    let bytes = hex::decode(encoded).map_err(error)?;
    let receipt = Receipt::issue(Snapshot {
        kind: "instruction_snapshot".into(),
        snapshot_id: request.snapshot_id.clone(),
        source: request.source.clone(),
        halt,
        snapshot: StateArtifactIdentity {
            format: captured["format"].as_str().unwrap().into(),
            bytes: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        },
    })
    .map_err(error)?;
    receipt.verify(&bytes).map_err(error)?;
    check_boundary(link, &receipt.body.halt, &request.source.launch_id, budget)?;
    budget.check()?;
    store.publish(&receipt, &bytes).map_err(error)?;
    budget.check()?;
    snapshot_store::export(Path::new(&request.destination), &bytes).map_err(error)?;
    check_boundary(link, &receipt.body.halt, &request.source.launch_id, budget)?;
    Ok(
        json!({"status":"completed", "state":"frozen", "boundary":"instruction_boundary", "path":request.destination,
        "bytes":bytes.len(), "receipt_issued":true, "export":"completed", "snapshot_receipt":receipt,
        "receipt_path":store.directory.join("published/receipt.json"),
        "snapshot_path":store.directory.join("published/state.bin")}),
    )
}
fn observe(
    store: &Store,
    path: Option<&str>,
    expected_launch: Option<&str>,
) -> Result<Value, LinkError> {
    let request = match store.request() {
        Ok(request) => request,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"status":"indeterminate", "receipt_issued":null,
                "next_action":"Reserved key has no durable request record. It cannot serialize again."}));
        }
        Err(e) => return Err(error(e)),
    };
    if expected_launch.is_some_and(|s| s != request.source.launch_id) {
        return Err(error("snapshot launch mismatch"));
    }
    let acquired = store.acquire();
    if matches!(std::fs::symlink_metadata(store.directory.join("published")), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    {
        if let Some(outcome) = store.outcome().map_err(error)? {
            return Ok(outcome);
        }
        return Ok(json!({"status":"indeterminate", "receipt_issued":false,
            "snapshot_id":request.snapshot_id, "next_action":"No verified publication exists. This key cannot serialize again."}));
    }
    let (receipt, _) = acquired.map_err(error)?;
    if let Some(path) = path {
        if !Path::new(path).is_absolute() {
            return Err(error("snapshot verification path must be absolute"));
        }
        let bytes = crate::path_safety::read_bounded_regular_file_no_follow(
            Path::new(path),
            MAX_SNAPSHOT_BYTES,
        )
        .map_err(error)?;
        receipt.verify(&bytes).map_err(error)?;
    }
    let (outcome, outcome_error) = match store.outcome() {
        Ok(outcome) => (outcome, None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(
        json!({"status":"verified", "receipt_issued":true, "snapshot_receipt":receipt,
        "expected_launch_checked":expected_launch.is_some(), "copy_checked":path.is_some(),
        "save_outcome":outcome, "save_outcome_error":outcome_error,
        "receipt_path":store.directory.join("published/receipt.json"),
        "snapshot_path":store.directory.join("published/state.bin")}),
    )
}
pub fn verify(
    key: &str,
    path: Option<&str>,
    expected_launch: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    verify_in_store(&RuntimeStore::discover(), key, path, expected_launch)
}
pub(crate) fn verify_in_store(
    runtime: &RuntimeStore,
    key: &str,
    path: Option<&str>,
    expected_launch: Option<&str>,
) -> Result<ToolOutput, LinkError> {
    let (store, _) = Store::open(runtime, key, false).map_err(error)?;
    Ok(ToolOutput::Json(observe(&store, path, expected_launch)?))
}
