use super::*;

#[test]
fn canonical_pc98_keys_keep_number_row_and_keypad_distinct() {
    assert_eq!(canonical_button("0"), Some("0"));
    assert_eq!(canonical_button("0 (pad)"), Some("kp0"));
    assert_eq!(canonical_button("numpad6"), Some("kp6"));
    assert_ne!(key_id("0"), key_id("kp0"));
}

#[test]
fn input_aliases_are_stable_and_unknown_keys_fail() {
    let buttons = normalize_buttons(Some(&json!(["return", "KP_2", "Esc"]))).unwrap();
    assert_eq!(
        buttons,
        BTreeSet::from(["enter".into(), "escape".into(), "kp2".into()])
    );
    assert!(normalize_buttons(Some(&json!(["mame raw field"]))).is_err());
}

#[test]
fn mouse_buttons_are_callable_controls_not_keyboard_keys() {
    let buttons = normalize_buttons(Some(&json!(["left_click", "mouse_right"]))).unwrap();
    assert_eq!(mouse_button_mask(&buttons), 3);
    assert!(key_ids(&buttons).is_empty());
    assert!(normalize_buttons(Some(&json!(["middle_click"]))).is_err());
}

#[test]
fn breakpoint_filters_reject_lossy_or_non_authoritative_values() {
    assert!(debug::breakpoint_pc_range(&json!({"pc_min":"0x100000000"})).is_err());
    assert!(debug::breakpoint_pc_range(&json!({"pc_min":10,"pc_max":9})).is_err());
    assert!(debug::breakpoint_value_filter(&json!({"value":1}), "read").is_err());
    assert!(
        debug::breakpoint_value_filter(&json!({"value":"0x100","value_len":1}), "write").is_err()
    );
    assert_eq!(
        debug::breakpoint_value_filter(
            &json!({"value":"0xab","value_mask":"0xf0","value_len":1}),
            "write"
        )
        .unwrap(),
        (true, 0xab, 0xf0, 1)
    );
    assert_eq!(debug::authoritative_event_value(BP_WRITE, 0xab), Some(0xab));
    assert_eq!(debug::authoritative_event_value(BP_READ, 0xab), None);
    assert_eq!(debug::authoritative_event_value(BP_ACCESS, 0xab), None);
}

#[test]
fn frame_counts_accept_hex_and_reject_unbounded_work() {
    assert_eq!(frame_count(&json!({"count":"0x10"})).unwrap(), 16);
    assert!(frame_count(&json!({"count":0})).is_err());
    assert!(frame_count(&json!({"count":MAX_SYNC_FRAMES + 1})).is_err());
}

#[test]
fn state_identity_uses_a_stable_format_name() {
    let identity = StateIdentity {
        format: "np2kai-libretro-state".into(),
        system: "pc98".into(),
        target_os: "test".into(),
        target_arch: "test".into(),
        media_sha256: "m".into(),
        firmware_sha256: "f".into(),
        core_sha256: "c".into(),
        upstream_commit: "u".into(),
        patchset_sha256: "p".into(),
        build_profile: "license-clean-libretro".into(),
        frontend_build: "b".into(),
        state_sha256: "s".into(),
    };
    let encoded = serde_json::to_value(identity).unwrap();
    assert_eq!(encoded["format"], "np2kai-libretro-state");
    assert!(encoded.to_string().find("v1").is_none());
}

#[test]
fn np2kai_contract_advertisement_is_validated() {
    let hello = json!({"contracts": crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS)});
    let methods = METHODS
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &crate::contracts::advertisement_from_hello(&hello),
        Some("np2kai-libretro"),
        Some("pc98"),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
}

#[test]
fn debug_abi_matches_the_pinned_c_layout() {
    assert_eq!(std::mem::size_of::<NativeRegisters>(), 60);
    assert_eq!(std::mem::size_of::<NativeBreakpoint>(), 72);
    assert_eq!(std::mem::size_of::<NativeEvent>(), 96);
    assert_eq!(std::mem::size_of::<NativeTrace>(), 328);
}

#[test]
fn memory_regions_fail_loud_at_their_exclusive_end() {
    assert_eq!(
        debug::region_address(&json!({"memory_type":"tvram", "address":"0x3fff"}), 1).unwrap(),
        0xa3fff
    );
    assert!(debug::region_address(&json!({"memory_type":"tvram", "address":"0x3fff"}), 2).is_err());
    assert!(debug::region_address(&json!({"address":0}), 0).is_err());
}

#[test]
fn snapshot_and_register_parameters_use_shared_debug_names() {
    let snapshots = debug::parse_snapshots(Some(&json!(["tvram:0x10:0x20"]))).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].memory_type, "tvram");
    assert!(debug::parse_snapshots(Some(&json!(["tvram:0x3fff:2"]))).is_err());
    assert_eq!(debug::normalize_register("cpu.sp").unwrap(), ("esp", 4));
    assert!(debug::normalize_register("made_up").is_err());
}

#[test]
fn np2kai_exposes_the_complete_mame_pc98_method_surface() {
    let np2kai = METHODS.iter().copied().collect::<BTreeSet<_>>();
    let mame = crate::pc98_bridge::METHODS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(np2kai, mame);
}

