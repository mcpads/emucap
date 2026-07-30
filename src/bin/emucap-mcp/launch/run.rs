use super::plan::*;
use super::*;

fn transition_rejection(
    reason: EntryReason,
    status: serde_json::Value,
    current: Option<&emucap::live::runtime::CurrentManifest>,
) -> serde_json::Value {
    let (message, next_action) = match reason {
        EntryReason::RuntimeMetadataInvalid => (
            "runtime current capsule is unreadable; refusing to guess ownership",
            "Inspect status.runtime_diagnostics and repair or isolate the exact runtime metadata before launching.",
        ),
        EntryReason::ListenerBlocked | EntryReason::RuntimeCandidateAmbiguity => (
            "listening_port is already occupied or has ambiguous runtime candidates; refusing launch",
            "Inspect status and resolve the exact listener or runtime candidate without editing session files.",
        ),
        EntryReason::ListenerUnavailable => (
            "listening_port is unavailable; refusing launch",
            "Call bootstrap or status again to establish a listener.",
        ),
        EntryReason::FailurePreserved => (
            "the current generation is preserving failure evidence; refusing replacement",
            "Call get_failure_context before dismissing or replacing the failed generation.",
        ),
        EntryReason::TransportUncertain => (
            "adapter control status is stalled or malformed; refusing to infer disconnection",
            "Call status again or stop the exact managed generation before starting another launch.",
        ),
        EntryReason::LiveManagedGeneration => (
            "an emulator is already connected or the current launch generation is still alive",
            "Inspect status and reattach. Use replace=true only for an intentional replacement.",
        ),
        EntryReason::LiveUnmanagedGeneration => (
            "an emulator is already connected but legacy or unmanaged ownership cannot be proven",
            "Inspect status and stop the existing emulator through its verified owner before launching.",
        ),
        EntryReason::LeaseOccupied => (
            "the current generation lease is held by another live controller",
            "Wait for the current controller to release the lease.",
        ),
        EntryReason::LeaseUnknown => (
            "the current generation lease holder is unverifiable",
            "Inspect status and restore verifiable ownership; do not edit lease or link files.",
        ),
        EntryReason::BridgeExited => (
            "the current emulator is alive but its required bridge exited",
            "Inspect status and use replace=true only if terminating the exact managed generation is intended.",
        ),
        EntryReason::BridgeIdentityUnknown => (
            "current bridge process identity is unknown; refusing unsafe transition",
            "Verify bridge process identity and act only on the exact managed generation.",
        ),
        EntryReason::ProcessIdentityUnknown => (
            "current process liveness is unknown; refusing unsafe transition",
            "Verify process-start identity before retrying launch or replacement.",
        ),
        EntryReason::TransportReattach => (
            "the current launch generation should be reattached instead of duplicated",
            "Call status to reattach to the same generation.",
        ),
        EntryReason::ReadyNoHistory
        | EntryReason::TerminalHistory
        | EntryReason::OwnedHelperCleanupPending => (
            "generation transition was rejected after its readiness classification changed",
            "Call status and retry against the current generation.",
        ),
    };
    let mut value = serde_json::json!({
        "launched": false,
        "reason": message,
        "entry_reason": reason,
        "status": status,
        "next_action": next_action,
    });
    if let Some(current) = current {
        value["runtime_instance"] = current.public_value();
    }
    if matches!(
        reason,
        EntryReason::LiveManagedGeneration | EntryReason::LiveUnmanagedGeneration
    ) {
        value["connected_emulator"] = value["status"]
            .get("emulator_identity")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
    }
    value
}

