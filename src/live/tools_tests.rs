use super::link::{Capabilities, EmulatorIdentity, EmulatorLink, FakeLink, LinkError};
use super::tools::*;
use serde_json::{json, Value};

/// 모든 호출을 기록하고 status/read_memory를 스크립트로 돌려주는 가짜 링크(다중 호출 검증용).
struct Rec {
    calls: Vec<(String, Value)>,
    state: String,      // status가 돌려줄 state
    reads: Vec<String>, // read_memory가 차례로 돌려줄 hex(끝나면 마지막 값 반복)
    read_i: usize,
    fail_calls: Vec<usize>,
    caps: Capabilities,
}
impl Rec {
    fn new(state: &str, reads: &[&str]) -> Self {
        Rec {
            calls: vec![],
            state: state.into(),
            reads: reads.iter().map(|s| s.to_string()).collect(),
            read_i: 0,
            fail_calls: vec![],
            caps: Capabilities {
                protocol_version: 1,
                methods: vec![],
                memory_types: vec![],
                contracts: crate::contracts::ContractAdvertisement::Unreported,
                identity: EmulatorIdentity::default(),
            },
        }
    }
    fn with_fail_calls(mut self, calls: &[usize]) -> Self {
        self.fail_calls = calls.to_vec();
        self
    }
    fn with_methods(mut self, methods: &[&str]) -> Self {
        self.caps.methods = methods.iter().map(|method| method.to_string()).collect();
        self
    }
    fn methods(&self) -> Vec<&str> {
        self.calls.iter().map(|(m, _)| m.as_str()).collect()
    }
}
impl EmulatorLink for Rec {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.calls.push((method.to_string(), params));
        if self.fail_calls.contains(&self.calls.len()) {
            return Err(LinkError::Timeout);
        }
        // Mesen처럼: resume은 frozen에서만 허용(running에서 부르면 not_paused 에러).
        if method == "resume" && self.state != "frozen" {
            return Err(LinkError::Emulator {
                kind: "not_paused".into(),
                message: "resume은 frozen에서만".into(),
            });
        }
        Ok(match method {
            "status" => json!({ "state": self.state, "frame": 0 }),
            "read_memory" => {
                let h = self
                    .reads
                    .get(self.read_i)
                    .or_else(|| self.reads.last())
                    .cloned()
                    .unwrap_or_default();
                self.read_i += 1;
                json!({ "hex": h })
            }
            _ => json!({}),
        })
    }
}

#[test]
fn step_routes_public_units_to_compatible_wire_methods() {
    let mut link = Rec::new("frozen", &[]).with_methods(&["step", "step_instructions"]);

    step(&mut link, 3, StepUnit::Frames, None).unwrap();
    step(&mut link, 2, StepUnit::Instructions, Some("arm7")).unwrap();

    assert_eq!(link.calls[0], ("step".into(), json!({"frames": 3})));
    assert_eq!(
        link.calls[1],
        (
            "step_instructions".into(),
            json!({"count": 2, "cpu": "arm7"})
        )
    );
}

#[test]
fn step_rejects_units_missing_from_adapter_capabilities() {
    let mut link = Rec::new("frozen", &[]).with_methods(&["step"]);
    let error = step(&mut link, 1, StepUnit::Instructions, None).unwrap_err();

    assert!(matches!(
        error,
        LinkError::Emulator { ref kind, .. } if kind == "unsupported"
    ));
    assert!(link.calls.is_empty());
}

#[test]
fn synchronous_advance_caps_reject_before_transport() {
    let over = crate::live::temporal::MAX_SYNC_ADVANCE_COUNT + 1;

    let mut run_link = FakeLink::ok(json!({}));
    assert!(run_frames(&mut run_link, over).is_err());
    assert_eq!(run_link.last_method, None);

    let mut press_link = FakeLink::ok(json!({}));
    assert!(press_buttons(&mut press_link, 0, &["a".into()], over).is_err());
    assert_eq!(press_link.last_method, None);

    let mut step_link = Rec::new("frozen", &[]).with_methods(&["step"]);
    assert!(step(&mut step_link, over, StepUnit::Frames, None).is_err());
    assert!(step_link.calls.is_empty());
}

