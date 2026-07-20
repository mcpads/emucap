use super::*;

fn manifest(prepared: &PreparedGeneration) -> CurrentManifest {
    prepared.manifest(ManifestSpec {
        adapter: "mesen2".into(),
        system: "snes".into(),
        content: "/games/test.sfc".into(),
        emulator_pid: std::process::id(),
        bridge_pid: None,
        backend_endpoint: None,
        build: Some("test-build".into()),
    })
}

#[test]
fn prepare_writes_private_auth_without_replacing_current() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let prepared = store.prepare(47800).unwrap();

    assert!(store.read_current(47800).unwrap().is_none());
    assert_eq!(
        store
            .read_auth(47800, prepared.launch_id())
            .unwrap()
            .as_deref(),
        Some(prepared.reclaim_token())
    );
    assert!(!prepared.reclaim_token().contains("47800"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(store.auth_path(47800, prepared.launch_id()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn failed_generation_does_not_destroy_previous_current() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let first = store.prepare(47801).unwrap();
    first.commit(&manifest(&first)).unwrap();

    let second = store.prepare(47801).unwrap();
    second.abort().unwrap();

    let current = store.read_current(47801).unwrap().unwrap();
    assert_eq!(current.launch_id, first.launch_id());
    assert!(store.read_auth(47801, first.launch_id()).unwrap().is_some());
    assert!(!store.generation_dir(47801, second.launch_id()).exists());
}

#[test]
fn commit_atomically_switches_current_and_prunes_old_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let first = store.prepare(47802).unwrap();
    first.commit(&manifest(&first)).unwrap();
    let first_dir = store.generation_dir(47802, first.launch_id());

    let second = store.prepare(47802).unwrap();
    second.commit(&manifest(&second)).unwrap();

    assert_eq!(
        store.read_current(47802).unwrap().unwrap().launch_id,
        second.launch_id()
    );
    assert!(!first_dir.exists());
    assert!(store.generation_dir(47802, second.launch_id()).is_dir());
    let temp_entries = fs::read_dir(store.session_dir(47802))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(temp_entries, 0);
}

#[test]
fn commit_rejects_manifest_from_another_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let first = store.prepare(47803).unwrap();
    let second = store.prepare(47803).unwrap();

    let err = second.commit(&manifest(&first)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(store.read_current(47803).unwrap().is_none());
}

#[test]
fn oversized_capsule_file_is_rejected_before_parsing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let path = store.current_path(47804);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![b'x'; MAX_CAPSULE_FILE_BYTES as usize + 1]).unwrap();

    let err = store.read_current(47804).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::FileTooLarge);
}

#[test]
fn current_manifest_never_serializes_reclaim_token() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let prepared = store.prepare(47805).unwrap();
    prepared.commit(&manifest(&prepared)).unwrap();

    let current = fs::read_to_string(store.current_path(47805)).unwrap();
    assert!(!current.contains(prepared.reclaim_token()));
    assert!(current.contains(prepared.launch_id()));
}

#[test]
fn process_state_requires_matching_start_identity() {
    let captured = capture_process(std::process::id());
    if captured.start_identity.is_none() {
        assert_eq!(process_state(&captured), ProcessState::Unknown);
        return;
    }
    assert_eq!(process_state(&captured), ProcessState::Alive);

    let reused = ProcessIdentity {
        pid: captured.pid,
        start_identity: Some("different-start".into()),
    };
    assert_eq!(process_state(&reused), ProcessState::Exited);
}

#[test]
fn next_action_distinguishes_a_dead_emulator_from_its_live_bridge() {
    assert_eq!(
        next_safe_action(
            ProcessState::Exited,
            Some(ProcessState::Alive),
            LeaseState::Held
        ),
        "cleanup_owned_bridge_then_launch"
    );
    assert_eq!(
        next_safe_action(
            ProcessState::Alive,
            Some(ProcessState::Exited),
            LeaseState::Held
        ),
        "recover_bridge_or_replace"
    );
    assert_eq!(
        next_safe_action(
            ProcessState::Exited,
            Some(ProcessState::Unknown),
            LeaseState::Held
        ),
        "inspect_bridge_identity_before_launch"
    );
    assert_eq!(
        next_safe_action(
            ProcessState::Exited,
            Some(ProcessState::Exited),
            LeaseState::Held
        ),
        "launch_allowed"
    );
    assert_eq!(
        next_safe_action(
            ProcessState::Exited,
            Some(ProcessState::Alive),
            LeaseState::Occupied
        ),
        "coordinate_with_current_controller"
    );
    assert_eq!(
        next_safe_action(
            ProcessState::Exited,
            Some(ProcessState::Alive),
            LeaseState::Unknown
        ),
        "inspect_lease_before_generation_transition"
    );
}

