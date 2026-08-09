use super::plan::adapter_log_path;
use super::*;
use crate::args::LaunchExecutionProfileArgs;
use emucap::live::runtime::ExecutionProfileIdentity;

/// Mesen leg of `make_launch`: resolve the binary and system adapter, then hand off to the
/// cross-platform launcher.
pub(super) fn launch_mesen(
    port: u16,
    token: Option<&str>,
    runtime: RuntimeEnv<'_>,
    system: &str,
    args: &LaunchArgs,
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
    let entry = match system {
        "gamegear" => "adapters/mesen2/emucap-sms.lua",
        "gb" | "gbc" => "adapters/mesen2/emucap-gb.lua",
        "gba" => "adapters/mesen2/emucap-gba.lua",
        "nes" => "adapters/mesen2/emucap-nes.lua",
        _ => "adapters/mesen2/emucap-snes.lua",
    };
    let lua = root.join(entry);
    let log = adapter_log_path("mesen2", port, "mesen.log");
    let repeatable = args.execution_profile == Some(LaunchExecutionProfileArgs::Repeatable);
    let spec = emucap::launch::mesen::Launch {
        binary: &binary,
        content: &args.content_path,
        lua: &lua,
        log_path: &log,
        port,
        name: args.name.as_deref(),
        build: Some(BUILD_HASH),
        session_token: token,
        runtime: Some(runtime),
        start_frozen: args.start_frozen,
        repeatable,
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
            "execution_profile": repeatable.then(|| ExecutionProfileIdentity {
                id: emucap::launch::mesen::REPEATABLE_PROFILE_ID.into(),
                conditions_sha256: emucap::launch::mesen::REPEATABLE_CONDITIONS_SHA256.into(),
            }),
            "next_action": "launch returns after the adapter connects",
        }),
        Err(error) => serde_json::json!({ "launched": false, "error": error.to_string() }),
    }
}
