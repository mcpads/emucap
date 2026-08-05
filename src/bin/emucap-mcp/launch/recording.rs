use emucap::live::link::EmulatorLink;
use emucap::live::runtime::{CurrentManifest, RuntimeStore};

pub(super) fn reconcile_previous_capture(
    link: &mut (dyn EmulatorLink + Send),
    store: &RuntimeStore,
    port: u16,
    current: &CurrentManifest,
) -> Option<serde_json::Value> {
    emucap::live::recording::reconcile_abandoned_capture(
        link,
        store.clone(),
        port,
        &current.launch_id,
    )
    .err()
    .map(|error| {
        serde_json::json!({
            "launched": false,
            "reason": "an unfinished recording capture could not be safely reconciled",
            "error": error.to_string(),
            "runtime_instance": current.public_value(),
            "next_action": "Keep the exact generation isolated and retry after its former controller has exited and cleanup or process termination is verifiable.",
        })
    })
}