/// Actually launch an emulator (the `launch` tool): ensure the listener, capture this session's port +
/// token, pick the adapter from the system/extension, and dispatch to that adapter's Rust orchestrator.
/// Returns a JSON outcome. A system without a Rust orchestrator yet points back at launch_plan, so no
/// existing flow breaks. The per-adapter spawn logic lives in emucap::launch::<adapter>, not here.
pub(crate) fn make_launch(
    link: &mut (dyn EmulatorLink + Send),
    a: &LaunchArgs,
) -> serde_json::Value {
    let control = match observe_control_state(link) {
        Ok(observation) => observation,
        Err(e) => return serde_json::json!({ "launched": false, "error": e.to_string() }),
    };
    let status = control.status;
    let intent = if a.replace {
        TransitionIntent::Replace
    } else {
        TransitionIntent::Launch
    };
    let initial_admission = admit_generation_transition(&control.runtime, intent);
    if let TransitionAdmission::Rejected(reason) = initial_admission {
        return transition_rejection(reason, status, None);
    }
    let Some(port) = link.endpoint_port() else {
        return transition_rejection(EntryReason::ListenerUnavailable, status, None);
    };
    let token = link.session_token().map(str::to_string);
    let store = RuntimeStore::discover();
    let previous = match store.read_current(port) {
        Ok(value) => value,
        Err(e) => {
            return serde_json::json!({
                "launched": false,
                "reason": "runtime current capsule is unreadable; refusing to guess ownership",
                "error": e.to_string(),
                "listening_port": port,
            })
        }
    };

    if !Path::new(&a.content_path).exists() {
        return serde_json::json!({
            "launched": false,
            "reason": "content_path does not exist",
            "content_path": &a.content_path,
            "next_action": "Verify content_path, then call launch_plan(content_path, system) again.",
        });
    }

    let inference = infer_system(Some(&a.content_path), a.system.as_deref());
    let Some(system) = inference.get("system").and_then(|v| v.as_str()) else {
        return serde_json::json!({
            "launched": false,
            "reason": "The system is ambiguous for this media; specify system and call again.",
            "inference": inference,
        });
    };
    let (adapter, module) = adapter_for_system(system);
    if a.sound == Some(true) && adapter != "mednafen" {
        return serde_json::json!({
            "launched": false,
            "reason": "sound:true is supported only by Mednafen systems",
            "system": system,
            "adapter": adapter,
        });
    }
    if let Some(root) = find_repo_root() {
        let adapter_binary =
            adapter_binary_precondition_for(adapter, &root, a.display.unwrap_or(false));
        if !adapter_binary["available"].as_bool().unwrap_or(false) {
            return missing_adapter_binary_response(adapter, system, port, &root, adapter_binary);
        }
        if adapter == "mame_pc98" {
            let bridge = mame_bridge_precondition(&root);
            if !bridge["available"].as_bool().unwrap_or(false) {
                return missing_mame_bridge_response(system, port, &root, adapter_binary, bridge);
            }
        } else if adapter == "mame_neogeo" {
            let bridge = neogeo_bridge_precondition(&root);
            if !bridge["available"].as_bool().unwrap_or(false) {
                return serde_json::json!({
                    "launched": false,
                    "reason": "mame_neogeo bridge is unavailable",
                    "system": system,
                    "adapter": adapter,
                    "preconditions": {
                        "adapter_binary": adapter_binary,
                        "bridge": bridge,
                    },
                    "next_action": "build emucap-mame-neogeo-bridge and retry launch_plan",
                });
            }
        }
    }
    if initial_admission == TransitionAdmission::AcquireLease {
        let Some(current) = previous.as_ref() else {
            return transition_rejection(EntryReason::ProcessIdentityUnknown, status, None);
        };
        let lease = match link.acquire_control_lease(&current.launch_id) {
            Ok(lease) => lease,
            Err(error) => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "failed to acquire the runtime generation lease",
                    "error": error.to_string(),
                })
            }
        };
        if lease.state != LeaseState::Held {
            let reason = if lease.state == LeaseState::Occupied {
                EntryReason::LeaseOccupied
            } else {
                EntryReason::LeaseUnknown
            };
            return transition_rejection(reason, status, Some(current));
        }
    }

    let refreshed = match store.read_current(port) {
        Ok(current) => current,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "reason": "runtime current capsule became unreadable before generation transition",
                "error": error.to_string(),
            })
        }
    };
    let expected_launch_id = previous.as_ref().map(|current| current.launch_id.as_str());
    let refreshed_launch_id = refreshed.as_ref().map(|current| current.launch_id.as_str());
    if expected_launch_id != refreshed_launch_id {
        return serde_json::json!({
            "launched": false,
            "reason": "runtime current generation changed before launch transition; no process was signalled",
            "expected_launch_id": expected_launch_id,
            "current_launch_id": refreshed_launch_id,
        });
    }
    let adapter_connected = status
        .get("connected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let refreshed_observation = observe_runtime(link, ListenerState::Bound, adapter_connected);
    if let TransitionAdmission::Rejected(reason) =
        admit_generation_transition(&refreshed_observation, intent)
    {
        return transition_rejection(reason, status, refreshed.as_ref());
    }
    if refreshed.is_some()
        && admit_generation_transition(&refreshed_observation, intent)
            == TransitionAdmission::AcquireLease
    {
        return transition_rejection(EntryReason::LeaseUnknown, status, refreshed.as_ref());
    }

    let before_signal = match store.read_current(port) {
        Ok(current) => current,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "reason": "runtime current capsule became unreadable immediately before process transition",
                "error": error.to_string(),
            })
        }
    };
    let before_signal_launch_id = before_signal
        .as_ref()
        .map(|current| current.launch_id.as_str());
    if expected_launch_id != before_signal_launch_id {
        return serde_json::json!({
            "launched": false,
            "reason": "runtime current generation changed immediately before process transition; no process was signalled",
            "expected_launch_id": expected_launch_id,
            "current_launch_id": before_signal_launch_id,
        });
    }

    if let Some(current) = before_signal.as_ref() {
        match (current.process_state(), current.bridge_process_state()) {
            (ProcessState::Unknown, _) => {
                return transition_rejection(
                    EntryReason::ProcessIdentityUnknown,
                    status,
                    Some(current),
                );
            }
            (_, Some(ProcessState::Unknown)) => {
                return transition_rejection(
                    EntryReason::BridgeIdentityUnknown,
                    status,
                    Some(current),
                );
            }
            (ProcessState::Alive, _) => {
                if let Err(error) = current.terminate_owned_processes() {
                    return serde_json::json!({
                        "launched": false,
                        "reason": "verified current generation could not be terminated for replacement",
                        "error": error.to_string(),
                        "runtime_instance": current.public_value(),
                    });
                }
            }
            (ProcessState::Exited, Some(ProcessState::Alive)) => {
                if let Err(error) = current.terminate_owned_processes() {
                    return serde_json::json!({
                        "launched": false,
                        "reason": "the emulator exited but its verified bridge could not be cleaned up",
                        "error": error.to_string(),
                        "runtime_instance": current.public_value(),
                    });
                }
            }
            (ProcessState::Exited, Some(ProcessState::Exited) | None) => {}
        }
    }

    let prepared = match store.prepare(port) {
        Ok(prepared) => prepared,
        Err(e) => {
            return serde_json::json!({
                "launched": false,
                "reason": "failed to prepare runtime launch generation",
                "error": e.to_string(),
            })
        }
    };
    let direct_reclaim = match link.stage_reclaim_token(prepared.reclaim_token()) {
        Ok(true) => Some(prepared.reclaim_token()),
        Ok(false) if token.is_none() => None,
        Ok(false) => {
            let _ = prepared.abort();
            return serde_json::json!({
                "launched": false,
                "reason": "direct link cannot install a launch-generation reclaim capability",
            });
        }
        Err(e) => {
            let _ = prepared.abort();
            return serde_json::json!({
                "launched": false,
                "reason": "failed to install launch reclaim capability",
                "error": e.to_string(),
            });
        }
    };

    let failure_path = prepared.adapter_failure_path();
    let runtime = RuntimeEnv {
        launch_id: prepared.launch_id(),
        adapter_failure_path: &failure_path,
    };
    let mut outcome = match adapter {
        "mesen2" => launch_mesen(port, direct_reclaim, runtime, system, a),
        "mednafen" => launch_mednafen(port, direct_reclaim, runtime, module, a),
        "flycast" => launch_flycast(port, direct_reclaim, runtime, a),
        "mame_pc98" => launch_mame(port, direct_reclaim, runtime, a),
        "mame_neogeo" => {
            super::mame_neogeo::launch_mame_neogeo(port, direct_reclaim, runtime, system, a)
        }
        "mupen64plus" => launch_mupen64plus(port, direct_reclaim, runtime, a),
        "openmsx" => super::openmsx::launch_openmsx(port, direct_reclaim, runtime, system, a),
        "desmume_nds" => launch_desmume_nds(port, direct_reclaim, runtime, a),
        "ppsspp" => launch_ppsspp(port, direct_reclaim, runtime, a),
        "pcsx2" => launch_pcsx2(port, direct_reclaim, runtime, a),
        "dolphin" => launch_dolphin(port, direct_reclaim, runtime, system, a),
        _ => serde_json::json!({
            "launched": false,
            "reason": format!("system {system} is not supported by the Rust launcher"),
        }),
    };
    if !outcome
        .get("launched")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        record_token_cleanup_error(&mut outcome, abort_staged_reclaim(link, direct_reclaim));
        let _ = prepared.abort();
        return outcome;
    }

    let bridge_pid = outcome
        .get("bridge_pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok());
    let Some(emulator_pid) = outcome
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        if let Some(bridge_pid) = bridge_pid {
            let _ = emucap::launch::terminate_detached(bridge_pid);
        }
        let token_cleanup_error = abort_staged_reclaim(link, direct_reclaim);
        let _ = prepared.abort();
        let mut failure = serde_json::json!({
            "launched": false,
            "reason": "launcher returned success without an emulator PID",
            "launcher_outcome": outcome,
        });
        record_token_cleanup_error(&mut failure, token_cleanup_error);
        return failure;
    };
    let backend_endpoint = backend_endpoint_from_launch(&outcome);
    // 즉시 exec 실패·동적 로더 오류가 이전 current를 덮지 않게 짧은 process-readiness 창을 둔다.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let manifest = prepared.manifest(ManifestSpec {
        adapter: adapter.into(),
        system: system.into(),
        content: a.content_path.clone(),
        emulator_pid,
        bridge_pid,
        backend_endpoint,
        build: Some(BUILD_HASH.to_string()),
    });
    let emulator_state = manifest.process_state();
    let bridge_state = manifest.bridge_process_state();
    if emulator_state != ProcessState::Alive
        || bridge_state.is_some_and(|state| state != ProcessState::Alive)
    {
        if let Some(bridge_pid) = bridge_pid {
            let _ = emucap::launch::terminate_detached(bridge_pid);
        }
        let _ = emucap::launch::terminate_detached(emulator_pid);
        let token_cleanup_error = abort_staged_reclaim(link, direct_reclaim);
        let _ = prepared.abort();
        let mut failure = serde_json::json!({
            "launched": false,
            "reason": "a launch process was not verifiably alive before the runtime generation became current",
            "emulator_process_state": emulator_state,
            "bridge_process_state": bridge_state,
            "launcher_outcome": outcome,
        });
        record_token_cleanup_error(&mut failure, token_cleanup_error);
        return failure;
    }
    let ready_status = match wait_for_adapter_ready(link, adapter_ready_timeout(adapter), || {
        let emulator_state = manifest.process_state();
        let bridge_state = manifest.bridge_process_state();
        if emulator_state != ProcessState::Alive
            || bridge_state.is_some_and(|state| state != ProcessState::Alive)
        {
            Err(format!(
                    "launch process exited before adapter hello: emulator={emulator_state:?}, bridge={bridge_state:?}"
                ))
        } else {
            Ok(())
        }
    }) {
        Ok(status) => status,
        Err(error) => {
            let _ = manifest.terminate_owned_processes();
            let token_cleanup_error = abort_staged_reclaim(link, direct_reclaim);
            let _ = prepared.abort();
            let mut failure = serde_json::json!({
                "launched": false,
                "reason": "adapter did not become ready",
                "error": error,
                "launcher_outcome": outcome,
            });
            record_token_cleanup_error(&mut failure, token_cleanup_error);
            return failure;
        }
    };
    if let Some(reclaim_token) = direct_reclaim {
        match link.commit_staged_reclaim_token(reclaim_token) {
            Ok(true) => {}
            Ok(false) => {
                let _ = manifest.terminate_owned_processes();
                let token_cleanup_error = abort_staged_reclaim(link, direct_reclaim);
                let _ = prepared.abort();
                let mut failure = serde_json::json!({
                    "launched": false,
                    "reason": "direct link could not commit the ready launch token",
                });
                record_token_cleanup_error(&mut failure, token_cleanup_error);
                return failure;
            }
            Err(error) => {
                let _ = manifest.terminate_owned_processes();
                let token_cleanup_error = abort_staged_reclaim(link, direct_reclaim);
                let _ = prepared.abort();
                let mut failure = serde_json::json!({
                    "launched": false,
                    "reason": "failed to commit the ready launch token",
                    "error": error.to_string(),
                });
                record_token_cleanup_error(&mut failure, token_cleanup_error);
                return failure;
            }
        }
    }
    if let Err(e) = prepared.commit(&manifest) {
        let _ = manifest.terminate_owned_processes();
        let token_cleanup_error = token
            .as_deref()
            .and_then(|old| link.replace_reclaim_token(old).err())
            .map(|error| error.to_string());
        let _ = prepared.abort();
        let mut failure = serde_json::json!({
            "launched": false,
            "reason": "failed to publish runtime current generation",
            "error": e.to_string(),
        });
        record_token_cleanup_error(&mut failure, token_cleanup_error);
        return failure;
    }
    if let Some(obj) = outcome.as_object_mut() {
        obj.insert("launch_id".into(), serde_json::json!(prepared.launch_id()));
        obj.insert("runtime_instance".into(), manifest.public_value());
        obj.insert("ready".into(), serde_json::json!(true));
        obj.insert("connected".into(), serde_json::json!(true));
        obj.insert(
            "state".into(),
            ready_status
                .get("state")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "next_action".into(),
            serde_json::json!(
                "Inspect methods, memory_types, and state with status before starting work."
            ),
        );
    }
    outcome
}

