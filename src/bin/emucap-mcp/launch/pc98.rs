use super::plan::adapter_log_path;
use super::*;

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
    let sound = a.sound.unwrap_or(false);
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
        sound,
        cbus0: a.pc98_sound_board.map(|board| board.mame_slot()),
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
            "sound": sound,
            "pc98_sound_board": a.pc98_sound_board.map(|board| board.mame_slot()),
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

pub(super) fn launch_np2kai(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    a: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({"launched":false, "error":"emucap repository root was not found; set EMUCAP_REPO_ROOT"});
    };
    let request = np2kai_launch::Launch {
        repo_root: &root,
        content: Path::new(&a.content_path),
        port,
        name: a.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        start_frozen: a.start_frozen,
    };
    match np2kai_launch::launch(&request) {
        Ok(launched) => serde_json::json!({
            "launched":true,
            "adapter":"np2kai",
            "backend":"np2kai-libretro",
            "pid":launched.pid,
            "port":port,
            "display":false,
            "sound":false,
            "start_frozen":a.start_frozen,
            "binary":launched.frontend.display().to_string(),
            "core":launched.core.display().to_string(),
            "firmware_root":launched.firmware.display().to_string(),
            "runtime_home":launched.runtime_home.display().to_string(),
            "mounted_content":launched.mounted_content.display().to_string(),
            "source_content_sha256":launched.source_content_sha256,
            "host_build":launched.identity,
            "next_action":"launch returns after the adapter connects"
        }),
        Err(error) => serde_json::json!({"launched":false, "error":error.to_string()}),
    }
}
