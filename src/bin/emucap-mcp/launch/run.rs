use super::plan::*;
use super::*;

/// Actually launch an emulator (the `launch` tool): ensure the listener, capture this session's port +
/// token, pick the adapter from the system/extension, and dispatch to that adapter's Rust orchestrator.
/// Returns a JSON outcome. A system without a Rust orchestrator yet points back at launch_plan, so no
/// existing flow breaks. The per-adapter spawn logic lives in emucap::launch::<adapter>, not here.
pub(crate) fn make_launch(
    link: &mut (dyn EmulatorLink + Send),
    a: &LaunchArgs,
) -> serde_json::Value {
    let bootstrap = match make_bootstrap_value(link) {
        Ok(b) => b,
        Err(e) => return serde_json::json!({ "launched": false, "error": e.to_string() }),
    };
    let status = bootstrap
        .get("status")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let port_occupied = status
        .get("occupied_by_foreign")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || status
            .get("stale_own_token")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    if port_occupied {
        return serde_json::json!({
            "launched": false,
            "reason": "listening_port is already occupied; not launching another emulator on the same port",
            "status": status,
            "bootstrap": bootstrap,
            "next_action": status.get("recovery").cloned().unwrap_or_else(|| serde_json::json!("call status/bootstrap and resolve the occupied port before launch")),
        });
    }
    let Some(port) = bootstrap
        .get("listening_port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
    else {
        return serde_json::json!({ "launched": false, "reason": "listening_port 미확정 — status를 먼저 호출하라" });
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

    // A front connection normally means the current generation must be preserved. The only
    // launch-time exception is a capsule proving that the emulator exited while its exact bridge
    // remains alive: that bridge is an owned orphan, not a live emulator session, and is cleaned
    // below after the new launch has passed its non-mutating preflight.
    let already_connected = status
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let observed_lease = link.continuity().lease;
    let observed_cleanup_authorized = matches!(
        observed_lease.state,
        LeaseState::Held | LeaseState::Available
    );
    let exact_owned_bridge_orphan = previous.as_ref().is_some_and(|current| {
        current.process_state() == ProcessState::Exited
            && current.bridge_process_state() == Some(ProcessState::Alive)
    }) && observed_cleanup_authorized;
    if already_connected && !a.replace && !exact_owned_bridge_orphan {
        return serde_json::json!({
            "launched": false,
            "reason": "an emulator is already connected on this session's listening_port; not launching another (it would orphan the current one)",
            "connected_emulator": status.get("emulator_identity").cloned().unwrap_or(serde_json::Value::Null),
            "status": status,
            "next_action": "교체하려면 기존 에뮬을 정리한 뒤 다시 launch하라(save_state 후 connected_emulator를 참조해 그 PID만 종료; 광역 kill 금지). 연결이 이미 죽었으면 status가 connected=false가 된 뒤 재시도하면 새 연결로 자동 채택된다.",
        });
    }

    if !Path::new(&a.content_path).exists() {
        return serde_json::json!({
            "launched": false,
            "reason": "content_path does not exist",
            "content_path": &a.content_path,
            "next_action": "content_path를 확인한 뒤 launch_plan(content_path, system)을 다시 호출하라",
        });
    }

    let inference = infer_system(Some(&a.content_path), a.system.as_deref());
    let Some(system) = inference.get("system").and_then(|v| v.as_str()) else {
        return serde_json::json!({
            "launched": false,
            "reason": "시스템이 애매하다(CUE/CHD/BIN 등) — system을 지정해 다시 호출하라",
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
        }
    }
    if let Some(current) = previous.as_ref() {
        match (current.process_state(), current.bridge_process_state()) {
            (ProcessState::Alive, _) if !a.replace => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "current launch generation is still alive and may already be connected; reattach instead of launching a duplicate",
                    "runtime_instance": current.public_value_with_lease(&observed_lease),
                    "next_action": "status/bootstrap으로 같은 launch_id에 재부착하라. 의도적 교체만 replace=true로 다시 호출한다.",
                })
            }
            (ProcessState::Alive, Some(ProcessState::Unknown)) => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "current bridge process identity is unknown; refusing unsafe replacement",
                    "runtime_instance": current.public_value_with_lease(&observed_lease),
                    "next_action": "브리지 process identity를 확인하고 그 세대만 정리한 뒤 다시 launch하라.",
                })
            }
            (ProcessState::Unknown, _) => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "current process liveness is unknown; refusing duplicate launch or unsafe replacement",
                    "runtime_instance": current.public_value_with_lease(&observed_lease),
                    "next_action": "프로세스 identity를 확인하고 명시적으로 정리한 뒤 다시 launch하라.",
                })
            }
            (ProcessState::Exited, Some(ProcessState::Unknown)) => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "the emulator exited but bridge ownership is unknown; refusing unsafe cleanup",
                    "runtime_instance": current.public_value_with_lease(&observed_lease),
                    "next_action": "브리지 process identity를 확인하고 그 세대만 정리한 뒤 다시 launch하라.",
                })
            }
            _ => {}
        }
    }
    let lease = if let Some(current) = previous.as_ref() {
        match link.acquire_control_lease(&current.launch_id) {
            Ok(lease) => lease,
            Err(error) => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "failed to acquire the runtime generation lease",
                    "error": error.to_string(),
                })
            }
        }
    } else {
        observed_lease
    };
    let cleanup_authorized = lease.state == LeaseState::Held;
    if let Some(current) = previous.as_ref() {
        match current.process_state() {
            ProcessState::Alive if !a.replace => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "current launch generation is still alive and may already be connected; reattach instead of launching a duplicate",
                    "runtime_instance": current.public_value(),
                    "next_action": "status/bootstrap으로 같은 launch_id에 재부착하라. 의도적 교체만 replace=true로 다시 호출한다.",
                })
            }
            ProcessState::Alive => {
                if !cleanup_authorized {
                    return serde_json::json!({
                        "launched": false,
                        "reason": "current generation is controlled by another or unverifiable lease; refusing replacement",
                        "lease": lease,
                        "runtime_instance": current.public_value_with_lease(&lease),
                        "next_action": "현재 제어 임대가 반환되거나 같은 제어 세션임을 확인한 뒤 replace를 다시 요청하라.",
                    });
                }
                if let Err(e) = current.terminate_owned_processes() {
                    return serde_json::json!({
                        "launched": false,
                        "reason": "verified current generation could not be terminated for replacement",
                        "error": e.to_string(),
                        "runtime_instance": current.public_value(),
                    });
                }
            }
            ProcessState::Unknown => {
                return serde_json::json!({
                    "launched": false,
                    "reason": "current process liveness is unknown; refusing duplicate launch or unsafe replacement",
                    "runtime_instance": current.public_value(),
                    "next_action": "프로세스 identity를 확인하고 명시적으로 정리한 뒤 다시 launch하라.",
                })
            }
            ProcessState::Exited => {
                if !cleanup_authorized {
                    return serde_json::json!({
                        "launched": false,
                        "reason": "the exited generation is controlled by another or unverifiable lease; refusing cleanup or replacement",
                        "lease": lease,
                        "runtime_instance": current.public_value_with_lease(&lease),
                        "next_action": "제어 임대가 반환된 뒤 exact generation만 정리하거나 새 launch를 요청하라.",
                    });
                }
                match current.bridge_process_state() {
                    Some(ProcessState::Alive) => {
                        if let Err(e) = current.terminate_owned_processes() {
                            return serde_json::json!({
                                "launched": false,
                                "reason": "the emulator exited but its verified bridge could not be cleaned up",
                                "error": e.to_string(),
                                "runtime_instance": current.public_value(),
                            });
                        }
                    }
                    Some(ProcessState::Unknown) => {
                        return serde_json::json!({
                            "launched": false,
                            "reason": "the emulator exited but bridge ownership is unknown; refusing unsafe cleanup",
                            "runtime_instance": current.public_value_with_lease(&lease),
                            "next_action": "브리지 process identity를 확인하고 그 세대만 정리한 뒤 다시 launch하라.",
                        })
                    }
                    Some(ProcessState::Exited) | None => {}
                }
            }
        }
    } else if already_connected {
        return serde_json::json!({
            "launched": false,
            "reason": "connected legacy emulator has no runtime capsule; safe replacement ownership cannot be proven",
            "next_action": "기존 에뮬레이터를 명시적으로 정리한 뒤 status가 connected=false인지 확인하고 다시 launch하라.",
        });
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
    let direct_reclaim = match link.replace_reclaim_token(prepared.reclaim_token()) {
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
        "desmume_nds" => launch_desmume_nds(port, direct_reclaim, runtime, a),
        "ppsspp" => launch_ppsspp(port, direct_reclaim, runtime, a),
        "pcsx2" => launch_pcsx2(port, direct_reclaim, runtime, a),
        "dolphin" => launch_dolphin(port, direct_reclaim, runtime, system, a),
        _ => serde_json::json!({
            "launched": false,
            "reason": format!("{system} 시스템은 Rust 런처 대상이 아니다"),
        }),
    };
    if !outcome
        .get("launched")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
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
        let _ = prepared.abort();
        return serde_json::json!({
            "launched": false,
            "reason": "launcher returned success without an emulator PID",
            "launcher_outcome": outcome,
        });
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
        let _ = prepared.abort();
        return serde_json::json!({
            "launched": false,
            "reason": "a launch process was not verifiably alive before the runtime generation became current",
            "emulator_process_state": emulator_state,
            "bridge_process_state": bridge_state,
            "launcher_outcome": outcome,
        });
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
            let _ = prepared.abort();
            return serde_json::json!({
                "launched": false,
                "reason": "adapter did not become ready",
                "error": error,
                "launcher_outcome": outcome,
            });
        }
    };
    if let Err(e) = prepared.commit(&manifest) {
        let _ = manifest.terminate_owned_processes();
        let _ = prepared.abort();
        return serde_json::json!({
            "launched": false,
            "reason": "failed to publish runtime current generation",
            "error": e.to_string(),
        });
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
            serde_json::json!("status로 methods, memory_types, state를 확인한 뒤 작업을 시작하라"),
        );
    }
    outcome
}