fn abort_staged_reclaim(
    link: &mut (dyn EmulatorLink + Send),
    reclaim_token: Option<&str>,
) -> Option<String> {
    reclaim_token.and_then(|token| {
        link.abort_staged_reclaim_token(token)
            .err()
            .map(|error| error.to_string())
    })
}

fn record_token_cleanup_error(outcome: &mut serde_json::Value, error: Option<String>) {
    let Some(error) = error else {
        return;
    };
    if let Some(object) = outcome.as_object_mut() {
        object.insert("token_cleanup_error".into(), serde_json::json!(error));
        object.insert(
            "next_safe_action".into(),
            serde_json::json!(
                "Inspect the listener token state before another launch; do not edit runtime files."
            ),
        );
    }
}

pub(super) fn adapter_ready_timeout(adapter: &str) -> std::time::Duration {
    // Mesen starts an Avalonia window and then loads the command-line Lua script. On macOS a cold
    // display wake can make that path exceed the generic bridge budget even though the process is
    // healthy. Both built-in and fallback paths use the same bounded 30-second budget; the
    // built-in path additionally polls authenticated adapter readiness and process liveness.
    if adapter == "mesen2" {
        std::time::Duration::from_secs(30)
    } else {
        std::time::Duration::from_secs(15)
    }
}