struct ProjectionLink {
    caps: Capabilities,
    delay: std::time::Duration,
    buttons: Vec<String>,
    projection: Vec<Vec<String>>,
}

impl ProjectionLink {
    fn new(delay: std::time::Duration) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["pause".into(), "set_input".into(), "step".into()],
                memory_types: vec![],
                contracts: crate::contracts::ContractAdvertisement::Unreported,
                identity: EmulatorIdentity::default(),
            },
            delay,
            buttons: vec![],
            projection: vec![],
        }
    }
}

impl EmulatorLink for ProjectionLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        std::thread::sleep(self.delay);
        match method {
            "pause" => {}
            "set_input" => {
                self.buttons = params["buttons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect();
            }
            "step" => {
                let frames = params["frames"].as_u64().unwrap();
                for _ in 0..frames {
                    self.projection.push(self.buttons.clone());
                }
            }
            other => return Err(LinkError::Protocol(format!("unexpected call: {other}"))),
        }
        Ok(json!({}))
    }
}

#[test]
fn frozen_tap_projection_is_independent_of_host_delay() {
    let down = vec!["down".to_string()];
    let a = vec!["a".to_string()];
    let mut no_delay = ProjectionLink::new(std::time::Duration::ZERO);
    let mut delayed = ProjectionLink::new(std::time::Duration::from_millis(2));

    tap(&mut no_delay, 0, &down, 2, 0).unwrap();
    tap(&mut no_delay, 0, &a, 2, 0).unwrap();
    tap(&mut delayed, 0, &down, 2, 0).unwrap();
    tap(&mut delayed, 0, &a, 2, 0).unwrap();

    assert_eq!(delayed.projection, no_delay.projection);
    assert!(delayed.buttons.is_empty());
    assert_eq!(
        no_delay.projection,
        vec![
            vec!["down".to_string()],
            vec!["down".to_string()],
            vec![],
            vec!["a".to_string()],
            vec!["a".to_string()],
            vec![]
        ]
    );
}

struct FaultProjectionLink {
    caps: Capabilities,
    fail_after_apply: Option<usize>,
    disconnect_after_apply: Option<usize>,
    calls: usize,
    connected: bool,
    frozen: bool,
    buttons: Vec<String>,
    projection: Vec<Vec<String>>,
}

impl FaultProjectionLink {
    fn timeout_on(call: usize) -> Self {
        Self::new(Some(call), None)
    }

    fn disconnect_on(call: usize) -> Self {
        Self::new(None, Some(call))
    }

    fn new(fail_after_apply: Option<usize>, disconnect_after_apply: Option<usize>) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["pause".into(), "set_input".into(), "step".into()],
                memory_types: vec![],
                contracts: crate::contracts::ContractAdvertisement::Unreported,
                identity: EmulatorIdentity::default(),
            },
            fail_after_apply,
            disconnect_after_apply,
            calls: 0,
            connected: true,
            frozen: false,
            buttons: vec![],
            projection: vec![],
        }
    }
}

impl EmulatorLink for FaultProjectionLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        if !self.connected {
            return Err(LinkError::NotConnected);
        }
        self.calls += 1;
        match method {
            "pause" => self.frozen = true,
            "set_input" => {
                self.buttons = params["buttons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect();
            }
            "step" => {
                assert!(self.frozen, "step projection requires a frozen clock");
                for _ in 0..params["frames"].as_u64().unwrap() {
                    self.projection.push(self.buttons.clone());
                }
            }
            other => return Err(LinkError::Protocol(format!("unexpected call: {other}"))),
        }
        if self.disconnect_after_apply == Some(self.calls) {
            self.connected = false;
            return Err(LinkError::NotConnected);
        }
        if self.fail_after_apply == Some(self.calls) {
            return Err(LinkError::Timeout);
        }
        Ok(json!({}))
    }
}

