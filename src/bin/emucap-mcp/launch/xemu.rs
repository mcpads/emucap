use super::*;

pub(super) fn precondition(root: &Path) -> serde_json::Value {
    let binary = xemu_launch::resolve_binary(root);
    let bridge = xemu_launch::resolve_bridge(root);
    let build = binary
        .as_deref()
        .map(|path| xemu_launch::require_compatible_build(root, path));
    let firmware = xemu_launch::resolve_firmware();
    serde_json::json!({
        "available": binary.is_some()
            && bridge.is_some()
            && build.as_ref().is_some_and(Result::is_ok)
            && firmware.is_ok(),
        "path": binary.as_ref().map(|path| path.display().to_string()),
        "bridge": bridge.as_ref().map(|path| path.display().to_string()),
        "bridge_available": bridge.is_some(),
        "host_build": build.as_ref().and_then(|result| result.as_ref().ok()),
        "build_error": build.and_then(Result::err).map(|error| error.to_string()),
        "firmware_available": firmware.is_ok(),
        "firmware_error": firmware.err().map(|error| error.to_string()),
        "source": "EMUCAP_XEMU_BIN / pinned repo build; EMUCAP_XEMU_BRIDGE_BIN / installed emucap-xemu-bridge; EMUCAP_XEMU_FIRMWARE / managed inventory",
    })
}

/// Original Xbox leg of `make_launch`: stage one isolated machine generation, then connect the
/// pinned xemu QMP extension and native i386 GDB stub through the Rust bridge.
pub(super) fn launch_xemu(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    args: &LaunchArgs,
) -> serde_json::Value {
    let Some(root) = find_repo_root() else {
        return serde_json::json!({"launched":false, "error":"emucap repository root was not found; set EMUCAP_REPO_ROOT"});
    };
    let Some(binary) = xemu_launch::resolve_binary(&root) else {
        return serde_json::json!({
            "launched": false,
            "kind": "xemu-patch-required",
            "reason": "compatible xemu binary not found; run adapters/xemu/build.sh or set EMUCAP_XEMU_BIN",
        });
    };
    let host_build = match xemu_launch::require_compatible_build(&root, &binary) {
        Ok(build) => build,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "kind": "xemu-patch-required",
                "error": error.to_string(),
                "next_action": "adapters/xemu/build.sh",
            });
        }
    };
    let Some(bridge) = xemu_launch::resolve_bridge(&root) else {
        return serde_json::json!({
            "launched": false,
            "reason": "original Xbox bridge binary not found; build emucap-xemu-bridge",
        });
    };
    let firmware = match xemu_launch::resolve_firmware() {
        Ok(firmware) => firmware,
        Err(error) => {
            return serde_json::json!({
                "launched": false,
                "reason": error.to_string(),
                "required_user_input": "Provide legally obtained original Xbox machine files in one inventory directory and set EMUCAP_XEMU_FIRMWARE if the managed inventory is not used.",
            });
        }
    };
    let display = args.display.unwrap_or(false);
    let sound = args.sound.unwrap_or(false);
    let launch = xemu_launch::Launch {
        binary: &binary,
        bridge: &bridge,
        content: Path::new(&args.content_path),
        firmware: &firmware,
        host_build: &host_build,
        port,
        name: args.name.as_deref(),
        session_token: token,
        runtime: Some(runtime),
        display,
        sound,
        start_frozen: args.start_frozen,
    };
    match xemu_launch::launch(&launch) {
        Ok(launched) => serde_json::json!({
            "launched": true,
            "adapter": "xemu",
            "system": "xbox",
            "pid": launched.xemu_pid,
            "xemu_pid": launched.xemu_pid,
            "bridge_pid": launched.bridge_pid,
            "qmp_port": launched.qmp_port,
            "gdb_port": launched.gdb_port,
            "port": port,
            "display": display,
            "sound": sound,
            "start_frozen": args.start_frozen,
            "binary": binary.display().to_string(),
            "bridge": bridge.display().to_string(),
            "host_build": host_build,
            "runtime_home": launched.runtime_home.display().to_string(),
            "settings": launched.settings.display().to_string(),
            "machine_inputs": {
                "mcpx": {"sha256": launched.mcpx_identity.sha256},
                "flash": {"sha256": launched.flash_identity.sha256},
                "hdd": {"template_sha256": launched.hdd_template_identity.sha256, "path": launched.hdd.display().to_string()},
                "eeprom": {"initial_sha256": launched.eeprom_initial_identity.sha256, "path": launched.eeprom.display().to_string()},
            },
            "log": launched.log.display().to_string(),
            "isolation": "xemu uses an emucap-owned launch generation with copied machine files, writable HDD and EEPROM, and a private settings file; the source inventory and user profile are not modified.",
            "next_action": "launch returns after QMP and GDB readiness are verified by the adapter",
        }),
        Err(error) => serde_json::json!({"launched":false, "error":error.to_string()}),
    }
}
