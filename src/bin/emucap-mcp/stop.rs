use emucap::live::link::EmulatorLink;
use emucap::live::runtime::{
    LeaseState, ProcessState, RuntimeStore, TerminationRecord, TerminationState,
};

use crate::args::StopArgs;

pub(crate) fn make_stop(
    link: &mut (dyn EmulatorLink + Send),
    args: &StopArgs,
) -> serde_json::Value {
    make_stop_with_store(link, args, &RuntimeStore::discover())
}

fn make_stop_with_store(
    link: &mut (dyn EmulatorLink + Send),
    args: &StopArgs,
    store: &RuntimeStore,
) -> serde_json::Value {
    let Some(port) = link.endpoint_port() else {
        return serde_json::json!({
            "stopped": false,
            "reason": "this Control MCP does not own a managed runtime port; refusing process termination",
            "launch_id": args.launch_id,
            "next_action": "Use stop from the Control MCP that owns the runtime capsule. Broker or unmanaged emulator processes are not terminated by inference.",
        });
    };

    let current = match store.read_current(port) {
        Ok(Some(current)) => current,
        Ok(None) => {
            return serde_json::json!({
                "stopped": false,
                "reason": "no current managed runtime generation exists on this listener port",
                "launch_id": args.launch_id,
                "listening_port": port,
            })
        }
        Err(error) => {
            return serde_json::json!({
                "stopped": false,
                "reason": "runtime current capsule is unreadable; refusing to guess process ownership",
                "launch_id": args.launch_id,
                "listening_port": port,
                "error": error.to_string(),
            })
        }
    };
    if current.launch_id != args.launch_id {
        return serde_json::json!({
            "stopped": false,
            "reason": "requested launch_id is not the current runtime generation; no process was signalled",
            "launch_id": args.launch_id,
            "runtime_instance": current.public_value_with_lease(&link.continuity().lease),
            "next_action": "Read status.runtime_instance.launch_id and stop only that exact generation.",
        });
    }

    let lease = match link.acquire_control_lease(&args.launch_id) {
        Ok(lease) => lease,
        Err(error) => {
            return serde_json::json!({
                "stopped": false,
                "reason": "failed to acquire the exact runtime generation lease; no process was signalled",
                "launch_id": args.launch_id,
                "error": error.to_string(),
            })
        }
    };
    if lease.state != LeaseState::Held {
        return serde_json::json!({
            "stopped": false,
            "reason": "the runtime generation is controlled by another or unverifiable lease; no process was signalled",
            "launch_id": args.launch_id,
            "lease": lease,
            "runtime_instance": current.public_value_with_lease(&lease),
            "next_action": "Wait for the controller to release the lease or use that controller to stop the generation.",
        });
    }

    // Lease acquisition is conditional on launch_id, but re-read under the current writer's view
    // before recording intent or signalling. This closes the status-to-stop generation race.
    let current = match store.read_current(port) {
        Ok(Some(current)) if current.launch_id == args.launch_id => current,
        Ok(Some(current)) => {
            return serde_json::json!({
                "stopped": false,
                "reason": "runtime current generation changed after lease acquisition; no process was signalled",
                "launch_id": args.launch_id,
                "runtime_instance": current.public_value_with_lease(&lease),
            })
        }
        Ok(None) => {
            return serde_json::json!({
                "stopped": false,
                "reason": "runtime current generation disappeared after lease acquisition; no process was signalled",
                "launch_id": args.launch_id,
            })
        }
        Err(error) => {
            return serde_json::json!({
                "stopped": false,
                "reason": "runtime current capsule became unreadable after lease acquisition; no process was signalled",
                "launch_id": args.launch_id,
                "error": error.to_string(),
            })
        }
    };

    if let Err(error) = current.validate_termination_targets() {
        return serde_json::json!({
            "stopped": false,
            "reason": "a generation-owned process identity is unknown; no process was signalled",
            "launch_id": args.launch_id,
            "error": error.to_string(),
            "runtime_instance": current.public_value_with_lease(&lease),
        });
    }

    let previous = match store.read_termination(port, &args.launch_id) {
        Ok(previous) => previous,
        Err(error) => {
            return serde_json::json!({
                "stopped": false,
                "reason": "runtime termination record is unreadable; no process was signalled",
                "launch_id": args.launch_id,
                "error": error.to_string(),
            })
        }
    };
    let requested = TerminationRecord::requested(port, args.launch_id.clone(), previous.as_ref());
    if let Err(error) = store.write_current_termination(&requested) {
        return serde_json::json!({
            "stopped": false,
            "reason": "could not durably record stop intent; no process was signalled",
            "launch_id": args.launch_id,
            "error": error.to_string(),
        });
    }

    let result = match current.terminate_owned_processes_report() {
        Ok(result) => result,
        Err(error) => {
            let failed = requested
                .clone()
                .finish(emucap::live::runtime::GenerationTermination {
                    completed: false,
                    emulator: emucap::live::runtime::ProcessTermination {
                        pid: current.emulator.pid,
                        before: current.process_state(),
                        after: current.process_state(),
                        method: None,
                        error: Some(error.to_string()),
                    },
                    bridge: current.bridge.as_ref().map(|bridge| {
                        emucap::live::runtime::ProcessTermination {
                            pid: bridge.pid,
                            before: current
                                .bridge_process_state()
                                .unwrap_or(ProcessState::Unknown),
                            after: current
                                .bridge_process_state()
                                .unwrap_or(ProcessState::Unknown),
                            method: None,
                            error: Some("termination was rejected before signalling".into()),
                        }
                    }),
                });
            let record_error = store
                .write_current_termination(&failed)
                .err()
                .map(|write_error| write_error.to_string());
            return serde_json::json!({
                "stopped": false,
                "reason": "generation termination was rejected before signalling",
                "launch_id": args.launch_id,
                "error": error.to_string(),
                "termination_record_error": record_error,
            });
        }
    };
    let finished = requested.finish(result.clone());
    if let Err(error) = store.write_current_termination(&finished) {
        return serde_json::json!({
            "stopped": false,
            "reason": "process termination finished but its terminal result could not be recorded",
            "launch_id": args.launch_id,
            "error": error.to_string(),
            "processes": result,
            "next_action": "Inspect status and the generation termination record before launching or retrying stop.",
        });
    }

    let mut runtime_instance = current.public_value_with_lease(&lease);
    if let Some(runtime) = runtime_instance.as_object_mut() {
        runtime.insert(
            "termination".into(),
            serde_json::to_value(&finished).unwrap_or_else(|_| serde_json::json!({})),
        );
    }
    serde_json::json!({
        "stopped": finished.state == TerminationState::Completed,
        "launch_id": args.launch_id,
        "status": if finished.state == TerminationState::Completed { "completed" } else { "failed" },
        "processes": result,
        "termination": finished,
        "failure_context_preserved": true,
        "runtime_instance": runtime_instance,
        "next_action": if result.completed {
            "The managed generation is stopped. A later launch may replace this exited current generation."
        } else {
            "Inspect the per-process result and retry stop only for this same launch_id after resolving the reported failure."
        },
    })
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
