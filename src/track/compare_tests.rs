use crate::track::compare::*;
use crate::track::model::{
    Artifact, Gate, GateKind, Intervention, Metric, Run, RunStatus, RUN_FORMAT_VERSION,
};

fn base_run(id: &str) -> Run {
    Run {
        format_version: RUN_FORMAT_VERSION,
        id: id.into(),
        rom_sha1: "rom".into(),
        goal: None,
        description: None,
        tags: vec![],
        status: RunStatus::Running,
        started_at: "2026-06-29T00:00:00Z".into(),
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
fn metric(key: &str, value: f64) -> Metric {
    Metric {
        id: format!("m-{key}"),
        key: key.into(),
        value,
        created_at: "t".into(),
    }
}
fn interv(id: &str, op: &str) -> Intervention {
    Intervention {
        id: id.into(),
        seq: 0,
        at_frame: None,
        at_event: None,
        frozen_context: false,
        op: op.into(),
        args: serde_json::json!({}),
        created_at: "t".into(),
    }
}
fn artifact(id: &str, kind: &str) -> Artifact {
    Artifact {
        id: id.into(),
        kind: kind.into(),
        path: "p".into(),
        sha256: "s".into(),
        meta: None,
    }
}

#[test]
fn metric_delta_computed_when_both_present() {
    let mut a = base_run("a");
    a.metrics = vec![metric("score", 10.0), metric("only_a", 1.0)];
    let mut b = base_run("b");
    b.metrics = vec![metric("score", 25.0)];
    let c = build_comparison(&a, &b);
    let score = c.metrics.iter().find(|m| m.key == "score").unwrap();
    assert_eq!(score.delta, Some(15.0));
    let only = c.metrics.iter().find(|m| m.key == "only_a").unwrap();
    assert_eq!(only.a, Some(1.0));
    assert_eq!(only.b, None);
    assert_eq!(only.delta, None);
}

#[test]
fn gate_change_classified() {
    let mut a = base_run("a");
    a.gates = vec![
        gate("g", Some(false)),
        gate("same", Some(true)),
        gate("regressed", Some(true)),
        gate("nn", None),
    ];
    let mut b = base_run("b");
    b.gates = vec![
        gate("g", Some(true)),
        gate("same", Some(true)),
        gate("new", Some(true)),
        gate("regressed", Some(false)),
        gate("nn", None),
    ];
    let c = build_comparison(&a, &b);
    assert_eq!(
        c.gates.iter().find(|x| x.name == "g").unwrap().change,
        GateChange::Improved
    );
    assert_eq!(
        c.gates.iter().find(|x| x.name == "same").unwrap().change,
        GateChange::Same
    );
    assert_eq!(
        c.gates.iter().find(|x| x.name == "new").unwrap().change,
        GateChange::Added
    );
    assert_eq!(
        c.gates
            .iter()
            .find(|x| x.name == "regressed")
            .unwrap()
            .change,
        GateChange::Regressed
    );
    assert_eq!(
        c.gates.iter().find(|x| x.name == "nn").unwrap().change,
        GateChange::Unknown
    );
}

#[test]
fn latest_gate_wins_and_count_reported() {
    let mut a = base_run("a");
    a.gates = vec![
        gate("determinism_replay", Some(false)),
        gate("determinism_replay", Some(true)),
    ];
    let b = base_run("b");
    let c = build_comparison(&a, &b);
    let g = c
        .gates
        .iter()
        .find(|x| x.name == "determinism_replay")
        .unwrap();
    assert_eq!(g.a, Some(true)); // 삽입순 마지막
    assert_eq!(g.a_count, 2);
    assert_eq!(g.b_count, 0);
    assert_eq!(g.change, GateChange::Removed); // b엔 없음
}

#[test]
fn intervention_and_artifact_counts() {
    let mut a = base_run("a");
    a.interventions = vec![
        interv("1", "write_memory"),
        interv("2", "input_burst"),
        interv("3", "input_burst"),
    ];
    a.artifacts = vec![
        artifact("a1", "screenshot"),
        artifact("a2", "screenshot"),
        artifact("a3", "savestate"),
    ];
    let b = base_run("b");
    let c = build_comparison(&a, &b);
    assert_eq!(c.interventions.a.total, 3);
    assert_eq!(c.interventions.a.by_op.get("input_burst"), Some(&2));
    assert_eq!(c.artifacts.a.get("screenshot"), Some(&2));
}

#[test]
fn serializes_with_expected_top_level_keys() {
    let c = build_comparison(&base_run("a"), &base_run("b"));
    let v = serde_json::to_value(&c).unwrap();
    for k in ["a", "b", "metrics", "gates", "interventions", "artifacts"] {
        assert!(v.get(k).is_some(), "missing key {k}");
    }
    assert!(v["interventions"].get("a").is_some());
    assert!(v["artifacts"].get("b").is_some());
}

#[test]
fn metrics_and_gates_sorted_by_name() {
    let mut a = base_run("a");
    a.metrics = vec![metric("zeta", 1.0), metric("alpha", 2.0)];
    a.gates = vec![gate("zzz", Some(true)), gate("aaa", Some(true))];
    let b = base_run("b");
    let c = build_comparison(&a, &b);
    let mkeys: Vec<&str> = c.metrics.iter().map(|m| m.key.as_str()).collect();
    assert_eq!(mkeys, vec!["alpha", "zeta"]);
    let gnames: Vec<&str> = c.gates.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(gnames, vec!["aaa", "zzz"]);
}

#[test]
fn compare_runs_reads_fs_and_errors_on_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut a = base_run("aaa");
    a.metrics = vec![metric("k", 1.0)];
    let b = base_run("bbb");
    crate::track::store::save_run(root, &a).unwrap();
    crate::track::store::save_run(root, &b).unwrap();
    let c = compare_runs(root, "aaa", "bbb").unwrap();
    assert_eq!(c.a.run_id, "aaa");
    assert!(matches!(
        compare_runs(root, "aaa", "zzz"),
        Err(CompareError::RunNotFound(_))
    ));
}
