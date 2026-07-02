use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// 한 리전의 기준 주소 + 바이트.
#[derive(Debug, Clone)]
pub struct RegionData {
    pub base_address: u64,
    pub bytes: Vec<u8>,
}

/// 이름 → 리전. 디프의 입력.
#[derive(Debug, Clone, Default)]
pub struct RegionSet {
    pub regions: BTreeMap<String, RegionData>,
}

impl RegionSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: &str, base_address: u64, bytes: Vec<u8>) {
        self.regions.insert(
            name.to_string(),
            RegionData {
                base_address,
                bytes,
            },
        );
    }
}

/// 비교에서 제외할 리전의 오프셋 범위 [start, end).
#[derive(Debug, Clone, PartialEq)]
pub struct IgnoreSpec {
    pub region: String,
    pub start: u64,
    pub end: u64,
}

/// "region:start-end"를 파싱한다. 예: "wram:256-512".
pub fn parse_ignore(s: &str) -> Result<IgnoreSpec, String> {
    let (region, range) = s
        .split_once(':')
        .ok_or_else(|| format!("형식 오류(region:start-end): {s}"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| format!("형식 오류(start-end): {range}"))?;
    let start: u64 = start
        .parse()
        .map_err(|_| format!("start 정수 아님: {start}"))?;
    let end: u64 = end.parse().map_err(|_| format!("end 정수 아님: {end}"))?;
    if end < start {
        return Err(format!("end < start: {s}"));
    }
    Ok(IgnoreSpec {
        region: region.to_string(),
        start,
        end,
    })
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Divergence {
    pub offset: u64,
    pub address: u64,
    pub a: u8,
    pub b: u8,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct RegionDiff {
    pub name: String,
    pub compared: usize,
    pub differing: usize,
    pub first_divergence: Option<Divergence>,
    pub a_len: usize,
    pub b_len: usize,
    /// 비교 범위에서 다른 모든 오프셋(ignore·baseline 제외 후). 기준선 빼기의 입력.
    #[serde(default)]
    pub divergences: Vec<u64>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct DiffReport {
    pub regions: Vec<RegionDiff>,
    /// 한쪽에만 있는 리전 이름(비교 못 함).
    pub unmatched: Vec<String>,
}

/// 기준선(정상 정렬 지점의 원본↔패치본 diff): 리전별 "예상 차이" 오프셋 집합. 버그 지점 diff에서
/// 이 오프셋들을 제외하면 새로 생긴 차이만 남는다.
#[derive(Debug, Default)]
pub struct Baseline {
    by_region: HashMap<String, HashSet<u64>>,
}

impl Baseline {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 이전 diff 리포트의 분기 오프셋들을 기준선으로 삼는다.
    pub fn from_report(r: &DiffReport) -> Self {
        let mut by_region = HashMap::new();
        for rd in &r.regions {
            by_region.insert(rd.name.clone(), rd.divergences.iter().copied().collect());
        }
        Self { by_region }
    }

    fn excludes(&self, region: &str, offset: u64) -> bool {
        self.by_region
            .get(region)
            .is_some_and(|s| s.contains(&offset))
    }
}

fn is_ignored(ignore: &[IgnoreSpec], region: &str, offset: u64) -> bool {
    ignore
        .iter()
        .any(|i| i.region == region && offset >= i.start && offset < i.end)
}

/// 두 리전셋을 비교한다. 같은 이름의 리전끼리, 공통 길이까지, ignore 범위와 baseline
/// 오프셋은 제외.
pub fn diff(
    a: &RegionSet,
    b: &RegionSet,
    ignore: &[IgnoreSpec],
    baseline: &Baseline,
) -> DiffReport {
    let mut regions = Vec::new();
    let mut unmatched = Vec::new();

    for (name, ra) in &a.regions {
        let rb = match b.regions.get(name) {
            Some(rb) => rb,
            None => {
                unmatched.push(name.clone());
                continue;
            }
        };
        let common = ra.bytes.len().min(rb.bytes.len());
        let mut first = None;
        let mut divergences = Vec::new();
        for off in 0..common {
            let o = off as u64;
            if is_ignored(ignore, name, o) || baseline.excludes(name, o) {
                continue;
            }
            if ra.bytes[off] != rb.bytes[off] {
                divergences.push(o);
                if first.is_none() {
                    first = Some(Divergence {
                        offset: o,
                        address: ra.base_address.saturating_add(o),
                        a: ra.bytes[off],
                        b: rb.bytes[off],
                    });
                }
            }
        }
        regions.push(RegionDiff {
            name: name.clone(),
            compared: common,
            differing: divergences.len(),
            first_divergence: first,
            a_len: ra.bytes.len(),
            b_len: rb.bytes.len(),
            divergences,
        });
    }
    // b에만 있는 리전도 unmatched에
    for name in b.regions.keys() {
        if !a.regions.contains_key(name) {
            unmatched.push(name.clone());
        }
    }
    DiffReport { regions, unmatched }
}

pub fn render_json(report: &DiffReport) -> String {
    serde_json::to_string_pretty(report).expect("디프 직렬화")
}

pub fn render_table(report: &DiffReport) -> String {
    let mut s = String::new();
    s.push_str("리전        다른바이트/비교    최초 분기점(offset @addr: A→B)\n");
    for r in &report.regions {
        let first = match &r.first_divergence {
            Some(d) => format!(
                "0x{:x} @0x{:x}: {:02x}→{:02x}",
                d.offset, d.address, d.a, d.b
            ),
            None => "(차이 없음)".to_string(),
        };
        let size = if r.a_len != r.b_len {
            format!(" [크기 불일치 {}≠{}]", r.a_len, r.b_len)
        } else {
            String::new()
        };
        s.push_str(&format!(
            "{:<12}{}/{:<14}{}{}\n",
            r.name, r.differing, r.compared, first, size
        ));
    }
    if !report.unmatched.is_empty() {
        s.push_str(&format!("한쪽에만 있는 리전: {:?}\n", report.unmatched));
    }
    s.push_str("(최초 분기점만 의미 있다 — 이후 차이는 파생이다.)\n");
    s
}
