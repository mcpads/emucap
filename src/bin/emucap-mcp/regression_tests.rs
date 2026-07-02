use super::*;
use emucap::track::observe::ObserveSpec;

/// 결정론 게이트 구동기 테스트용 목. read_memory/screenshot 응답을 큐에서 순서대로
/// 돌려줘 "재생마다 다른 관측치"를 흉내낼 수 있다. probe도 큐로.
pub(crate) struct DetReplayLink {
    caps: emucap::live::link::Capabilities,
    obs_queue: std::collections::VecDeque<&'static str>, // read_memory hex
    probe_queue: std::collections::VecDeque<&'static str>,
    poll_events: std::collections::VecDeque<serde_json::Value>,
    state_restore: Option<serde_json::Value>,
    read_memory_calls: usize,
    read_memory_fail_on: Option<usize>,
}
impl DetReplayLink {
    pub(crate) fn new(methods: &[&str]) -> Self {
        Self {
            caps: emucap::live::link::Capabilities {
                protocol_version: 1,
                methods: methods.iter().map(|m| (*m).to_string()).collect(),
                memory_types: vec![],
                identity: emucap::live::link::EmulatorIdentity::default(),
            },
            obs_queue: std::collections::VecDeque::new(),
            probe_queue: std::collections::VecDeque::new(),
            poll_events: std::collections::VecDeque::new(),
            state_restore: None,
            read_memory_calls: 0,
            read_memory_fail_on: None,
        }
    }
    pub(crate) fn obs(mut self, hexes: &[&'static str]) -> Self {
        self.obs_queue = hexes.iter().copied().collect();
        self
    }
    fn probes(mut self, hexes: &[&'static str]) -> Self {
        self.probe_queue = hexes.iter().copied().collect();
        self
    }
    fn fail_read_memory_on(mut self, n: usize) -> Self {
        self.read_memory_fail_on = Some(n);
        self
    }
}
impl EmulatorLink for DetReplayLink {
    fn capabilities(&self) -> &emucap::live::link::Capabilities {
        &self.caps
    }
    fn call(
        &mut self,
        method: &str,
        _p: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        match method {
            "reset"
            | "load_state"
            | "pause"
            | "set_input"
            | "step"
            | "set_breakpoint"
            | "clear_all_breakpoints"
            | "resume" => Ok(serde_json::json!({})),
            "status" => {
                let mut s = serde_json::json!({"connected": true, "state": "frozen"});
                if let Some(sr) = &self.state_restore {
                    s["state_restore"] = sr.clone();
                }
                Ok(s)
            }
            "poll_events" => Ok(self
                .poll_events
                .pop_front()
                .unwrap_or_else(|| serde_json::json!({"events": []}))),
            "read_memory" => {
                self.read_memory_calls += 1;
                if self.read_memory_fail_on == Some(self.read_memory_calls) {
                    return Err(LinkError::Protocol("injected read_memory failure".into()));
                }
                let hex = self.obs_queue.pop_front().unwrap_or("00");
                Ok(serde_json::json!({ "hex": hex }))
            }
            "probe" => {
                let hex = self.probe_queue.pop_front().unwrap_or("00");
                Ok(serde_json::json!({ "hex": hex }))
            }
            other => Err(LinkError::Protocol(format!("unexpected: {other}"))),
        }
    }
}

/// reset 시작 + 빈 movie인 InputReplay 케이스 디렉토리(case.json 포함).
pub(crate) fn det_input_case(
    anchor: Option<Predicate>,
) -> (tempfile::TempDir, std::path::PathBuf, regression::Case) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("inputs.movie"), "0:enter\n").unwrap();
    let case = regression::Case {
        format_version: regression::CASE_FORMAT_VERSION,
        id: "c".into(),
        description: "det".into(),
        rom: regression::RomRef {
            sha1: "unused".into(),
            path_hint: "x".into(),
        },
        repro: regression::Repro::InputReplay {
            start: "reset".into(),
            movie: "inputs.movie".into(),
            anchor,
        },
        predicate: Predicate {
            memory_type: "w".into(),
            address: 0,
            length: 2,
            op: CmpOp::Eq,
            value: 0,
        },
        expect: regression::Expect::Absent,
    };
    regression::save_case(&dir, &case).unwrap();
    (tmp, dir, case)
}

