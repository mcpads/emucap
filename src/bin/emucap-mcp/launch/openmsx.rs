use super::plan::adapter_log_path;
use super::*;

pub(super) fn precondition(root: &Path) -> serde_json::Value {
    let binary = openmsx_launch::resolve_binary(root);
    let bridge = openmsx_launch::resolve_bridge(root);
    let build = binary
        .as_deref()
        .map(|path| openmsx_launch::require_compatible_build(root, path));
    serde_json::json!({
        "available": binary.is_some()
            && bridge.is_some()
            && build.as_ref().is_some_and(Result::is_ok),
        "path": binary.as_ref().map(|path| path.display().to_string()),
        "bridge": bridge.as_ref().map(|path| path.display().to_string()),
        "bridge_available": bridge.is_some(),
        "host_build": build.as_ref().and_then(|result| result.as_ref().ok()),
        "error": build.and_then(Result::err).map(|error| error.to_string()),
        "source": "EMUCAP_OPENMSX_BIN / pinned repo build; EMUCAP_OPENMSX_BRIDGE_BIN / installed emucap-openmsx-bridge",
    })
}

/// MSX leg of `make_launch`: run pinned stock openMSX behind the separate XML bridge.
pub(super) fn launch_openmsx(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    system: &str,
    args: &LaunchArgs,
) -> serde_json::Value {
    let Some(profile) = openmsx_launch::OpenMsxProfile::for_system(system) else {
        return serde_json::json!({
            "launched": false,
            "reason": format!("unsupported openMSX system profile: {system}"),
        });
    };
    let Some(root) = find_repo_root() else {
        return serde_json::json!({ "launched": false, "error": "emucap repo root not found; set EMUCAP_REPO_ROOT" });
    };
    let Some(binary) = openmsx_launch::resolve_binary(&root) else {
        return serde_json::json!({
            "launched": false,
            "reason": "pinned openMSX binary not found",
            "next_action": "run adapters/openmsx/build.sh or set EMUCAP_OPENMSX_BIN to a compatible pinned build",
        });
    };
    let Some(bridge) = openmsx_launch::resolve_bridge(&root) else {
        return serde_json::json!({
            "launched": false,
            "reason": "openMSX bridge binary not found",
            "next_action": "build emucap-openmsx-bridge with cargo build --release or set EMUCAP_OPENMSX_BRIDGE_BIN",
        });
    };
    let host_build = match openmsx_launch::require_compatible_build(&root, &binary) {
        Ok(build) => build,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "reason": "compatible pinned stock openMSX build not found",
                "error": error.to_string(),
                "next_action": "run adapters/openmsx/build.sh",
            })
        }
    };
    let content = Path::new(&args.content_path);
    let log = adapter_log_path("openmsx", port, "openmsx.log");
    let display = args.display.unwrap_or(false);
    let launch = openmsx_launch::Launch {
        binary: &binary,
        bridge: &bridge,
        repo_root: &root,
        system,
        content,
        log_path: &log,
        port,
        name: args.name.as_deref(),
        session_token: token,
        build: Some(BUILD_HASH),
        runtime: Some(runtime),
        display,
    };
    match openmsx_launch::launch(&launch) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "openmsx",
            "pid": launched.openmsx_pid,
            "openmsx_pid": launched.openmsx_pid,
            "bridge_pid": launched.bridge_pid,
            "display": display,
            "port": port,
            "binary": binary.display().to_string(),
            "bridge": bridge.display().to_string(),
            "emucap_home": launched.runtime_home.display().to_string(),
            "host_build": host_build,
            "log": log.display().to_string(),
            "isolation": "openMSX uses an emucap-owned HOME, user-data file pool, and generation media tree. Operator firmware and original mutable media are never mounted writable.",
            "profile": {
                "system": profile.system(),
                "machine": profile.machine(),
                "machine_type": profile.machine_type(),
            },
            "next_action": "launch returns after the adapter connects",
        }),
        Err(error) => serde_json::json!({ "launched": false, "error": error.to_string() }),
    }
}