#[test]
fn transient_tap_recovers_after_post_effect_timeout_at_each_boundary() {
    // pause, acquire, pressed step, release, release-edge step. Pause itself owns no transient
    // resource, so inject an ambiguous post-effect timeout at every later boundary.
    for fail_call in 2..=5 {
        let mut link = FaultProjectionLink::timeout_on(fail_call);
        let result = tap(&mut link, 0, &["a".into()], 2, 0);
        assert!(
            result.is_err(),
            "failure at call {fail_call} must be visible"
        );
        assert!(
            link.buttons.is_empty(),
            "failure at call {fail_call} left transient input engaged"
        );
        assert!(
            link.frozen,
            "failure at call {fail_call} did not restore the frozen terminal state"
        );
    }
}

#[test]
fn disconnect_without_adapter_cleanup_is_never_reported_as_completion() {
    let mut link = FaultProjectionLink::disconnect_on(2);
    let error = tap(&mut link, 0, &["a".into()], 2, 0).unwrap_err();

    assert!(
        matches!(error, LinkError::Emulator { ref kind, .. } if kind == "cleanup_failed"),
        "an unreachable release must stay fail-loud: {error:?}"
    );
    assert_eq!(link.buttons, vec!["a"]);
}

#[test]
fn run_frames_no_separate_resume_when_frozen() {
    // frozen이어도 어댑터 run_frames 핸들러가 원자적으로 resume하므로, Rust는 별도 resume을 보내지 않는다
    // (별도 resume은 명령 도착 전 free-run으로 watch/BP를 조기 소진시키는 레이스다).
    let mut l = Rec::new("frozen", &[]);
    run_frames(&mut l, 5).unwrap();
    let m = l.methods();
    assert!(
        !m.contains(&"resume"),
        "별도 resume 금지(어댑터가 원자 처리): {m:?}"
    );
    assert_eq!(*m.last().unwrap(), "run_frames");
}

#[test]
fn press_buttons_no_separate_resume_when_frozen() {
    // press_buttons도 어댑터가 원자 resume — Rust는 별도 resume 없이 명령만 보낸다.
    let mut l = Rec::new("frozen", &[]);
    press_buttons(&mut l, 0, &["start".into()], 10).unwrap();
    let m = l.methods();
    assert!(
        !m.contains(&"resume"),
        "별도 resume 금지(어댑터가 원자 처리): {m:?}"
    );
    assert_eq!(*m.last().unwrap(), "press_buttons");
}

#[test]
fn run_frames_no_resume_when_running() {
    // running이면 resume를 부르면 안 된다(Mesen resume은 running에서 에러) → Rec가 에러로 잡는다.
    let mut l = Rec::new("running", &[]);
    run_frames(&mut l, 5).unwrap();
    assert!(
        !l.methods().contains(&"resume"),
        "running이면 resume 금지: {:?}",
        l.methods()
    );
}

#[test]
fn press_buttons_no_resume_when_running() {
    let mut l = Rec::new("running", &[]);
    press_buttons(&mut l, 0, &["start".into()], 10).unwrap();
    assert!(
        !l.methods().contains(&"resume"),
        "running이면 resume 금지: {:?}",
        l.methods()
    );
}

#[test]
fn tap_after_frames_advances_at_end() {
    let mut l = Rec::new("frozen", &[]);
    tap(&mut l, 0, &["a".into()], 2, 10).unwrap();
    let last = l.calls.last().unwrap();
    assert_eq!(last.0, "step");
    assert_eq!(last.1["frames"], 10); // 떼고 after_frames만큼 진행
}

