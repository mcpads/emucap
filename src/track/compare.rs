//! 두 run을 구조화 diff한다(메트릭 delta·게이트 변화·재현성·개입·산출물 집계). 순수 읽기,
//! 에뮬레이터 무통신. gates/metrics는 append-only라 name/key별 삽입순 마지막을
//! 대표로 집고 발생 횟수를 함께 노출한다(다중성 은폐 금지).
use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::track::model::{Gate, GateKind, ReproStatus, Run, RunStatus};
use crate::track::store::{self, TrackError};

#[derive(Debug, Serialize, PartialEq)]
pub struct RunMeta {
    pub run_id: String,
    pub goal: Option<String>,
    pub status: RunStatus,
    pub repro_status: Option<ReproStatus>,
    pub agent: Option<String>,
    pub started_at: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct MetricDelta {
    pub key: String,
    pub a: Option<f64>,
    pub b: Option<f64>,
    pub delta: Option<f64>,
    pub a_count: u64,
    pub b_count: u64,
}

#[derive(Debug, Serialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum GateChange {
    Added,
    Removed,
    Improved,
    Regressed,
    Same,
    Unknown,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct GateDiff {
    pub name: String,
    pub a: Option<bool>,
    pub b: Option<bool>,
    pub change: GateChange,
    pub kind: Option<GateKind>, // 존재하는 쪽(b 우선)의 게이트 종류
    pub a_count: u64,
    pub b_count: u64,
}

#[derive(Debug, Serialize, PartialEq, Default)]
pub struct InterventionSummary {
    pub total: u64,
    pub by_op: BTreeMap<String, u64>,
}

/// run A·B 양쪽 값을 nested {a,b}로 묶는다(top-level a/b와 일관).
#[derive(Debug, Serialize, PartialEq)]
pub struct SidePair<T> {
    pub a: T,
    pub b: T,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct RunComparison {
    pub a: RunMeta,
    pub b: RunMeta,
    pub metrics: Vec<MetricDelta>,
    pub gates: Vec<GateDiff>,
    pub interventions: SidePair<InterventionSummary>,
    pub artifacts: SidePair<BTreeMap<String, u64>>,
}

#[derive(Debug)]
pub enum CompareError {
    RunNotFound(String),
    Store(TrackError),
}
impl std::fmt::Display for CompareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompareError::RunNotFound(id) => write!(f, "run을 찾을 수 없음: {id}"),
            CompareError::Store(e) => write!(f, "store: {e}"),
        }
    }
}

fn meta(run: &Run) -> RunMeta {
    RunMeta {
        run_id: run.id.clone(),
        goal: run.goal.clone(),
        status: run.status.clone(),
        repro_status: run.repro_status,
        agent: run.agent.clone(),
        started_at: run.started_at.clone(),
        tags: run.tags.clone(),
    }
}

/// name별 (삽입순 마지막 게이트, 발생 횟수). Vec 순서 = 추가 순서.
pub(crate) fn latest_gates(run: &Run) -> BTreeMap<String, (&Gate, u64)> {
    let mut m: BTreeMap<String, (&Gate, u64)> = BTreeMap::new();
    for g in &run.gates {
        m.entry(g.name.clone())
            .and_modify(|(last, cnt)| {
                *last = g;
                *cnt += 1;
            })
            .or_insert((g, 1));
    }
    m
}

/// key별 (삽입순 마지막 값, 발생 횟수).
fn latest_metrics(run: &Run) -> BTreeMap<String, (f64, u64)> {
    let mut m: BTreeMap<String, (f64, u64)> = BTreeMap::new();
    for met in &run.metrics {
        m.entry(met.key.clone())
            .and_modify(|(v, c)| {
                *v = met.value;
                *c += 1;
            })
            .or_insert((met.value, 1));
    }
    m
}