#[test]
fn determinism_equal_hashes_is_reproducible() {
    let (_t, dir, case) = det_input_case(None);
    let mut link = DetReplayLink::new(&[
        "reset",
        "pause",
        "set_input",
        "step",
        "read_memory",
        "clear_all_breakpoints",
    ])
    .obs(&["aa", "aa"]); // 두 재생 동일
    let r = verify_determinism_core(
        &mut link,
        &dir,
        &case,
        &ObserveSpec::Memory {
            memory_type: "w".into(),
            address: 0,
            length: 1,
        },
        2,
    );
    assert_eq!(r.outcome, DetOutcome::Reproducible);
    assert_eq!(r.hashes.len(), 2);
}

#[test]
fn determinism_differing_hashes_is_not_reproducible() {
    let (_t, dir, case) = det_input_case(None);
    let mut link = DetReplayLink::new(&[
        "reset",
        "pause",
        "set_input",
        "step",
        "read_memory",
        "clear_all_breakpoints",
    ])
    .obs(&["aa", "bb"]); // 재생 간 다름
    let r = verify_determinism_core(
        &mut link,
        &dir,
        &case,
        &ObserveSpec::Memory {
            memory_type: "w".into(),
            address: 0,
            length: 1,
        },
        2,
    );
    assert_eq!(r.outcome, DetOutcome::NotReproducible);
}

#[test]
fn determinism_aborts_on_second_replay_error() {
    // 재생1 read_memory ok, 재생2 read_memory 실패 → MeasurementInvalid(repro_error),
    // 조용히 Reproducible로 떨어지지 않음(어느 재생이든 Err면 중단).
    let (_t, dir, case) = det_input_case(None);
    let mut link = DetReplayLink::new(&[
        "reset",
        "pause",
        "set_input",
        "step",
        "read_memory",
        "clear_all_breakpoints",
    ])
    .obs(&["aa"])
    .fail_read_memory_on(2);
    let r = verify_determinism_core(
        &mut link,
        &dir,
        &case,
        &ObserveSpec::Memory {
            memory_type: "w".into(),
            address: 0,
            length: 1,
        },
        2,
    );
    assert!(matches!(r.outcome, DetOutcome::MeasurementInvalid(_)));
    assert_ne!(r.outcome, DetOutcome::Reproducible);
}

#[test]
fn determinism_anchor_miss_is_measurement_invalid_not_drift() {
    let (_t, dir, case) = det_input_case(Some(Predicate {
        memory_type: "w".into(),
        address: 0x100,
        length: 1,
        op: CmpOp::Eq,
        value: 0,
    }));
    // poll_events 항상 빈 → anchor 미히트
    let mut link = DetReplayLink::new(&[
        "reset",
        "pause",
        "set_input",
        "step",
        "read_memory",
        "clear_all_breakpoints",
        "set_breakpoint",
        "poll_events",
    ]);
    let r = verify_determinism_core(
        &mut link,
        &dir,
        &case,
        &ObserveSpec::Memory {
            memory_type: "w".into(),
            address: 0,
            length: 1,
        },
        2,
    );
    assert_eq!(
        r.outcome,
        DetOutcome::MeasurementInvalid("anchor_miss".into())
    );
}

#[test]
fn savestate_broad_observe_is_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("deadbeef.mss"), b"x").unwrap();
    let case = regression::Case {
        format_version: regression::CASE_FORMAT_VERSION,
        id: "s".into(),
        description: "s".into(),
        rom: regression::RomRef {
            sha1: "unused".into(),
            path_hint: "x".into(),
        },
        repro: regression::Repro::Savestate {
            state_sha1: "deadbeef".into(),
            advance_frames: 1,
        },
        predicate: Predicate {
            memory_type: "w".into(),
            address: 0,
            length: 1,
            op: CmpOp::Eq,
            value: 0,
        },
        expect: regression::Expect::Absent,
    };
    let mut link = DetReplayLink::new(&["probe", "screenshot"]);
    let r = verify_determinism_core(&mut link, &dir, &case, &ObserveSpec::Screenshot, 2);
    assert_eq!(
        r.outcome,
        DetOutcome::Unsupported("savestate_broad_observe".into())
    );
}