#[test]
fn tap_step_failure_releases_input_before_returning_error() {
    let mut l = Rec::new("frozen", &[]).with_fail_calls(&[3]);
    let error = tap(&mut l, 0, &["a".into()], 2, 0).unwrap_err();
    assert!(matches!(error, LinkError::Timeout));
    assert_eq!(
        l.methods(),
        vec!["pause", "set_input", "step", "set_input", "pause"]
    );
    assert_eq!(l.calls[3].1["buttons"], json!([]));
}

#[test]
fn tap_lost_acquire_response_still_releases_and_refreezes() {
    let mut l = Rec::new("frozen", &[]).with_fail_calls(&[2]);
    let error = tap(&mut l, 0, &["a".into()], 2, 0).unwrap_err();
    assert!(matches!(error, LinkError::Timeout));
    assert_eq!(
        l.methods(),
        vec!["pause", "set_input", "set_input", "pause"]
    );
    assert_eq!(l.calls[2].1["buttons"], json!([]));
}

#[test]
fn tap_reports_cleanup_failure_without_claiming_completion() {
    let mut l = Rec::new("frozen", &[]).with_fail_calls(&[3, 4]);
    let error = tap(&mut l, 0, &["a".into()], 2, 0).unwrap_err();
    assert!(
        matches!(error, LinkError::Emulator { ref kind, .. } if kind == "cleanup_failed"),
        "dual failure must surface cleanup_failed: {error:?}"
    );
}

#[test]
fn hold_until_stops_on_change() {
    // before=aa, 한 step 후 aa(불변), 또 step 후 bb(변함) → frames=2
    let mut l = Rec::new("frozen", &["aa", "aa", "bb"]);
    let out = hold_until(&mut l, 0, &["down".to_string()], "workraml", 0x1000, 1, 100).unwrap();
    match out {
        ToolOutput::Json(v) => {
            assert_eq!(v["changed"], true);
            assert_eq!(v["frames"], 2);
            assert_eq!(v["before"], "aa");
            assert_eq!(v["after"], "bb");
        }
        _ => panic!("Json 기대"),
    }
}

#[test]
fn hold_until_read_failure_releases_input() {
    let mut l = Rec::new("frozen", &["aa"]).with_fail_calls(&[3]);
    let error = hold_until(&mut l, 0, &["down".to_string()], "workraml", 0x1000, 1, 3).unwrap_err();
    assert!(matches!(error, LinkError::Timeout));
    assert_eq!(
        l.methods(),
        vec!["pause", "set_input", "read_memory", "set_input", "pause"]
    );
    assert_eq!(l.calls[3].1["buttons"], json!([]));
}

#[test]
fn hold_until_lost_acquire_response_still_releases_and_refreezes() {
    let mut l = Rec::new("frozen", &["aa"]).with_fail_calls(&[2]);
    let error = hold_until(&mut l, 0, &["down".to_string()], "workraml", 0x1000, 1, 3).unwrap_err();
    assert!(matches!(error, LinkError::Timeout));
    assert_eq!(
        l.methods(),
        vec!["pause", "set_input", "set_input", "pause"]
    );
    assert_eq!(l.calls[2].1["buttons"], json!([]));
}

#[test]
fn hold_until_cleanup_failure_is_not_completed() {
    // pause, set_input, before-read, step, after-read, release
    let mut l = Rec::new("frozen", &["aa", "bb"]).with_fail_calls(&[6]);
    let error = hold_until(&mut l, 0, &["down".to_string()], "workraml", 0x1000, 1, 3).unwrap_err();
    assert!(
        matches!(error, LinkError::Emulator { ref kind, .. } if kind == "cleanup_failed"),
        "release failure must replace a false successful completion: {error:?}"
    );
}

#[test]
fn hold_until_max_frames_when_no_change() {
    let mut l = Rec::new("frozen", &["aa"]); // 항상 aa
    let out = hold_until(&mut l, 0, &["down".to_string()], "workraml", 0x1000, 1, 3).unwrap();
    match out {
        ToolOutput::Json(v) => {
            assert_eq!(v["changed"], false);
            assert_eq!(v["frames"], 3);
        }
        _ => panic!("Json 기대"),
    }
}