pub(super) fn adapter_ready_timeout(adapter: &str) -> std::time::Duration {
    // Mesen starts an Avalonia window and then loads the command-line Lua script. On macOS a cold
    // display wake can make that path exceed the generic bridge budget even though the process is
    // healthy. The fallback launcher already allows 20 seconds; the built-in path keeps a little
    // more margin while continuing to poll liveness and failing with a bounded deadline.
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
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some(binary) = emucap::launch::mame::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "MAME 바이너리 미발견 — adapters/mame-pc98/build.sh로 빌드하거나 MAME_BIN을 설정하라" });
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
            "next_action": "adapter가 연결되면 launch가 반환한다",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
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
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some(binary) = desmume_nds_launch::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "desmume-cli 바이너리 미발견 — adapters/desmume-nds/build.sh로 빌드하거나 EMUCAP_DESMUME_BIN을 설정하라" });
    };
    let Some(bridge) = desmume_nds_launch::resolve_bridge(&root) else {
        return serde_json::json!({ "launched": false, "reason": "NDS bridge 바이너리 미발견 — cargo build --release --bin emucap-desmume-nds-bridge 하거나 EMUCAP_NDS_BRIDGE_BIN을 설정하라" });
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
            "next_action": "adapter가 연결되면 launch가 반환한다",
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
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let display = a.display.unwrap_or(false);
    // display=true (HITL) launches the PPSSPPSDL GUI build (a real window a human sees and plays);
    // default headless launches PPSSPPHeadless. Both carry the same fork patch stack and speak the
    // same debugger WebSocket, so the agent drives either identically.
    let binary = if display {
        let Some(gui) = ppsspp_launch::resolve_gui_binary(&root) else {
            return serde_json::json!({ "launched": false, "reason": "PPSSPPSDL(GUI) 바이너리 미발견 — display=true는 adapters/ppsspp/build.sh(PPSSPPSDL 타깃)로 빌드하거나 EMUCAP_PPSSPP_GUI_BIN을 설정해야 한다" });
        };
        gui
    } else {
        let Some(headless) = ppsspp_launch::resolve_binary(&root) else {
            return serde_json::json!({ "launched": false, "reason": "PPSSPPHeadless 바이너리 미발견 — adapters/ppsspp/build.sh로 빌드하거나 EMUCAP_PPSSPP_BIN을 설정하라" });
        };
        headless
    };
    let Some(bridge) = ppsspp_launch::resolve_bridge(&root) else {
        return serde_json::json!({ "launched": false, "reason": "PSP bridge 바이너리 미발견 — cargo build --release --bin emucap-ppsspp-bridge 하거나 EMUCAP_PSP_BRIDGE_BIN을 설정하라" });
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
                "PPSSPP(GUI) + PSP debugger-WebSocket bridge 2-process launch. HITL 창이 열린다(사람이 보고 PPSSPP 자체 키/게임패드 매핑으로 플레이). GUI는 startBreak 없이 부팅되어 게임이 바로 돈다. macOS는 caffeinate로 디스플레이를 깨워둔다."
            } else {
                "PPSSPP + PSP debugger-WebSocket bridge 2-process launch. PPSSPPHeadless는 --timeout 없이 뜬다(지정하면 WS 활동과 무관하게 강제 종료됨). If the bridge spawn fails after PPSSPP spawn, the Rust launcher terminates PPSSPP."
            },
            "next_action": "adapter가 연결되면 launch가 반환한다",
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
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
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
            "next_action": "adapter가 연결되면 launch가 반환한다",
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
        return serde_json::json!({ "launched": false, "reason": "Flycast 바이너리 미발견 — adapters/flycast/build.sh로 빌드하거나 FLYCAST_APP을 실행파일 또는 macOS Flycast.app 경로로 설정하라" });
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
            "next_action": "adapter가 연결되면 launch가 반환한다",
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
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
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
            "next_action": "adapter가 연결되면 launch가 반환한다",
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
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some(binary) = emucap::launch::mesen::resolve_binary(&root) else {
        return serde_json::json!({
            "launched": false,
            "kind": "mesen-patch-required",
            "reason": "compatible Mesen 바이너리 미발견 — adapters/mesen2/build.sh(Windows: build.ps1)를 실행하라"
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
            "next_action": "adapter가 연결되면 launch가 반환한다",
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
        return serde_json::json!({ "launched": false, "error": "emucap repo root 미발견 — EMUCAP_REPO_ROOT를 설정하라" });
    };
    let Some((binary, explicit)) = emucap::launch::mednafen::resolve_binary(&root) else {
        return serde_json::json!({ "launched": false, "reason": "Mednafen 바이너리 미발견 — adapters/mednafen/build.sh로 빌드하거나 MEDNAFEN_BIN을 설정하라" });
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
            "next_action": "adapter가 연결되면 launch가 반환한다",
        }),
        Err(e) => serde_json::json!({ "launched": false, "error": e.to_string() }),
    }
}

/// 진입점이 IdentityMismatch(포트를 다른 세션 에뮬이 점유)일 때 하드에러 대신 줄 graceful 응답.
/// 계약: 미연결처럼 connected=false + listening_port + runtime_paths를 주고, 점유자 진단·복구절차를 더한다.
/// 그래야 새 세션이 잠기지 않고 자기 에뮬을 올바른 포트로 띄우거나 orphan을 정리할 수 있다.
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
        "이 포트의 에뮬레이터는 *이 세션 소유*인데 토큰이 어긋났다(토큰파일 유실/스윕 추정). 재연결로는 안 고쳐진다 — 필요하면 save_state 후 launch 도구로 같은 포트에 재기동하면 새 토큰파일을 읽어 매칭된다."
    } else {
        "이 포트를 다른 세션의 에뮬레이터가 점유 중이다(occupant 참조). 같은 세션의 stale 연결이면 /mcp 재연결 시 토큰이 재사용돼 자동 reclaim된다. 무관한 orphan이면 occupant.content/system을 확인해 그 PID만 종료(pgrep -f <content> → kill; 광역 kill 금지) 후 재시도하거나, 이 세션 에뮬을 다른 포트로 띄운다."
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