#[cfg(unix)]
#[test]
fn owned_cleanup_terminates_a_live_bridge_after_the_emulator_exits() {
    let mut emulator = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let emulator_identity = capture_process(emulator.id());
    emulator.kill().unwrap();
    emulator.wait().unwrap();

    let mut bridge = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let bridge_identity = capture_process(bridge.id());
    let current = CurrentManifest {
        schema_version: SCHEMA_VERSION,
        launch_id: "launch-cleanup-test".into(),
        port: 47810,
        adapter: "mame-pc98".into(),
        system: "pc98".into(),
        content: "/games/test.hdi".into(),
        build: None,
        emulator: emulator_identity,
        bridge: Some(bridge_identity),
        backend_endpoint: Some("127.0.0.1:48810".into()),
        created_at_unix_ms: 1,
    };

    assert_eq!(current.process_state(), ProcessState::Exited);
    assert_eq!(current.bridge_process_state(), Some(ProcessState::Alive));
    current.terminate_owned_processes().unwrap();
    bridge.wait().unwrap();
    assert_eq!(current.bridge_process_state(), Some(ProcessState::Exited));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_process_identity_uses_kernel_microseconds_and_accepts_legacy_capsules() {
    let captured = capture_process(std::process::id());
    let start = captured.start_identity.as_deref().unwrap();
    let fields: Vec<_> = start.split(':').collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], "macos-bsdinfo");
    assert!(fields[1].parse::<u64>().unwrap() > 0);
    assert!(fields[2].parse::<u64>().unwrap() < 1_000_000);
    assert_eq!(process_state(&captured), ProcessState::Alive);

    let legacy = ProcessIdentity {
        pid: captured.pid,
        start_identity: legacy_macos_process_start_identity(captured.pid),
    };
    assert!(legacy.start_identity.is_some());
    assert_eq!(process_state(&legacy), ProcessState::Alive);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_rapid_child_restart_captures_distinct_start_identity() {
    let mut first = std::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .unwrap();
    let first_identity = capture_process(first.id());
    first.kill().unwrap();
    first.wait().unwrap();
    assert!(first_identity.start_identity.is_some());
    assert_eq!(process_state(&first_identity), ProcessState::Exited);

    let mut second = std::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .unwrap();
    let second_identity = capture_process(second.id());
    second.kill().unwrap();
    second.wait().unwrap();
    assert!(second_identity.start_identity.is_some());
    assert_ne!(
        first_identity.start_identity,
        second_identity.start_identity
    );
}

#[test]
fn current_rejects_path_traversal_launch_id() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let path = store.current_path(47806);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "launch_id": "launch-../../escape",
            "port": 47806,
            "adapter": "mesen2",
            "system": "snes",
            "content": "/game.sfc",
            "emulator": {"pid": std::process::id()},
            "created_at_unix_ms": 1
        }))
        .unwrap(),
    )
    .unwrap();

    let error = store.read_current(47806).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[cfg(unix)]
#[test]
fn auth_reader_refuses_symlink() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let prepared = store.prepare(47807).unwrap();
    let auth = store.auth_path(47807, prepared.launch_id());
    let outside = tmp.path().join("outside-secret");
    fs::write(&outside, "secret").unwrap();
    fs::remove_file(&auth).unwrap();
    symlink(&outside, &auth).unwrap();

    let error = store.read_auth(47807, prepared.launch_id()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn compatibility_files_share_the_private_runtime_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));

    store
        .write_compatibility_token(47808, "compatibility-token")
        .unwrap();
    store
        .write_persisted_port("0123456789abcdef-1234", 47800, 47808)
        .unwrap();

    assert_eq!(
        store.read_compatibility_token(47808).unwrap().as_deref(),
        Some("compatibility-token")
    );
    assert_eq!(
        store
            .read_persisted_port("0123456789abcdef-1234", 47800)
            .unwrap(),
        Some(47808)
    );
    assert!(store
        .compatibility_token_path(47808)
        .starts_with(store.root()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [
            store.compatibility_token_path(47808),
            store
                .persisted_port_path("0123456789abcdef-1234", 47800)
                .unwrap(),
        ] {
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}

#[test]
fn compatibility_reader_rejects_invalid_port_file() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let identity = "0123456789abcdef-1234";
    store.write_persisted_port(identity, 47800, 47808).unwrap();
    let path = store.persisted_port_path(identity, 47800).unwrap();
    fs::write(&path, "not-a-port").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let error = store.read_persisted_port(identity, 47800).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains(&path.display().to_string()));
}

#[cfg(unix)]
#[test]
fn compatibility_writer_refuses_symlink_and_non_private_file() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let token = store.compatibility_token_path(47809);
    fs::create_dir_all(token.parent().unwrap()).unwrap();
    let outside = tmp.path().join("outside-token");
    fs::write(&outside, "outside").unwrap();
    symlink(&outside, &token).unwrap();

    let error = store
        .write_compatibility_token(47809, "replacement")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");

    fs::remove_file(&token).unwrap();
    fs::write(&token, "too-public").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    let error = store.read_compatibility_token(47809).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}
