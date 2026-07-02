use super::model::*;

#[test]
fn run_round_trips_through_json() {
    let run = Run {
        format_version: RUN_FORMAT_VERSION,
        id: "01J0RUN".into(),
        rom_sha1: "abc123".into(),
        goal: Some("opening-text".into()),
        description: Some("첫 시도".into()),
        tags: vec!["font".into()],
        status: RunStatus::Running,
        started_at: "2026-06-29T00:00:00Z".into(),
        ended_at: None,
        agent: None,
        session: None,
        connection_ref: Some("g1".into()),
        repro_base: Some("reset".into()),
        repro_movie_ref: None,
        repro_status: Some(ReproStatus::ReplayableWithInterventions),
        gates: vec![Gate {
            id: "01J0GATE".into(),
            name: "no-crash".into(),
            kind: GateKind::Machine,
            passed: Some(true),
            evidence_ref: Some("artifact:01J0ART".into()),
            detail: None,
            case_ref: None,
            created_at: "2026-06-29T00:00:01Z".into(),
        }],
        metrics: vec![Metric {
            id: "01J0MET".into(),
            key: "diff_bytes".into(),
            value: 12.0,
            created_at: "2026-06-29T00:00:02Z".into(),
        }],
        artifacts: vec![Artifact {
            id: "01J0ART".into(),
            kind: "screenshot".into(),
            path: "artifacts/shot.png".into(),
            sha256: "deadbeef".into(),
            meta: None,
        }],
        interventions: vec![Intervention {
            id: "01J0IV".into(),
            seq: 0,
            at_frame: Some(100),
            at_event: None,
            frozen_context: true,
            op: "write_memory".into(),
            args: serde_json::json!({"space":"wram","address":256,"bytes":"00"}),
            created_at: "2026-06-29T00:00:03Z".into(),
        }],
    };
    let json = serde_json::to_string(&run).unwrap();
    let back: Run = serde_json::from_str(&json).unwrap();
    assert_eq!(run, back);
}

#[test]
fn enums_serialize_snake_case() {
    assert_eq!(
        serde_json::to_string(&RunStatus::Aborted).unwrap(),
        "\"aborted\""
    );
    assert_eq!(
        serde_json::to_string(&GateKind::Judgment).unwrap(),
        "\"judgment\""
    );
    assert_eq!(
        serde_json::to_string(&ReproStatus::SavestateOnly).unwrap(),
        "\"savestate_only\""
    );
}