pub(super) fn wait_for_adapter_ready<F>(
    link: &mut (dyn EmulatorLink + Send),
    timeout: std::time::Duration,
    mut check_processes: F,
) -> Result<serde_json::Value, String>
where
    F: FnMut() -> Result<(), String>,
{
    let started = std::time::Instant::now();
    loop {
        check_processes()?;
        let last_error = match link.call("status", serde_json::json!({})) {
            Ok(status)
                if status.get("connected").and_then(serde_json::Value::as_bool) == Some(true) =>
            {
                return Ok(status);
            }
            Ok(status) => format!("status did not report connected=true: {status}"),
            Err(error) => error.to_string(),
        };
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Err(format!(
                "adapter hello/status was not ready within {} ms; last error: {last_error}",
                timeout.as_millis()
            ));
        }
        std::thread::sleep(
            std::time::Duration::from_millis(100).min(timeout.saturating_sub(elapsed)),
        );
    }
}

pub(super) fn backend_endpoint_from_launch(outcome: &serde_json::Value) -> Option<String> {
    if let Some(path) = outcome
        .get("pine_socket")
        .and_then(serde_json::Value::as_str)
    {
        return Some(path.to_string());
    }
    if let Some(slot) = outcome.get("pine_slot").and_then(serde_json::Value::as_u64) {
        return Some(format!("pine:{slot}"));
    }
    for key in ["ws_port", "gdb_port", "arm9_gdb_port"] {
        if let Some(port) = outcome.get(key).and_then(serde_json::Value::as_u64) {
            return Some(format!("127.0.0.1:{port}"));
        }
    }
    None
}

