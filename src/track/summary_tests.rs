use crate::track::model::{
    Gate, GateKind, Intervention, ReproStatus, Run, RunStatus, RUN_FORMAT_VERSION,
};
use crate::track::summary::*;

fn base_run(id: &str, goal: &str) -> Run {
    Run {
        format_version: RUN_FORMAT_VERSION,
        id: id.into(),
        rom_sha1: "rom".into(),
        goal: Some(goal.into()),
        description: None,
        tags: vec![],
        status: RunStatus::Done,
        started_at: format!("2026-06-29T00:00:0{}Z", id.len() % 10),
        ended_at: None,
        agent: None,
        session: None,
        connection_ref: None,
        repro_base: None,
        repro_movie_ref: None,
        repro_status: None,
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    }
}
fn gate(name: &str, passed: Option<bool>) -> Gate {
    Gate {
        id: format!("g-{name}"),
        name: name.into(),
        kind: GateKind::Machine,
        passed,
        evidence_ref: None,
        detail: None,
        case_ref: None,
        created_at: "t".into(),
    }
}

#[test]
fn filters_and_status_repro_distribution() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut a = base_run("aaa", "fix-x");
    a.tags = vec!["t1".into()];
    a.repro_status = Some(ReproStatus::SavestateOnly);
    let mut b = base_run("bbb", "fix-x");
    b.status = RunStatus::Aborted; // repro_status None → "none"
    let c = base_run("ccc", "other-goal");
    crate::track::store::save_run(root, &a).unwrap();
    crate::track::store::save_run(root, &b).unwrap();
    crate::track::store::save_run(root, &c).unwrap();

    let s = summarize_runs(
        root,
        &SummaryFilter {
            goal: Some("fix-x".into()),
            tag: None,
            rom_sha1: None,
        },
    )
    .unwrap();
    assert_eq!(s.total, 2); // a,b (c는 goal 불일치)
    assert_eq!(s.by_status.get("done"), Some(&1));
    assert_eq!(s.by_status.get("aborted"), Some(&1));
    assert_eq!(s.by_repro_status.get("savestate_only"), Some(&1));
    assert_eq!(s.by_repro_status.get("none"), Some(&1));

    // tag 정확 일치
    let st = summarize_runs(
        root,
        &SummaryFilter {
            goal: None,
            tag: Some("t1".into()),
            rom_sha1: None,
        },
    )
    .unwrap();
    assert_eq!(st.total, 1);
}

#[test]
fn gate_stats_and_intervention_ops() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut a = base_run("aaa", "g");
    a.gates = vec![gate("det", Some(true))];
    a.interventions = vec![Intervention {
        id: "1".into(),
        seq: 0,
        at_frame: None,
        at_event: None,
        frozen_context: false,
        op: "input_burst".into(),
        args: serde_json::json!({}),
        created_at: "t".into(),
    }];
    let mut b = base_run("bbb", "g");
    // 같은 게이트 다회 → 삽입순 마지막(false) 대표
    b.gates = vec![gate("det", Some(true)), gate("det", Some(false))];
    crate::track::store::save_run(root, &a).unwrap();
    crate::track::store::save_run(root, &b).unwrap();

    let s = summarize_runs(root, &SummaryFilter::default()).unwrap();
    let det = s.gates.iter().find(|g| g.name == "det").unwrap();
    assert_eq!(det.passed, 1); // a
    assert_eq!(det.failed, 1); // b(마지막 false)
    assert_eq!(s.intervention_ops.get("input_burst"), Some(&1));
    // per-run capsule: b의 det 대표=Some(false)
    let bc = s.runs.iter().find(|r| r.run_id == "bbb").unwrap();
    assert_eq!(bc.gate_summary.get("det"), Some(&Some(false)));
}

#[test]
fn corrupt_run_is_skipped_with_count() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let a = base_run("aaa", "g");
    crate::track::store::save_run(root, &a).unwrap();
    // 손상 run.json 직접 작성
    let bad_dir = root.join("roms").join("rom").join("runs").join("zzz");
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(bad_dir.join("run.json"), b"{ not json").unwrap();

    let s = summarize_runs(root, &SummaryFilter::default()).unwrap();
    assert_eq!(s.total, 1); // a만
    assert_eq!(s.skipped, 1); // 손상 1
    assert_eq!(s.skipped_runs, vec!["zzz".to_string()]);
}

#[test]
fn empty_match_is_total_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let s = summarize_runs(tmp.path(), &SummaryFilter::default()).unwrap();
    assert_eq!(s.total, 0);
    assert_eq!(s.skipped, 0);
}