#[test]
fn read_memory_forwards_params_and_returns_json() {
    let mut link = FakeLink::ok(json!({ "hex": "00ff" }));
    let out = read_memory(&mut link, "snesWorkRam", 0, 2).unwrap();
    match out {
        ToolOutput::Json(v) => assert_eq!(v["hex"], "00ff"),
        _ => panic!("Json 기대"),
    }
    assert_eq!(link.last_method.as_deref(), Some("read_memory"));
    let p = link.last_params.unwrap();
    assert_eq!(p["memory_type"], "snesWorkRam");
    assert_eq!(p["length"], 2);
}

#[test]
fn probe_forwards_params_and_returns_json() {
    let mut link = FakeLink::ok(json!({ "hex": "11223344", "frame": 3 }));
    let out = probe(&mut link, "/tmp/base.mst", 3, "vram", 0x4000, 8).unwrap();
    match out {
        ToolOutput::Json(v) => {
            assert_eq!(v["hex"], "11223344");
            assert_eq!(v["frame"], 3);
        }
        _ => panic!("Json 기대"),
    }
    assert_eq!(link.last_method.as_deref(), Some("probe"));
    let p = link.last_params.unwrap();
    assert_eq!(p["state"], "/tmp/base.mst");
    assert_eq!(p["frame"], 3);
    assert_eq!(p["memory_type"], "vram");
    assert_eq!(p["address"], 0x4000);
    assert_eq!(p["length"], 8);
}

#[test]
fn write_memory_forwards() {
    let mut link = FakeLink::ok(json!({"written":4}));
    let out = write_memory(&mut link, "snesWorkRam", 16, "deadbeef").unwrap();
    assert!(matches!(out, ToolOutput::Json(_)));
    let p = link.last_params.unwrap();
    assert_eq!(p["memory_type"], "snesWorkRam");
    assert_eq!(p["address"], 16);
    assert_eq!(p["hex"], "deadbeef");
}

#[test]
fn write_memory_rejects_invalid_or_oversized_payload_before_transport() {
    for hex in ["", "0", "zz"] {
        let mut link = FakeLink::ok(json!({"written":0}));
        assert!(write_memory(&mut link, "ram", 0, hex).is_err());
        assert_eq!(link.last_method, None);
    }

    let mut link = FakeLink::ok(json!({"written":0}));
    let oversized = vec![0u8; MAX_WRITE_BYTES + 1];
    assert!(write_memory_bytes(&mut link, "ram", 0, &oversized).is_err());
    assert_eq!(link.last_method, None);

    let mut link = FakeLink::ok(json!({"written":0}));
    assert!(write_memory_bytes(&mut link, "ram", u64::MAX, &[1]).is_err());
    assert_eq!(link.last_method, None);
}

#[test]
fn press_buttons_forwards_list_and_frames() {
    let mut link = FakeLink::ok(json!({"status":"completed"}));
    press_buttons(&mut link, 0, &["a".into(), "start".into()], 10).unwrap();
    let p = link.last_params.unwrap();
    assert_eq!(p["buttons"], json!(["a", "start"]));
    assert_eq!(p["frames"], 10);
    assert_eq!(link.last_method.as_deref(), Some("press_buttons"));
}

#[test]
fn run_frames_passes_interrupted_through() {
    let mut link = FakeLink::ok(json!({"status":"interrupted","reason":"breakpoint"}));
    let out = run_frames(&mut link, 100).unwrap();
    match out {
        ToolOutput::Json(v) => assert_eq!(v["status"], "interrupted"),
        _ => panic!("Json 기대"),
    }
}