pub(super) fn pc98_headless(a: &LaunchArgs) -> bool {
    !a.display.unwrap_or(false)
}

/// MAME/PC-98 leg of `make_launch`: spawn MAME + the GDB bridge; defaults the machine to pc9801rs.
pub(super) fn launch_mame(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let Some(binary) = emucap::launch::mame::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "MAME binary was not found; build it with adapters/mame-pc98/build.sh or set MAME_BIN" });
    };
    let headless = pc98_headless(a);
    let log = adapter_log_path("mame-pc98", port, "mame-pc98.log");
    let spec = emucap::launch::mame::Launch {
        binary: &binary,
        repo_root: &root,
        content: &a.content_path,
        flop2: a.content_path2.as_deref(),
        machine: "pc9801rs",
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        headless,
    };
    match emucap::launch::mame::launch(&spec) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "mame_pc98",
            "pid": launched.mame_pid,
            "mame_pid": launched.mame_pid,
            "bridge_pid": launched.bridge_pid,
            "bridge": launched.bridge_kind,
            "display": !headless,
            "gdb_port": launched.gdb_port,
            "port": port,
            "binary": binary.display().to_string(),
            "log": log.display().to_string(),
            "note": "MAME + GDB bridge 2-process launch. If MAME spawn fails after bridge spawn, the Rust launcher terminates that bridge.",
            "next_action": "launch returns after the adapter connects",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// Nintendo 64 leg of `make_launch`: run the native adapter against the pinned debugger core.
pub(super) fn launch_mupen64plus(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repo root not found; set EMUCAP_REPO_ROOT" });
    };
    let display = a.display.unwrap_or(false);
    let Some(binary) = mupen64plus_launch::resolve_binary(&root) else {
        return serde_json::json!({
            "launched": false,
            "reason": "emucap-mupen64plus binary not found",
            "next_action": "build emucap-mupen64plus and run adapters/mupen64plus/build.sh",
        });
    };
    let plugin_root = std::env::var_os("EMUCAP_M64P_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| mupen64plus_launch::default_root(&root));
    let host_build = match mupen64plus_launch::require_compatible_root(&root, &plugin_root, display)
    {
        Ok(build) => build,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "reason": "compatible debugger-enabled Mupen64Plus build not found",
                "error": error.to_string(),
                "next_action": "run adapters/mupen64plus/build.sh",
            })
        }
    };
    let content = Path::new(&a.content_path);
    let log = adapter_log_path("mupen64plus", port, "mupen64plus.log");
    let launch = mupen64plus_launch::Launch {
        binary: &binary,
        repo_root: &root,
        root: &plugin_root,
        content,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        build: Some(BUILD_HASH),
        session_token: token,
        runtime: Some(runtime),
        display,
    };
    match mupen64plus_launch::launch(&launch) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "mupen64plus",
            "pid": launched.pid,
            "display": display,
            "port": port,
            "binary": binary.display().to_string(),
            "plugin_root": plugin_root.display().to_string(),
            "emucap_home": launched.runtime_home.display().to_string(),
            "host_build": host_build,
            "log": log.display().to_string(),
            "isolation": "Mupen64Plus uses an emucap-owned per-port configuration and does not read or change the user's emulator settings.",
            "next_action": "launch returns after the adapter connects",
        }),
        Err(error) => serde_json::json!({ "launched": false, "error": error.to_string() }),
    }
}