#[test]
fn savestate_memory_uses_atomic_probe() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("deadbeef.mss"), b"x").unwrap();
    let case = regression::Case {
        format_version: regression::CASE_FORMAT_VERSION,
        id: "s".into(),
        description: "s".into(),
        rom: regression::RomRef {
            sha1: "unused".into(),
            path_hint: "x".into(),
        },
        repro: regression::Repro::Savestate {
            state_sha1: "deadbeef".into(),
            advance_frames: 1,
        },
        predicate: Predicate {
            memory_type: "w".into(),
            address: 0,
            length: 2,
            op: CmpOp::Eq,
            value: 0,
        },
        expect: regression::Expect::Absent,
    };
    let mut link = DetReplayLink::new(&["probe"]).probes(&["1234", "1234"]);
    let r = verify_determinism_core(
        &mut link,
        &dir,
        &case,
        &ObserveSpec::Memory {
            memory_type: "w".into(),
            address: 0,
            length: 2,
        },
        2,
    );
    assert_eq!(r.outcome, DetOutcome::Reproducible);
}

#[test]
fn default_session_port_does_not_wrap() {
    assert_eq!(default_session_port(47800), 47900);
    assert_eq!(
        default_session_port(65535),
        65535,
        "u16 wrap 금지(saturating)"
    );
    assert_eq!(default_session_port(65500), 65535, "saturating으로 포화");
}

#[test]
fn validate_movie_frames_rejects_absurd_and_nonmonotonic() {
    use emucap::analysis::regression::{Movie, MovieFrame};
    let absurd = Movie {
        frames: vec![MovieFrame {
            frame: 5_000_000,
            buttons: vec![],
        }],
    };
    assert!(
        validate_movie_frames(&absurd).is_err(),
        "상한 초과 frame은 거부"
    );
    let nonmono = Movie {
        frames: vec![
            MovieFrame {
                frame: 10,
                buttons: vec![],
            },
            MovieFrame {
                frame: 3,
                buttons: vec![],
            },
        ],
    };
    assert!(
        validate_movie_frames(&nonmono).is_err(),
        "비단조 frame은 거부"
    );
    // 중복 frame도 거부(재생 루프의 조용한 버튼 드롭 방지)
    let dup = Movie {
        frames: vec![
            MovieFrame {
                frame: 10,
                buttons: vec![],
            },
            MovieFrame {
                frame: 10,
                buttons: vec!["a".into()],
            },
        ],
    };
    assert!(
        validate_movie_frames(&dup).is_err(),
        "중복 frame은 거부(엄격 증가)"
    );
    let ok = Movie {
        frames: vec![
            MovieFrame {
                frame: 0,
                buttons: vec![],
            },
            MovieFrame {
                frame: 10,
                buttons: vec![],
            },
        ],
    };
    assert!(validate_movie_frames(&ok).is_ok());
}

struct LazyNoProbe {
    caps: emucap::live::link::Capabilities,
    calls: Vec<String>,
}

impl LazyNoProbe {
    fn new() -> Self {
        Self {
            caps: emucap::live::link::Capabilities::empty(),
            calls: Vec::new(),
        }
    }
}

impl EmulatorLink for LazyNoProbe {
    fn capabilities(&self) -> &emucap::live::link::Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.calls.push(method.to_string());
        if method == "status" {
            self.caps.protocol_version = 1;
            self.caps.methods = vec!["status".into(), "read_memory".into()];
            return Ok(serde_json::json!({"connected": true, "state": "frozen"}));
        }
        Err(LinkError::Protocol(format!("unexpected call: {method}")))
    }
}

#[test]
fn savestate_regression_without_probe_is_unsupported_after_capability_load() {
    let tmp = tempfile::tempdir().unwrap();
    let case_dir = tmp.path().join("pc98_case");
    std::fs::create_dir_all(&case_dir).unwrap();
    let case = regression::Case {
        format_version: regression::CASE_FORMAT_VERSION,
        id: "pc98_case".into(),
        description: "probe 없는 어댑터".into(),
        rom: regression::RomRef {
            sha1: "unused".into(),
            path_hint: "disk.hdi".into(),
        },
        repro: regression::Repro::Savestate {
            state_sha1: "missing".into(),
            advance_frames: 0,
        },
        predicate: Predicate {
            memory_type: "ram".into(),
            address: 0,
            length: 1,
            op: CmpOp::Eq,
            value: 0,
        },
        expect: regression::Expect::Absent,
    };
    let mut link = LazyNoProbe::new();
    let verdict = run_one_case(&mut link, &case_dir, &case);
    assert_eq!(verdict, regression::Verdict::Unsupported);
    assert_eq!(link.calls, vec!["status"]);
}

