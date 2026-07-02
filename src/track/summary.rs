//! run 횡단 rollup — goal/tag/rom로 묶은 run들의 사실 집계(상태·재현성·게이트 통과율·개입
//! op 빈도·per-run 캡슐). **성공 판정은 안 한다** — 패턴 추론은 AI 몫. 순수 읽기.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::track::compare::latest_gates;
use crate::track::model::{ReproStatus, Run, RunStatus};
use crate::track::store::{self, TrackError};

#[derive(Debug, Serialize, PartialEq, Default)]
pub struct SummaryFilter {
    pub goal: Option<String>,
    pub tag: Option<String>,
    pub rom_sha1: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct GateStat {
    pub name: String,
    pub passed: u64,
    pub failed: u64,
    pub inconclusive: u64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct RunCapsule {
    pub run_id: String,
    pub goal: Option<String>,
    pub status: RunStatus,
    pub repro_status: Option<ReproStatus>,
    pub tags: Vec<String>,
    pub gate_summary: BTreeMap<String, Option<bool>>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct RunsSummary {
    pub total: u64,
    pub skipped: u64,
    pub skipped_runs: Vec<String>, // 손상으로 건너뛴 run_id(소비자가 어떤 run인지 알도록)
    pub by_status: BTreeMap<String, u64>,
    pub by_repro_status: BTreeMap<String, u64>,
    pub gates: Vec<GateStat>,
    pub intervention_ops: BTreeMap<String, u64>,
    pub metric_keys: Vec<String>,
    pub runs: Vec<RunCapsule>,
}

/// enum을 serde snake_case 문자열 키로(by_status/by_repro_status용).
fn enum_key<T: Serialize + std::fmt::Debug>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => format!("{v:?}"), // 미래 비-unit 변형: 묵음 "" 대신 식별 가능한 키
    }
}

fn matches(r: &Run, f: &SummaryFilter) -> bool {
    if let Some(g) = &f.goal {
        if r.goal.as_deref() != Some(g.as_str()) {
            return false;
        }
    }
    if let Some(rom) = &f.rom_sha1 {
        if &r.rom_sha1 != rom {
            return false;
        }
    }
    if let Some(t) = &f.tag {
        if !r.tags.iter().any(|x| x == t) {
            return false;
        }
    }
    true
}

pub fn summarize_runs(root: &Path, filter: &SummaryFilter) -> Result<RunsSummary, TrackError> {
    let (all, skipped) = store::walk_runs_lenient(root)?;
    let skipped_runs: Vec<String> = skipped
        .iter()
        .filter_map(|p| {
            p.parent()
                .and_then(|d| d.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
        .collect();
    let mut matched: Vec<&Run> = all.iter().filter(|r| matches(r, filter)).collect();
    matched.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));

    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_repro_status: BTreeMap<String, u64> = BTreeMap::new();
    let mut intervention_ops: BTreeMap<String, u64> = BTreeMap::new();
    let mut metric_set: BTreeSet<String> = BTreeSet::new();
    let mut gate_acc: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new(); // (pass,fail,inconclusive)
    let mut runs = Vec::new();

    for r in &matched {
        *by_status.entry(enum_key(&r.status)).or_insert(0) += 1;
        let rk = r
            .repro_status
            .map(|s| enum_key(&s))
            .unwrap_or_else(|| "none".into());
        *by_repro_status.entry(rk).or_insert(0) += 1;
        for iv in &r.interventions {
            *intervention_ops.entry(iv.op.clone()).or_insert(0) += 1;
        }
        for m in &r.metrics {
            metric_set.insert(m.key.clone());
        }
        let lg = latest_gates(r); // BTreeMap<String,(&Gate,u64)>, 삽입순 마지막 대표
        let mut gate_summary = BTreeMap::new();
        for (name, (g, _)) in &lg {
            gate_summary.insert(name.clone(), g.passed);
            let e = gate_acc.entry(name.clone()).or_insert((0, 0, 0));
            match g.passed {
                Some(true) => e.0 += 1,
                Some(false) => e.1 += 1,
                None => e.2 += 1,
            }
        }
        runs.push(RunCapsule {
            run_id: r.id.clone(),
            goal: r.goal.clone(),
            status: r.status.clone(),
            repro_status: r.repro_status,
            tags: r.tags.clone(),
            gate_summary,
        });
    }

    let gates = gate_acc
        .into_iter()
        .map(|(name, (passed, failed, inconclusive))| GateStat {
            name,
            passed,
            failed,
            inconclusive,
        })
        .collect();

    Ok(RunsSummary {
        total: matched.len() as u64,
        skipped: skipped.len() as u64,
        skipped_runs,
        by_status,
        by_repro_status,
        gates,
        intervention_ops,
        metric_keys: metric_set.into_iter().collect(),
        runs,
    })
}