/// DeSmuME/NDS leg of `make_launch`: spawn headless desmume-cli (ARM9/ARM7 GDB stubs) + the NDS GDB
/// bridge; a 2-process launch like MAME PC-98. Mirrors adapters/desmume-nds/launch.sh.
pub(super) fn launch_desmume_nds(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let Some(binary) = desmume_nds_launch::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "desmume-cli binary was not found; build it with adapters/desmume-nds/build.sh or set EMUCAP_DESMUME_BIN" });
    };
    let Some(bridge) = desmume_nds_launch::resolve_bridge(&root) else {
        return serde_json::json!({ "launched": false, "reason": "NDS bridge binary was not found; build emucap-desmume-nds-bridge in release mode or set EMUCAP_NDS_BRIDGE_BIN" });
    };
    let log = adapter_log_path("desmume-nds", port, "desmume-nds.log");
    let display = a.display.unwrap_or(false);
    let spec = desmume_nds_launch::Launch {
        binary: &binary,
        bridge: &bridge,
        content: &a.content_path,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        display,
    };
    match desmume_nds_launch::launch(&spec) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "desmume_nds",
            "pid": launched.desmume_pid,
            "desmume_pid": launched.desmume_pid,
            "bridge_pid": launched.bridge_pid,
            "arm9_gdb_port": launched.arm9_gdb_port,
            "arm7_gdb_port": launched.arm7_gdb_port,
            "display": display,
            "port": port,
            "binary": binary.display().to_string(),
            "bridge": bridge.display().to_string(),
            "log": log.display().to_string(),
            "note": "DeSmuME + NDS GDB bridge 2-process launch. If the bridge spawn fails after DeSmuME spawn, the Rust launcher terminates DeSmuME.",
            "next_action": "launch returns after the adapter connects",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// PPSSPP/PSP leg of `make_launch`: spawn headless PPSSPP (debugger WebSocket) + the PSP WS bridge;
/// a 2-process launch like NDS/MAME PC-98. Mirrors adapters/ppsspp/launch.sh.
pub(super) fn launch_ppsspp(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let display = a.display.unwrap_or(false);
    // display=true (HITL) launches the PPSSPPSDL GUI build (a real window a human sees and plays);
    // default headless launches PPSSPPHeadless. Both carry the same fork patch stack and speak the
    // same debugger WebSocket, so the agent drives either identically.
    let binary = if display {
        let Some(gui) = ppsspp_launch::resolve_gui_binary(&root) else {
            return serde_json::json!({ "launched": false, "reason": "PPSSPPSDL GUI binary was not found; for display=true, build the PPSSPPSDL target with adapters/ppsspp/build.sh or set EMUCAP_PPSSPP_GUI_BIN" });
        };
        gui
    } else {
        let Some(headless) = ppsspp_launch::resolve_binary(&root) else {
            return serde_json::json!({ "launched": false, "reason": "PPSSPPHeadless binary was not found; build it with adapters/ppsspp/build.sh or set EMUCAP_PPSSPP_BIN" });
        };
        headless
    };
    let Some(bridge) = ppsspp_launch::resolve_bridge(&root) else {
        return serde_json::json!({ "launched": false, "reason": "PSP bridge binary was not found; build emucap-ppsspp-bridge in release mode or set EMUCAP_PSP_BRIDGE_BIN" });
    };
    let log = adapter_log_path("ppsspp", port, "ppsspp.log");
    let spec = ppsspp_launch::Launch {
        binary: &binary,
        bridge: &bridge,
        content: &a.content_path,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        display,
    };
    match ppsspp_launch::launch(&spec) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "ppsspp",
            "pid": launched.ppsspp_pid,
            "ppsspp_pid": launched.ppsspp_pid,
            "bridge_pid": launched.bridge_pid,
            "ws_port": launched.ws_port,
            "display": display,
            "port": port,
            "binary": binary.display().to_string(),
            "bridge": bridge.display().to_string(),
            "log": log.display().to_string(),
            "note": if display {
                "Two-process launch with PPSSPP GUI and the PSP debugger WebSocket bridge. Opens a HITL window for human play through PPSSPP's native input mappings. The GUI boots without startBreak so the game runs immediately. On macOS, caffeinate keeps the display awake."
            } else {
                "Two-process launch with PPSSPPHeadless and the PSP debugger WebSocket bridge. PPSSPPHeadless runs without --timeout because that option terminates independently of WebSocket activity. If bridge spawn fails after PPSSPP starts, the Rust launcher terminates PPSSPP."
            },
            "next_action": "launch returns after the adapter connects",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// PCSX2/PS2 leg of `make_launch`: start the pinned PINE fork with an isolated data root and relay
/// its PINE socket through the Rust bridge.
pub(super) fn launch_pcsx2(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let Some(binary) = pcsx2_launch::resolve_binary(&root) else {
        return serde_json::json!({
            "launched": false,
            "kind": "pcsx2-patch-required",
            "reason": "compatible PCSX2 binary not found; run adapters/pcsx2/build.sh or set EMUCAP_PCSX2_BIN",
        });
    };
    let host_build = match pcsx2_launch::require_compatible_build(&root, &binary) {
        Ok(build) => build,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "kind": "pcsx2-patch-required",
                "error": error.to_string(),
                "next_action": "adapters/pcsx2/build.sh",
            });
        }
    };
    let Some(bridge) = pcsx2_launch::resolve_bridge(&root) else {
        return serde_json::json!({
            "launched": false,
            "reason": "PS2 bridge binary not found; run cargo build --release --bin emucap-pcsx2-bridge or set EMUCAP_PCSX2_BRIDGE_BIN",
        });
    };
    let bios = match pcsx2_launch::resolve_bios() {
        Ok(path) => path,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "reason": error.to_string(),
                "required_user_input": "Set EMUCAP_PCSX2_BIOS to an absolute path for a legally obtained PS2 BIOS file.",
            });
        }
    };
    let display = a.display.unwrap_or(false);
    let log = adapter_log_path("pcsx2", port, "pcsx2.log");
    let launch = pcsx2_launch::Launch {
        binary: &binary,
        bridge: &bridge,
        bios: &bios,
        content: &a.content_path,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        display,
    };
    match pcsx2_launch::launch(&launch) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "pcsx2",
            "system": "ps2",
            "pid": launched.pcsx2_pid,
            "pcsx2_pid": launched.pcsx2_pid,
            "bridge_pid": launched.bridge_pid,
            "pine_slot": launched.pine_slot,
            "pine_socket": launched.pine_socket.map(|path| path.display().to_string()),
            "data_root": launched.data_root.display().to_string(),
            "display": display,
            "port": port,
            "binary": binary.display().to_string(),
            "bridge": bridge.display().to_string(),
            "host_build": host_build,
            "bios": bios.display().to_string(),
            "log": log.display().to_string(),
            "isolation": "PCSX2 uses an emucap-owned per-port data root; the selected BIOS is referenced in place.",
            "next_action": "launch returns after the adapter connects",
        }),
        Err(error) => serde_json::json!({ "launched": false, "error": error.to_string() }),
    }
}