#[test]
fn probe_passes_interrupted_through() {
    // 진행 중 pause_on_hit BP가 끼면 어댑터가 hex 대신 interrupted를 돌려준다 — 링크가 그대로 통과시켜
    // 에이전트가 받아야 한다(working만 건너뛰고 interrupted는 정상 result).
    let mut link = FakeLink::ok(json!({"status":"interrupted","reason":"breakpoint","pc":196624}));
    let out = probe(&mut link, "/tmp/s.mss", 60, "cpu", 0x1000, 4).unwrap();
    match out {
        ToolOutput::Json(v) => {
            assert_eq!(v["status"], "interrupted");
            assert_eq!(v["reason"], "breakpoint");
        }
        _ => panic!("Json 기대"),
    }
}

#[test]
fn set_breakpoint_forwards_all_fields() {
    let mut link = FakeLink::ok(json!({"id":3}));
    set_breakpoint(
        &mut link,
        "exec",
        "snesMemory",
        0x8000,
        0x8000,
        true,
        true,
        None,
        None,
        None,
        None,
        None,
        &[],
    )
    .unwrap();
    let p = link.last_params.unwrap();
    assert_eq!(p["kind"], "exec");
    assert_eq!(p["start"], 0x8000);
    assert_eq!(p["pause_on_hit"], true);
    assert_eq!(p["auto_savestate"], true);
    assert!(p.get("snapshot").is_none(), "빈 snapshot은 전송 안 함");

    // snapshot 스펙은 그대로 리스트로 전달
    let mut link2 = FakeLink::ok(json!({"id":4}));
    let snap = vec!["snesWorkRam:0x68:3".to_string()];
    set_breakpoint(
        &mut link2,
        "write",
        "snesMemory",
        0x2118,
        0x2118,
        true,
        false,
        None,
        None,
        None,
        None,
        None,
        &snap,
    )
    .unwrap();
    let p2 = link2.last_params.unwrap();
    assert_eq!(p2["snapshot"][0], "snesWorkRam:0x68:3");
}

#[test]
fn step_and_poll_events_forward() {
    let mut link = FakeLink::ok(json!({"events":[],"dropped":0}));
    let out = poll_events(&mut link).unwrap();
    match out {
        ToolOutput::Json(v) => assert_eq!(v["dropped"], 0),
        _ => panic!("Json 기대"),
    }
    assert_eq!(link.last_method.as_deref(), Some("poll_events"));
}

#[test]
fn screenshot_returns_image_without_save() {
    let expected_hash = crate::track::observe::sha256_hex(b"ABC");
    let mut link = FakeLink::ok(json!({
        "png_base64": "QUJD",
        "frame_before": 42,
        "frame_after": 42,
        "state": "frozen",
        "sha256": expected_hash,
    }));
    let out = screenshot(&mut link, None).unwrap();
    match out {
        ToolOutput::Image {
            png_base64,
            saved_path,
            provenance,
        } => {
            assert_eq!(png_base64, "QUJD");
            assert!(saved_path.is_none());
            assert_eq!(provenance["sha256"], expected_hash);
            assert_eq!(provenance["byte_len"], 3);
            assert_eq!(provenance["frame_before"], 42);
            assert_eq!(provenance["frame_after"], 42);
            assert_eq!(provenance["state"], "frozen");
            assert!(provenance.get("png_base64").is_none());
        }
        _ => panic!("Image 기대"),
    }
}

#[test]
fn screenshot_rejects_adapter_hash_mismatch() {
    let mut link = FakeLink::ok(json!({
        "png_base64": "QUJD",
        "sha256": "not-the-decoded-png-hash",
    }));
    let error = screenshot(&mut link, None).unwrap_err();
    assert!(error.to_string().contains("sha256 mismatch"));
}

#[test]
fn screenshot_saves_to_path_when_given() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    // "QUJD" = base64("ABC")
    let mut link = FakeLink::ok(json!({ "png_base64": "QUJD" }));
    let out = screenshot(&mut link, Some(&path)).unwrap();
    match out {
        ToolOutput::Image { saved_path, .. } => {
            assert_eq!(saved_path.as_deref(), Some(path.to_str().unwrap()))
        }
        _ => panic!("Image 기대"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"ABC");
}

