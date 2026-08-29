use super::plan::{env_path_matches, path_matches_candidates, simple_binary_precondition};
use super::*;

pub(super) fn mame_binary_precondition_from(
    root: &Path,
    resolved: Option<PathBuf>,
) -> serde_json::Value {
    let repo_work = root.join("adapters/mame-pc98/work");
    simple_binary_precondition(resolved, |path| {
        if env_path_matches("MAME_BIN", path) {
            "MAME_BIN"
        } else if path.starts_with(&repo_work) {
            "repo_build"
        } else if path_matches_candidates(path, mame_launch::default_install_candidates()) {
            "default_install"
        } else {
            "PATH"
        }
    })
}

pub(super) fn mame_binary_precondition(root: &Path) -> serde_json::Value {
    mame_binary_precondition_from(root, mame_launch::resolve_binary(root))
}

pub(super) fn mame_bridge_precondition(root: &Path) -> serde_json::Value {
    match mame_launch::resolve_bridge_runtime(root) {
        Ok(runtime) => serde_json::json!({
            "available": true,
            "kind": runtime.kind,
            "program": runtime.program.display().to_string(),
        }),
        Err(e) => serde_json::json!({
            "available": false,
            "error": e.to_string(),
            "source": "EMUCAP_PC98_BRIDGE_BIN / installed emucap-mame-pc98-bridge",
        }),
    }
}

pub(super) fn np2kai_precondition(root: &Path) -> serde_json::Value {
    let frontend = np2kai_launch::resolve_frontend(root);
    let core = np2kai_launch::require_compatible_core(root);
    let firmware = np2kai_launch::default_firmware_root();
    serde_json::json!({
        "available": frontend.is_some() && core.is_ok() && firmware.is_dir(),
        "path": frontend.map(|path| path.display().to_string()),
        "core": core.as_ref().ok().map(|bundle| bundle.path.display().to_string()),
        "host_build": core.as_ref().ok().map(|bundle| &bundle.identity),
        "core_error": core.err().map(|error| error.to_string()),
        "firmware_root": firmware.display().to_string(),
        "firmware_available": firmware.is_dir(),
        "source": "EMUCAP_NP2KAI_BIN / installed emucap-np2kai; EMUCAP_NP2KAI_CORE / pinned repo build; EMUCAP_NP2KAI_FIRMWARE / operator firmware"
    })
}