/// Flycast leg of `make_launch` (Dreamcast): resolve the built app and hand off with the isolated
/// config seeding. Mute defaults on and the GDB stub off (the exec-BP path enables it explicitly).
pub(super) fn launch_flycast(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(binary) = emucap::launch::flycast::resolve_binary() else {
        return serde_json::json!({ "launched": false, "reason": "Flycast binary was not found; build it with adapters/flycast/build.sh or set FLYCAST_APP to an executable or macOS Flycast.app path" });
    };
    let log = adapter_log_path("flycast", port, "flycast.log");
    let spec = emucap::launch::flycast::Launch {
        binary: &binary,
        content: &a.content_path,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        mute: true,
        gdb: false,
    };
    match emucap::launch::flycast::launch(&spec) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "flycast",
            "pid": pid,
            "port": port,
            "binary": binary.display().to_string(),
            "log": log.display().to_string(),
            "next_action": "launch returns after the adapter connects",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// Dolphin leg of `make_launch`: select the compatible no-GUI or DolphinQt fork, copy it into the
/// per-port runtime, and launch with an isolated `--user` directory.
pub(super) fn launch_dolphin(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    system: &str,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let display = a.display.unwrap_or(false);
    let Some(binary) = dolphin_launch::resolve_binary(&root, display) else {
        return serde_json::json!({
            "launched": false,
            "kind": "dolphin-patch-required",
            "reason": if display {
                "compatible DolphinQt binary not found; run adapters/dolphin/build.sh or set EMUCAP_DOLPHIN_GUI_BIN"
            } else {
                "compatible dolphin-emu-nogui binary not found; run adapters/dolphin/build.sh or set EMUCAP_DOLPHIN_HEADLESS_BIN"
            },
        });
    };
    let host_build = match dolphin_launch::require_compatible_build(&root, &binary) {
        Ok(build) => build,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "kind": "dolphin-patch-required",
                "error": error.to_string(),
                "next_action": if cfg!(windows) { "adapters/dolphin/build.ps1" } else { "adapters/dolphin/build.sh" },
            });
        }
    };
    let log = adapter_log_path("dolphin", port, "dolphin.log");
    let launch = dolphin_launch::Launch {
        binary: &binary,
        content: &a.content_path,
        system,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        display,
    };
    match dolphin_launch::launch(&launch) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "dolphin",
            "system": system,
            "pid": pid,
            "display": display,
            "port": port,
            "binary": binary.display().to_string(),
            "host_build": host_build,
            "log": log.display().to_string(),
            "emucap_home": emucap::launch::emu_home_dir("dolphin", port).display().to_string(),
            "isolation": "Dolphin runs from an emucap-owned portable copy with a per-port --user directory.",
            "next_action": "launch returns after the adapter connects",
        }),
        Err(error) => serde_json::json!({ "launched": false, "error": error.to_string() }),
    }
}