#[test]
fn controlled_start_advertisement_matches_the_common_entry_boundary() {
    let launch_start = operations::launch_start_value(true);
    assert_eq!(launch_start["requested_frozen"], true);
    assert_eq!(launch_start["controlled"], true);
    assert_eq!(launch_start["boundary"], "pre_first_instruction");

    let identity = crate::live::link::EmulatorIdentity::from_hello(&json!({
        "host_features": HOST_FEATURES
    }));
    assert!(identity
        .host_features
        .iter()
        .any(|feature| feature == "controlled_start"));

    let ordinary_start = operations::launch_start_value(false);
    assert_eq!(ordinary_start["controlled"], false);
    assert!(ordinary_start["boundary"].is_null());
}

#[test]
#[ignore = "requires a built NP2kai core, PC-98 firmware, and an HDI test image"]
fn live_exec_resume_and_media_rollback_contracts() {
    let mut fixture = live_np2kai_host();
    let host = &mut fixture.host;

    let hello = host.hello().unwrap();
    assert_eq!(hello["launch_start"]["boundary"], "pre_first_instruction");
    assert!(hello["host_features"]
        .as_array()
        .unwrap()
        .iter()
        .any(|feature| feature == "controlled_start"));
    assert_eq!(host.status().unwrap()["state"], "frozen");

    host.step(&json!({"count": 1, "unit": "frames"})).unwrap();
    let pc_before = host.get_state().unwrap()["state"]["cpu.pc"]
        .as_u64()
        .unwrap();
    let breakpoint = host
        .set_breakpoint(&json!({
            "kind": "exec",
            "memory_type": "physical",
            "start": pc_before,
            "pause_on_hit": true
        }))
        .unwrap();
    let breakpoint_id = breakpoint["id"].as_u64().unwrap();

    let stepped = host.step_instructions(&json!({"count": 10})).unwrap();
    assert_eq!(stepped["status"], "interrupted");
    assert_eq!(stepped["reason"], "breakpoint");
    assert_eq!(stepped["completed"], 0);
    assert_eq!(
        host.get_state().unwrap()["state"]["cpu.pc"],
        pc_before,
        "the instruction at a pausing exec breakpoint must not execute"
    );

    let events = host
        .poll_events(&json!({"breakpoint_id": breakpoint_id}))
        .unwrap();
    assert!(events["events"].as_array().unwrap().iter().any(|event| {
        event["breakpoint_id"].as_u64() == Some(breakpoint_id)
            && event["paused"].as_bool() == Some(true)
    }));

    let resumed = host.step_instructions(&json!({"count": 1})).unwrap();
    assert_eq!(resumed["status"], "completed");
    assert_eq!(resumed["completed"], 1);
    assert_ne!(
        host.get_state().unwrap()["state"]["cpu.pc"],
        pc_before,
        "the stopped instruction must execute once while the breakpoint remains armed"
    );
    assert!(host.list_breakpoints().unwrap()["breakpoints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|breakpoint| breakpoint["id"].as_u64() == Some(breakpoint_id)));

    let previous_c = path_cstring(&host.content_path).unwrap();
    let alternate = fixture.runtime.path().join("injected-media-effect.hdi");
    fs::copy(&fixture.hdi, &alternate).unwrap();
    let alternate_c = path_cstring(&alternate).unwrap();
    assert_ne!(
        unsafe { (host.api.debug_change_hdd)(alternate_c.as_ptr()) },
        0,
        "fault injection must first produce an observable media effect"
    );
    assert_eq!(
        host.current_hdd_bytes().as_deref(),
        Some(alternate_c.as_bytes())
    );
    let error = host.media_change_failure("injected post-effect failure", &previous_c);
    assert!(error.to_string().contains("rollback=restored"));
    assert_eq!(
        host.current_hdd_bytes().as_deref(),
        Some(previous_c.as_bytes())
    );
}

struct LiveNp2kaiFixture {
    host: Np2kaiHost,
    runtime: tempfile::TempDir,
    hdi: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn live_np2kai_host() -> LiveNp2kaiFixture {
    static LIVE_CORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = LIVE_CORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let core = PathBuf::from(
        std::env::var("EMUCAP_NP2KAI_TEST_CORE")
            .expect("set EMUCAP_NP2KAI_TEST_CORE to the pinned libretro core"),
    );
    let firmware = PathBuf::from(
        std::env::var("EMUCAP_NP2KAI_TEST_FIRMWARE")
            .expect("set EMUCAP_NP2KAI_TEST_FIRMWARE to a PC-98 firmware directory"),
    );
    let hdi = PathBuf::from(
        std::env::var("EMUCAP_NP2KAI_TEST_HDI")
            .expect("set EMUCAP_NP2KAI_TEST_HDI to an HDI test image"),
    );
    assert_eq!(
        std::env::var("EMUCAP_START_FROZEN").ok().as_deref(),
        Some("1"),
        "run this contract test with EMUCAP_START_FROZEN=1"
    );

    let sidecar_path = core
        .parent()
        .and_then(Path::parent)
        .expect("core path must be below the pinned source tree")
        .join("emucap-np2kai-build.json");
    let sidecar: Value =
        serde_json::from_slice(&fs::read(&sidecar_path).expect("read the NP2kai build sidecar"))
            .expect("parse the NP2kai build sidecar");
    let field = |name: &str| {
        sidecar[name]
            .as_str()
            .unwrap_or_else(|| panic!("build sidecar lacks {name}"))
    };
    let runtime = tempfile::tempdir().unwrap();
    let host = Np2kaiHost::open(
        &hdi,
        &core,
        &firmware,
        runtime.path(),
        field("commit"),
        field("patchset_sha256"),
        field("build_profile"),
    )
    .unwrap();
    LiveNp2kaiFixture {
        host,
        runtime,
        hdi,
        _lock: lock,
    }
}
