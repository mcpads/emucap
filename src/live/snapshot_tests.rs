use super::{
    link::{Capabilities, EmulatorLink, LinkError},
    runtime::{ManifestSpec, RuntimeStore},
    snapshot,
    tools::ToolOutput,
};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

struct Host {
    caps: Capabilities,
    captures: usize,
    unsafe_halt: bool,
    timeout: bool,
    cancel_at_observation: Option<(usize, super::link::RequestCancellation)>,
    observations: usize,
    wrong_artifact: bool,
}
fn halt() -> Value {
    json!({"cpu":"main", "kind":"main_cpu_instruction", "boundary":"instruction_boundary", "pc":33000,"program_bank":128,
    "frame":{"value":"42","domain":"snes_ppu_frame"},"cycle":{"value":"9007199254740993","domain":"snes_master_clock"}})
}
impl EmulatorLink for Host {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn endpoint_port(&self) -> Option<u16> {
        Some(47800)
    }
    fn continuity(&self) -> super::continuity::ContinuitySnapshot {
        let mut c = super::continuity::ContinuitySnapshot::default();
        c.runtime_binding.state = super::continuity::RuntimeBindingState::Bound;
        c
    }
    fn call(&mut self, method: &str, _: Value) -> Result<Value, LinkError> {
        match method {
            "status" => Ok(json!({"state":"frozen"})),
            "get_rom_info" => Ok(
                json!({"sha1": if self.wrong_artifact { String::new() } else { hex::encode_upper(Sha1::digest(b"game")) }}),
            ),
            "observe_snapshot_halt" => {
                self.observations += 1;
                if let Some((n, token)) = &self.cancel_at_observation {
                    if self.observations == *n {
                        token.cancel();
                        return Err(LinkError::Cancelled);
                    }
                }
                Ok(json!({"halt":halt()}))
            }
            "capture_snapshot" => {
                if self.unsafe_halt {
                    return Err(LinkError::Emulator {
                        kind: "unsafe_halt".into(),
                        message: "unsafe halt".into(),
                    });
                }
                self.captures += 1;
                if self.timeout {
                    return Err(LinkError::Timeout);
                }
                Ok(
                    json!({"state":"frozen","boundary":"instruction_boundary","format":"mesen-savestate",
                    "hex":hex::encode(b"native-state"),"halt":halt(), "loaded_artifact_sha1":hex::encode(Sha1::digest(b"game"))}),
                )
            }
            _ => panic!("unexpected operation {method}"),
        }
    }
}
fn fixture() -> (tempfile::TempDir, RuntimeStore, Host) {
    let d = tempfile::tempdir().unwrap();
    let content = d.path().join("game.sfc");
    std::fs::write(&content, b"game").unwrap();
    let store = RuntimeStore::new(d.path().join("home"));
    let p = store.prepare(47800).unwrap();
    let launch = p.launch_id().to_string();
    p.commit(&p.manifest(ManifestSpec {
        adapter: "mesen2".into(),
        system: "snes".into(),
        content: content.to_str().unwrap().into(),
        emulator_pid: std::process::id(),
        bridge_pid: None,
        backend_endpoint: None,
        build: Some("build".into()),
    }))
    .unwrap();
    let mut caps = Capabilities::empty();
    caps.identity.system = Some("snes".into());
    caps.identity.launch_id = Some(launch);
    caps.identity.content = Some(content.to_str().unwrap().into());
    caps.identity.adapter = Some("mesen2".into());
    caps.identity.build = Some("build".into());
    caps.identity.host_features = vec!["instruction_snapshot_capture".into()];
    caps.identity.host_build = Some(
        json!({"upstream":"mesen","commit":"aa".repeat(20),"patchset_sha256":"bb".repeat(32),"binary_sha256":"cc".repeat(32)}),
    );
    (
        d,
        store,
        Host {
            caps,
            captures: 0,
            unsafe_halt: false,
            timeout: false,
            cancel_at_observation: None,
            observations: 0,
            wrong_artifact: false,
        },
    )
}
fn value(output: ToolOutput) -> Value {
    match output {
        ToolOutput::Json(v) => v,
        _ => panic!(),
    }
}
#[test]
fn instruction_receipt_needs_no_recording_and_survives_loss_of_live_host() {
    let (d, store, mut host) = fixture();
    assert!(host.caps.recording.is_none());
    let dest = d.path().join("state.mss");
    let path = dest.to_str().unwrap();
    let saved = value(snapshot::save_in_store(&mut host, path, "anchor", &store).unwrap());
    assert_eq!(saved["status"], "completed");
    assert_eq!(host.captures, 1);
    assert_eq!(
        saved["snapshot_receipt"]["body"]["halt"]["cycle"]["value"],
        "9007199254740993"
    );
    let replay = value(snapshot::save_in_store(&mut host, path, "anchor", &store).unwrap());
    assert_eq!(replay["status"], "verified");
    assert_eq!(host.captures, 1);
    assert!(snapshot::save_in_store(
        &mut host,
        d.path().join("other").to_str().unwrap(),
        "anchor",
        &store
    )
    .is_err());
    drop(host);
    assert_eq!(
        value(snapshot::verify_in_store(&store, "anchor", Some(path), None).unwrap())["status"],
        "verified"
    );
    assert!(snapshot::verify_in_store(&store, "anchor", None, Some("another-launch")).is_err());
    std::fs::write(&dest, b"substitution").unwrap();
    assert!(snapshot::verify_in_store(&store, "anchor", Some(path), None).is_err());
}
#[test]
fn unsafe_halt_and_uncertain_capture_do_not_replace_or_retry() {
    let (d, store, mut host) = fixture();
    let dest = d.path().join("state.mss");
    std::fs::write(&dest, b"old").unwrap();
    let path = dest.to_str().unwrap();
    host.unsafe_halt = true;
    let result = value(snapshot::save_in_store(&mut host, path, "unsafe", &store).unwrap());
    assert_eq!(result["status"], "failed");
    assert_eq!(host.captures, 0);
    assert_eq!(std::fs::read(&dest).unwrap(), b"old");
    host.unsafe_halt = false;
    host.timeout = true;
    snapshot::save_in_store(&mut host, path, "lost", &store).unwrap();
    host.timeout = false;
    snapshot::save_in_store(&mut host, path, "lost", &store).unwrap();
    assert_eq!(host.captures, 1);
    assert_eq!(std::fs::read(dest).unwrap(), b"old");
}
#[test]
fn issued_pair_survives_export_failure_and_detects_corruption() {
    let (d, store, mut host) = fixture();
    let dest = d.path().join("destination-directory");
    std::fs::create_dir(&dest).unwrap();
    let result = value(
        snapshot::save_in_store(&mut host, dest.to_str().unwrap(), "export-failed", &store)
            .unwrap(),
    );
    assert_eq!(result["status"], "failed");
    assert_eq!(result["receipt_issued"], true);
    let verified = value(snapshot::verify_in_store(&store, "export-failed", None, None).unwrap());
    assert_eq!(verified["status"], "verified");
    std::fs::write(verified["snapshot_path"].as_str().unwrap(), b"broken").unwrap();
    assert!(snapshot::verify_in_store(&store, "export-failed", None, None).is_err());
    let good = d.path().join("good");
    let saved = value(
        snapshot::save_in_store(&mut host, good.to_str().unwrap(), "tamper", &store).unwrap(),
    );
    let receipt_path = saved["receipt_path"].as_str().unwrap();
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
    receipt["body"]["halt"]["pc"] = json!(0);
    std::fs::write(receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    assert!(snapshot::verify_in_store(&store, "tamper", None, None).is_err());
}

#[test]
fn cancellation_preserves_only_already_published_evidence() {
    for (when, issued) in [(1, false), (2, true)] {
        let (d, store, mut host) = fixture();
        let token = super::link::RequestCancellation::default();
        host.cancel_at_observation = Some((when, token.clone()));
        let dest = d.path().join("copy");
        let outcome = value(
            snapshot::save_with_cancellation(
                &mut host,
                dest.to_str().unwrap(),
                "cancel",
                &store,
                token,
            )
            .unwrap(),
        );
        assert_eq!(outcome["status"], "failed");
        assert_eq!(outcome["receipt_issued"], issued);
        assert_eq!(host.captures, 1);
        let observed = value(snapshot::verify_in_store(&store, "cancel", None, None).unwrap());
        assert_eq!(observed["receipt_issued"], issued);
        assert_eq!(
            observed["status"],
            if issued { "verified" } else { "failed" }
        );
    }
}
#[test]
fn wrong_loaded_artifact_is_refused_before_serialization() {
    let (d, store, mut host) = fixture();
    host.wrong_artifact = true;
    let outcome = value(
        snapshot::save_in_store(
            &mut host,
            d.path().join("copy").to_str().unwrap(),
            "wrong",
            &store,
        )
        .unwrap(),
    );
    assert_eq!(outcome["receipt_issued"], false);
    assert_eq!(host.captures, 0);
}
#[test]
fn publication_recovers_without_terminal_update_and_active_keys_are_locked() {
    let (d, store, mut host) = fixture();
    let dest = d.path().join("copy");
    snapshot::save_in_store(&mut host, dest.to_str().unwrap(), "recover", &store).unwrap();
    std::fs::remove_file(store.root().join("state-snapshots/recover/terminal.json")).unwrap();
    let recovered = value(snapshot::verify_in_store(&store, "recover", None, None).unwrap());
    assert_eq!(recovered["status"], "verified");
    assert!(recovered["save_outcome"].is_null());
    std::fs::write(
        store.root().join("state-snapshots/recover/terminal.json"),
        b"interrupted",
    )
    .unwrap();
    let recovered = value(snapshot::verify_in_store(&store, "recover", None, None).unwrap());
    assert_eq!(recovered["status"], "verified");
    assert!(recovered["save_outcome_error"].is_string());
    let (locked, _) = super::snapshot_store::Store::open(&store, "recover", false).unwrap();
    assert!(snapshot::verify_in_store(&store, "recover", None, None).is_err());
    drop(locked);
    assert!(snapshot::verify_in_store(&store, "recover", None, None).is_ok());
    // A reserved but unrecorded key is permanently indeterminate, never a new capture.
    let (unfinished, fresh) =
        super::snapshot_store::Store::open(&store, "unfinished", true).unwrap();
    assert!(fresh);
    drop(unfinished);
    assert_eq!(
        value(snapshot::verify_in_store(&store, "unfinished", None, None).unwrap())["status"],
        "indeterminate"
    );
    assert!(
        snapshot::save_in_store(&mut host, dest.to_str().unwrap(), "unfinished", &store).is_err()
    );
    assert_eq!(host.captures, 1);
}

#[test]
fn export_cannot_replace_retained_evidence_even_through_a_directory_alias() {
    let (d, store, mut host) = fixture();
    let result = value(
        snapshot::save_in_store(
            &mut host,
            d.path().join("copy").to_str().unwrap(),
            "kept",
            &store,
        )
        .unwrap(),
    );
    let retained = result["snapshot_path"].as_str().unwrap();
    assert!(snapshot::save_in_store(&mut host, retained, "overwrite", &store).is_err());
    #[cfg(unix)]
    {
        let alias = d.path().join("alias");
        std::os::unix::fs::symlink(std::path::Path::new(retained).parent().unwrap(), &alias)
            .unwrap();
        assert!(snapshot::save_in_store(
            &mut host,
            alias.join("state.bin").to_str().unwrap(),
            "alias-overwrite",
            &store
        )
        .is_err());
    }
    assert_eq!(host.captures, 1);
    assert_eq!(
        value(snapshot::verify_in_store(&store, "kept", None, None).unwrap())["status"],
        "verified"
    );
}