fn intervention_summary(run: &Run) -> InterventionSummary {
    let mut by_op: BTreeMap<String, u64> = BTreeMap::new();
    for i in &run.interventions {
        *by_op.entry(i.op.clone()).or_insert(0) += 1;
    }
    InterventionSummary {
        total: run.interventions.len() as u64,
        by_op,
    }
}

fn artifact_counts(run: &Run) -> BTreeMap<String, u64> {
    let mut m: BTreeMap<String, u64> = BTreeMap::new();
    for a in &run.artifacts {
        *m.entry(a.kind.clone()).or_insert(0) += 1;
    }
    m
}

fn classify_gate(pa: bool, pb: bool, a: Option<bool>, b: Option<bool>) -> GateChange {
    match (pa, pb) {
        (false, true) => GateChange::Added,
        (true, false) => GateChange::Removed,
        (true, true) => match (a, b) {
            (Some(false), Some(true)) => GateChange::Improved,
            (Some(true), Some(false)) => GateChange::Regressed,
            (Some(x), Some(y)) if x == y => GateChange::Same,
            _ => GateChange::Unknown,
        },
        (false, false) => GateChange::Unknown, // 합집합이라 도달 불가, 방어
    }
}

pub(crate) fn build_comparison(a: &Run, b: &Run) -> RunComparison {
    let ga = latest_gates(a);
    let gb = latest_gates(b);
    let ma = latest_metrics(a);
    let mb = latest_metrics(b);

    // metrics: 키 합집합 사전순
    let mut keys: Vec<String> = ma.keys().chain(mb.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    let metrics = keys
        .into_iter()
        .map(|k| {
            let av = ma.get(&k);
            let bv = mb.get(&k);
            let a_val = av.map(|(v, _)| *v);
            let b_val = bv.map(|(v, _)| *v);
            let delta = match (a_val, b_val) {
                (Some(x), Some(y)) => Some(y - x),
                _ => None,
            };
            MetricDelta {
                key: k,
                a: a_val,
                b: b_val,
                delta,
                a_count: av.map(|(_, c)| *c).unwrap_or(0),
                b_count: bv.map(|(_, c)| *c).unwrap_or(0),
            }
        })
        .collect();

    // gates: 이름 합집합 사전순
    let mut names: Vec<String> = ga.keys().chain(gb.keys()).cloned().collect();
    names.sort();
    names.dedup();
    let gates = names
        .into_iter()
        .map(|name| {
            let ag = ga.get(&name);
            let bg = gb.get(&name);
            let a_passed = ag.and_then(|(g, _)| g.passed);
            let b_passed = bg.and_then(|(g, _)| g.passed);
            let change = classify_gate(ag.is_some(), bg.is_some(), a_passed, b_passed);
            let kind = bg.or(ag).map(|(g, _)| g.kind);
            GateDiff {
                name,
                a: a_passed,
                b: b_passed,
                change,
                kind,
                a_count: ag.map(|(_, c)| *c).unwrap_or(0),
                b_count: bg.map(|(_, c)| *c).unwrap_or(0),
            }
        })
        .collect();

    RunComparison {
        a: meta(a),
        b: meta(b),
        metrics,
        gates,
        interventions: SidePair {
            a: intervention_summary(a),
            b: intervention_summary(b),
        },
        artifacts: SidePair {
            a: artifact_counts(a),
            b: artifact_counts(b),
        },
    }
}

/// 두 run_id(전역 유일)를 find_run_by_id로 타깃 로드해 비교한다. 미존재는 에러(조용한 실패 금지).
pub fn compare_runs(root: &Path, id_a: &str, id_b: &str) -> Result<RunComparison, CompareError> {
    let a = store::find_run_by_id(root, id_a)
        .map_err(CompareError::Store)?
        .ok_or_else(|| CompareError::RunNotFound(id_a.to_string()))?;
    let b = store::find_run_by_id(root, id_b)
        .map_err(CompareError::Store)?
        .ok_or_else(|| CompareError::RunNotFound(id_b.to_string()))?;
    Ok(build_comparison(&a, &b))
}