struct Pc98InputReplayLink {
    caps: emucap::live::link::Capabilities,
    calls: Vec<String>,
    read_hex: &'static str,
    state_restore: Option<serde_json::Value>,
    poll_events: Vec<serde_json::Value>,
}

impl Pc98InputReplayLink {
    fn new(methods: &[&str], read_hex: &'static str) -> Self {
        Self {
            caps: emucap::live::link::Capabilities {
                protocol_version: 1,
                methods: methods.iter().map(|m| (*m).to_string()).collect(),
                memory_types: vec![],
                identity: emucap::live::link::EmulatorIdentity {
                    system: Some("pc98".into()),
                    adapter: Some("mame-pc98-gdb".into()),
                    ..Default::default()
                },
            },
            calls: Vec::new(),
            read_hex,
            state_restore: None,
            poll_events: Vec::new(),
        }
    }

    fn with_state_restore(mut self, state_restore: serde_json::Value) -> Self {
        self.state_restore = Some(state_restore);
        self
    }

    fn with_poll_events(mut self, poll_events: Vec<serde_json::Value>) -> Self {
        self.poll_events = poll_events;
        self
    }
}

impl EmulatorLink for Pc98InputReplayLink {
    fn capabilities(&self) -> &emucap::live::link::Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.calls.push(method.to_string());
        match method {
            "reset"
            | "load_state"
            | "pause"
            | "set_input"
            | "step"
            | "set_breakpoint"
            | "clear_all_breakpoints" => Ok(serde_json::json!({})),
            "status" => {
                let mut status = serde_json::json!({"connected": true, "state": "frozen"});
                if let Some(state_restore) = &self.state_restore {
                    status["state_restore"] = state_restore.clone();
                }
                Ok(status)
            }
            "poll_events" => {
                if self.poll_events.is_empty() {
                    Ok(serde_json::json!({"events": []}))
                } else {
                    Ok(self.poll_events.remove(0))
                }
            }
            "read_memory" => Ok(serde_json::json!({"hex": self.read_hex})),
            other => Err(LinkError::Protocol(format!("unexpected call: {other}"))),
        }
    }
}

fn input_replay_case() -> (tempfile::TempDir, std::path::PathBuf, regression::Case) {
    let tmp = tempfile::tempdir().unwrap();
    let case_dir = tmp.path().join("pc98_input");
    std::fs::create_dir_all(&case_dir).unwrap();
    std::fs::write(case_dir.join("inputs.movie"), "0:enter\n").unwrap();
    let case = regression::Case {
        format_version: regression::CASE_FORMAT_VERSION,
        id: "pc98_input".into(),
        description: "PC-98 reset input replay".into(),
        rom: regression::RomRef {
            sha1: "unused".into(),
            path_hint: "disk.hdi".into(),
        },
        repro: regression::Repro::InputReplay {
            start: "reset".into(),
            movie: "inputs.movie".into(),
            anchor: None,
        },
        predicate: Predicate {
            memory_type: "ram".into(),
            address: 0x9000,
            length: 1,
            op: CmpOp::Eq,
            value: 0,
        },
        expect: regression::Expect::Absent,
    };
    (tmp, case_dir, case)
}

#[test]
fn pc98_input_replay_without_probe_can_pass_from_reset() {
    let (_tmp, case_dir, case) = input_replay_case();
    let mut link = Pc98InputReplayLink::new(
        &[
            "reset",
            "pause",
            "set_input",
            "step",
            "read_memory",
            "clear_all_breakpoints",
        ],
        "01",
    );
    let verdict = run_one_case(&mut link, &case_dir, &case);
    assert_eq!(verdict, regression::Verdict::Pass);
    assert_eq!(
        link.calls,
        vec![
            "reset",
            "pause",
            "set_input",
            "step",
            "clear_all_breakpoints",
            "read_memory"
        ]
    );
}