/// SNES/Mesen leg of `make_launch`: resolve the binary + adapter Lua and hand off to the orchestrator.
pub(super) fn launch_mesen(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    system: &str,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let Some(binary) = emucap::launch::mesen::resolve_binary(&root) else {
        return serde_json::json!({
            "launched": false,
            "kind": "mesen-patch-required",
            "reason": "compatible Mesen binary was not found; run adapters/mesen2/build.sh or build.ps1 on Windows"
        });
    };
    let host_build = match emucap::launch::mesen::require_compatible_build(&root, &binary) {
        Ok(build) => build,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "kind": "mesen-patch-required",
                "error": error.to_string(),
                "next_action": if cfg!(windows) { "adapters/mesen2/build.ps1" } else { "adapters/mesen2/build.sh" },
            });
        }
    };
    // 시스템별 얇은 엔트리 스크립트(SYS config 설정 후 emucap-core.lua를 require). Mesen은 SNES/GG/GB(+GBC)/GBA/NES 처리.
    let entry = match system {
        "gamegear" => "adapters/mesen2/emucap-sms.lua",
        "gb" | "gbc" => "adapters/mesen2/emucap-gb.lua",
        "gba" => "adapters/mesen2/emucap-gba.lua",
        "nes" => "adapters/mesen2/emucap-nes.lua",
        _ => "adapters/mesen2/emucap-snes.lua",
    };
    let lua = root.join(entry);
    let log = adapter_log_path("mesen2", port, "mesen.log");
    let spec = emucap::launch::mesen::Launch {
        binary: &binary,
        content: &a.content_path,
        lua: &lua,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        build: Some(BUILD_HASH),
        session_token: token,
        runtime: Some(runtime),
    };
    match emucap::launch::mesen::launch(&spec) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "mesen2",
            "pid": pid,
            "port": port,
            "binary": binary.display().to_string(),
            "host_build": host_build,
            "log": log.display().to_string(),
            "emucap_home": emucap::launch::emu_home_dir("mesen2", port).display().to_string(),
            "isolation": "Mesen runs from an emucap-owned portable copy; user settings.json is not edited.",
            "next_action": "launch returns after the adapter connects",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// Mednafen leg of `make_launch` (Saturn/PSX/PCE/MD): resolve the built fork (per-port copy unless
/// MEDNAFEN_BIN is pinned) and hand off with the force_module.
pub(super) fn launch_mednafen(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    module: Option<&'static str>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repository root was not found; set EMUCAP_REPO_ROOT" });
    };
    let Some((binary, explicit)) = emucap::launch::mednafen::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "Mednafen binary was not found; build it with adapters/mednafen/build.sh or set MEDNAFEN_BIN" });
    };
    let log = adapter_log_path("mednafen", port, "mednafen.log");
    let sound = a.sound.unwrap_or(false);
    let display = a.display.unwrap_or(false);
    let spec = emucap::launch::mednafen::Launch {
        binary: &binary,
        explicit_binary: explicit,
        content: &a.content_path,
        module,
        log_path: &log,
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        headless: !display,
        sound,
    };
    match emucap::launch::mednafen::launch(&spec) {
        Ok(pid) => serde_json::json!({
            "launched": true,
            "adapter": "mednafen",
            "module": module,
            "display": display,
            "sound": sound,
            "pid": pid,
            "port": port,
            "binary": binary.display().to_string(),
            "log": log.display().to_string(),
            "next_action": "launch returns after the adapter connects",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// IdentityMismatch is a recoverable listener state, so report it without discarding diagnostics.
/// The response stays disconnected, preserves the listening port, and describes the occupant.
pub(crate) fn occupied_graceful(
    occupant: &EmulatorIdentity,
    port: Option<u16>,
    token: Option<&str>,
) -> serde_json::Value {
    // 점유자가 *이 세션 소유*(echo 토큰의 cwd_hash 일치)인데도 mismatch면, 토큰파일 유실/스윕으로
    // 서버 토큰만 새로 발급된 경우다 — foreign이 아니라 stale-own. 재연결로는 못 고치고(파일이 이미
    // 새 토큰) save_state 후 relaunch가 복구다. foreign과 다르게 안내해야 무한 재연결 루프를 막는다.
    let stale_own = occupant
        .session_token
        .as_deref()
        .map(emucap::live::tcp::session_token_is_own)
        .unwrap_or(false);
    let recovery = if stale_own {
        "The emulator on this port belongs to this session, but its token no longer matches, likely because the token file was lost or swept. Reconnection cannot repair this. Preserve state if needed, then relaunch on the same port so the new token file is adopted."
    } else {
        "An emulator from another session occupies this port; inspect occupant. A stale connection from the same session is reclaimed automatically when MCP reconnects with the same token. For an unrelated orphan, verify occupant.content and system, terminate only that PID, and retry, or launch this session on another port. Do not use a broad kill."
    };
    let mut v = serde_json::json!({
        "connected": false,
        "occupied_by_foreign": !stale_own,
        "stale_own_token": stale_own,
        "listening_port": port,
        "first_tool_if_unknown": "bootstrap",
        "occupant": {
            "system": occupant.system,
            "adapter": occupant.adapter,
            "name": occupant.name,
            "content": occupant.content,
        },
        "recovery": recovery
    });
    enrich_link_status(&mut v, port, token, None);
    v
}