/// A dump-capable fake link: `dump_memory` writes region files into the caller-provided directory
/// (as a real bridge does), and can sabotage the host's follow-up `state.json` write by occupying
/// that name with a directory — modelling a state-write failure after a successful bridge dump.
struct DumpLink {
    sabotage_state_write: bool,
    caps: Capabilities,
}
impl DumpLink {
    fn new(sabotage_state_write: bool) -> Self {
        DumpLink {
            sabotage_state_write,
            caps: Capabilities {
                protocol_version: 1,
                methods: vec![],
                memory_types: vec![],
                contracts: crate::contracts::ContractAdvertisement::Unreported,
                identity: EmulatorIdentity::default(),
            },
        }
    }
}
impl EmulatorLink for DumpLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        match method {
            "dump_memory" => {
                let path = std::path::PathBuf::from(params["path"].as_str().unwrap());
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join("main.bin"), b"NEWDUMP").unwrap();
                std::fs::write(path.join("regions.json"), br#"[{"name":"main"}]"#).unwrap();
                if self.sabotage_state_write {
                    // Occupy `state.json` with a directory so the host's later file write fails.
                    std::fs::create_dir_all(path.join("state.json")).unwrap();
                }
                Ok(json!({ "path": path.display().to_string(), "regions": 1 }))
            }
            "get_state" => Ok(json!({ "state": { "cpu.pc": 42 } })),
            _ => Ok(json!({})),
        }
    }
}

fn staging_leftovers(parent: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("dump-staging") || n.contains("dump-old"))
        .collect()
}

#[test]
fn dump_memory_publishes_region_files_and_state_together() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    let mut link = DumpLink::new(false);
    let result = dump_memory(&mut link, out.to_str().unwrap()).unwrap();
    // Region files AND the host-written state.json are all published under the requested dir.
    assert_eq!(std::fs::read(out.join("main.bin")).unwrap(), b"NEWDUMP");
    let state: Value =
        serde_json::from_slice(&std::fs::read(out.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["cpu.pc"], 42);
    // The reported path is the caller's dir, not the internal staging dir.
    match result {
        ToolOutput::Json(v) => assert_eq!(v["path"], out.to_str().unwrap()),
        _ => panic!("Json 기대"),
    }
    assert!(
        staging_leftovers(tmp.path()).is_empty(),
        "no staging/backup dirs may be left behind on success"
    );
}

#[test]
fn dump_memory_state_write_failure_preserves_prior_dump() {
    // A state.json write failure AFTER a successful bridge dump must not destroy the prior good dump:
    // the region files + state.json are staged and swapped in atomically, so a failure before the swap
    // leaves the previous dump at `dir` byte-for-byte intact and leaves no staging litter.
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("main.bin"), b"OLDDUMP").unwrap();
    std::fs::write(out.join("regions.json"), br#"[{"name":"old"}]"#).unwrap();
    std::fs::write(out.join("state.json"), br#"{"cpu.pc":1}"#).unwrap();

    let mut link = DumpLink::new(true);
    let r = dump_memory(&mut link, out.to_str().unwrap());
    assert!(r.is_err(), "a state.json write failure must fail the dump");

    assert_eq!(
        std::fs::read(out.join("main.bin")).unwrap(),
        b"OLDDUMP",
        "the prior main.bin must survive a failed re-dump"
    );
    assert_eq!(
        std::fs::read(out.join("regions.json")).unwrap(),
        br#"[{"name":"old"}]"#
    );
    assert_eq!(
        std::fs::read(out.join("state.json")).unwrap(),
        br#"{"cpu.pc":1}"#,
        "the prior state.json must survive a failed re-dump"
    );
    assert!(
        staging_leftovers(tmp.path()).is_empty(),
        "staging/backup dirs must be cleaned up on failure: {:?}",
        staging_leftovers(tmp.path())
    );
}