#[test]
fn load_state_input_replay_is_unsupported_when_state_is_not_deterministic() {
    let (tmp, case_dir, mut case) = input_replay_case();
    let state_sha1 = "base_state";
    std::fs::write(case_dir.join(format!("{state_sha1}.mss")), b"state").unwrap();
    case.repro = regression::Repro::InputReplay {
        start: state_sha1.into(),
        movie: "inputs.movie".into(),
        anchor: None,
    };
    let mut link = Pc98InputReplayLink::new(
        &[
            "status",
            "load_state",
            "pause",
            "set_input",
            "step",
            "read_memory",
            "clear_all_breakpoints",
        ],
        "01",
    )
    .with_state_restore(serde_json::json!({
        "deterministic_replay": false,
        "hidden_device_state": false,
        "post_restore_instruction_exact": false
    }));

    let verdict = run_one_case(&mut link, &case_dir, &case);
    drop(tmp);
    assert_eq!(verdict, regression::Verdict::Unsupported);
    assert_eq!(
        link.calls,
        vec!["status"],
        "unsafe load_state replay must stop before loading or stepping"
    );
}

#[test]
fn load_state_input_replay_can_pass_when_state_is_deterministic() {
    let (tmp, case_dir, mut case) = input_replay_case();
    let state_sha1 = "base_state";
    std::fs::write(case_dir.join(format!("{state_sha1}.mss")), b"state").unwrap();
    case.repro = regression::Repro::InputReplay {
        start: state_sha1.into(),
        movie: "inputs.movie".into(),
        anchor: None,
    };
    let mut link = Pc98InputReplayLink::new(
        &[
            "status",
            "load_state",
            "pause",
            "set_input",
            "step",
            "read_memory",
            "clear_all_breakpoints",
        ],
        "01",
    )
    .with_state_restore(serde_json::json!({
        "deterministic_replay": true,
        "hidden_device_state": true,
        "post_restore_instruction_exact": true
    }));

    let verdict = run_one_case(&mut link, &case_dir, &case);
    drop(tmp);
    assert_eq!(verdict, regression::Verdict::Pass);
    assert_eq!(
        link.calls,
        vec![
            "status",
            "load_state",
            "pause",
            "set_input",
            "step",
            "clear_all_breakpoints",
            "read_memory"
        ]
    );
}

#[test]
fn input_replay_anchor_hit_during_gap_is_polled_before_next_input() {
    let (_tmp, case_dir, mut case) = input_replay_case();
    std::fs::write(case_dir.join("inputs.movie"), "3:enter\n").unwrap();
    case.repro = regression::Repro::InputReplay {
        start: "reset".into(),
        movie: "inputs.movie".into(),
        anchor: Some(Predicate {
            memory_type: "cpu".into(),
            address: 0x1234,
            length: 1,
            op: CmpOp::Eq,
            value: 0,
        }),
    };
    let mut link = Pc98InputReplayLink::new(
        &[
            "reset",
            "pause",
            "set_input",
            "step",
            "read_memory",
            "clear_all_breakpoints",
            "set_breakpoint",
            "poll_events",
        ],
        "01",
    )
    .with_poll_events(vec![
        // arming 전 stale-event 드레인이 첫 poll_events를 소비한다. 이후 pre-poll·
        // step1·step2 순으로 폴링해 2 step 뒤 anchor hit — frame 3 입력 전에 멈춘다.
        serde_json::json!({"events": []}),
        serde_json::json!({"events": []}),
        serde_json::json!({"events": []}),
        serde_json::json!({"events": [{"type": "breakpoint_hit", "address": 0x1234}]}),
    ]);

    let verdict = run_one_case(&mut link, &case_dir, &case);
    assert_eq!(verdict, regression::Verdict::Pass);
    assert_eq!(
        link.calls.iter().filter(|m| m.as_str() == "step").count(),
        2,
        "anchor hit during the empty-frame gap should stop before frame 3 input"
    );
}

#[test]
fn input_replay_missing_required_method_is_unsupported() {
    let (_tmp, case_dir, case) = input_replay_case();
    let mut link = Pc98InputReplayLink::new(
        &[
            "reset",
            "pause",
            "set_input",
            "read_memory",
            "clear_all_breakpoints",
        ],
        "01",
    );
    let verdict = run_one_case(&mut link, &case_dir, &case);
    assert_eq!(verdict, regression::Verdict::Unsupported);
    assert!(
        link.calls.is_empty(),
        "missing method should be gated before replay"
    );
}

#[test]
fn parse_observe_memory_rejects_zero_length() {
    let r = parse_observe_spec(Some("memory"), Some("w".into()), Some(0), Some(0));
    assert!(r.is_err());
}
