use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 노이즈 키(타이밍·카운터·사운드 등): 소문자 부분 문자열로 매칭해 제외한다. 교차-ROM에서
/// 두 실행의 절대 타이밍은 항상 다르므로 신호가 아니다. 필요하면 `extra`로 더 제외한다.
pub const DEFAULT_NOISE: &[&str] = &[
    "clock",
    "counter",
    "cycle",
    "framecount",
    "scanline",
    "spc.",
    "openbus",
    "nextevent",
    "refresh",
    "masterclock",
    "pollcounter",
    "hcounter",
    "vcounter",
    "prevcpu",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyDiff {
    pub key: String,
    pub a: Value,
    pub b: Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct StateDiff {
    pub diffs: Vec<KeyDiff>,
    pub only_in_a: Vec<String>,
    pub only_in_b: Vec<String>,
    pub ignored: usize,
}

fn is_noise(key: &str, patterns: &[String]) -> bool {
    let lk = key.to_lowercase();
    patterns.iter().any(|p| lk.contains(p.as_str()))
}

/// 두 상태 맵을 키별로 비교한다. 노이즈 키(기본 + extra)는 제외.
pub fn state_diff(
    a: &BTreeMap<String, Value>,
    b: &BTreeMap<String, Value>,
    extra: &[String],
) -> StateDiff {
    let mut patterns: Vec<String> = DEFAULT_NOISE.iter().map(|s| s.to_string()).collect();
    patterns.extend(extra.iter().map(|s| s.to_lowercase()));

    let mut diffs = Vec::new();
    let mut only_in_a = Vec::new();
    let mut ignored = 0;
    for (k, va) in a {
        if is_noise(k, &patterns) {
            ignored += 1;
            continue;
        }
        match b.get(k) {
            Some(vb) => {
                if va != vb {
                    diffs.push(KeyDiff {
                        key: k.clone(),
                        a: va.clone(),
                        b: vb.clone(),
                    });
                }
            }
            None => only_in_a.push(k.clone()),
        }
    }
    let mut only_in_b = Vec::new();
    for k in b.keys() {
        if !is_noise(k, &patterns) && !a.contains_key(k) {
            only_in_b.push(k.clone());
        }
    }
    StateDiff {
        diffs,
        only_in_a,
        only_in_b,
        ignored,
    }
}

pub fn render_table(sd: &StateDiff) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "상태(레지스터/DMA/PPU) 키 디프 — 다른 키 {}개 (노이즈 {}개 제외)\n",
        sd.diffs.len(),
        sd.ignored
    ));
    for d in &sd.diffs {
        s.push_str(&format!("  {} : {} → {}\n", d.key, d.a, d.b));
    }
    if !sd.only_in_a.is_empty() || !sd.only_in_b.is_empty() {
        s.push_str(&format!(
            "  (한쪽에만: A {}개, B {}개)\n",
            sd.only_in_a.len(),
            sd.only_in_b.len()
        ));
    }
    s
}

pub fn render_json(sd: &StateDiff) -> String {
    serde_json::to_string_pretty(sd).expect("상태 디프 직렬화")
}
