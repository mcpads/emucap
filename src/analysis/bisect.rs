use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::live::link::{EmulatorLink, LinkError};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Ge,
    Le,
}

impl CmpOp {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "eq" => Self::Eq,
            "ne" => Self::Ne,
            "lt" => Self::Lt,
            "gt" => Self::Gt,
            "ge" => Self::Ge,
            "le" => Self::Le,
            _ => return Err(format!("알 수 없는 op: {s} (eq|ne|lt|gt|ge|le)")),
        })
    }

    fn apply(self, a: u64, b: u64) -> bool {
        match self {
            Self::Eq => a == b,
            Self::Ne => a != b,
            Self::Lt => a < b,
            Self::Gt => a > b,
            Self::Ge => a >= b,
            Self::Le => a <= b,
        }
    }
}

/// 타깃 메모리가 "나쁜" 상태인지 판정하는 술어. 읽은 바이트를 LE 정수로 비교.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Predicate {
    pub memory_type: String,
    pub address: u64,
    pub length: u64,
    pub op: CmpOp,
    pub value: u64,
}

impl Predicate {
    /// 읽은 바이트(리틀엔디언 정수)가 술어를 만족하면 bad(참).
    pub fn eval(&self, bytes: &[u8]) -> bool {
        let mut v: u64 = 0;
        for (i, b) in bytes.iter().take(8).enumerate() {
            v |= (*b as u64) << (8 * i);
        }
        self.op.apply(v, self.value)
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Probe {
    pub frame: u64,
    pub bad: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct BisectResult {
    pub first_bad: Option<u64>,
    pub probes: Vec<Probe>,
}

/// lo(good)~hi(bad)에서 false→true 경계를 이분으로 찾는다.
/// `probe(F)` = 그 프레임이 bad인지. 단조 가정(F<K good, F≥K bad); 비단조면 한 경계를 찾음.
pub fn bisect<F, E>(lo: u64, hi: u64, mut probe: F) -> Result<BisectResult, E>
where
    F: FnMut(u64) -> Result<bool, E>,
{
    let mut probes = Vec::new();

    let lo_bad = probe(lo)?;
    probes.push(Probe {
        frame: lo,
        bad: lo_bad,
    });
    if lo_bad {
        // lo부터 이미 bad.
        return Ok(BisectResult {
            first_bad: Some(lo),
            probes,
        });
    }
    if hi <= lo {
        return Ok(BisectResult {
            first_bad: None,
            probes,
        });
    }

    let hi_bad = probe(hi)?;
    probes.push(Probe {
        frame: hi,
        bad: hi_bad,
    });
    if !hi_bad {
        // 구간 끝까지 good — 경계 없음.
        return Ok(BisectResult {
            first_bad: None,
            probes,
        });
    }

    // 불변식: probe(lo)=good, probe(hi)=bad.
    let mut lo = lo;
    let mut hi = hi;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let bad = probe(mid)?;
        probes.push(Probe { frame: mid, bad });
        if bad {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(BisectResult {
        first_bad: Some(hi),
        probes,
    })
}

/// 베이스 세이브스테이트 복귀 → frame 진행 → 타깃 읽기(원자적 probe). 읽은 바이트 반환.
pub fn probe_bytes(
    link: &mut dyn EmulatorLink,
    base_state: &str,
    frame: u64,
    pred: &Predicate,
) -> Result<Vec<u8>, LinkError> {
    let res = link.call(
        "probe",
        json!({
            "state": base_state, "frame": frame,
            "memory_type": pred.memory_type, "address": pred.address, "length": pred.length,
        }),
    )?;
    let hex = res
        .get("hex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinkError::Protocol("probe 응답에 hex 없음".into()))?;
    let bytes =
        hex_to_bytes(hex).map_err(|e| LinkError::Protocol(format!("hex 디코드 실패: {e}")))?;
    // 요청한 length만큼 읽혔는지 확인한다 — 부족분을 묵시 제로패딩하면 술어 판정이 조용히
    // 틀린다(eval은 읽은 바이트만 LE로 모은다).
    if bytes.len() as u64 != pred.length {
        return Err(LinkError::Protocol(format!(
            "probe가 {} 바이트를 기대했는데 {} 바이트 반환",
            pred.length,
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// 한 프레임을 프로브한다: 원자적 `probe`(베이스 복귀 → F프레임 진행 → 타깃 읽기)로
/// hex를 받아 술어를 평가한다.
///
/// 결정론은 원자성에서 온다: load와 read 사이에 외부 명령(네트워크 왕복)이 끼지 않아야
/// 자유 실행 누수가 없다. 별도의 load_state+run_frames 호출은 그 사이가 비결정론적이라
/// 쓰지 않는다.
pub fn probe_state(
    link: &mut dyn EmulatorLink,
    base_state: &str,
    frame: u64,
    pred: &Predicate,
) -> Result<bool, LinkError> {
    let bytes = probe_bytes(link, base_state, frame, pred)?;
    Ok(pred.eval(&bytes))
}

/// 라이브 이분: 순수 이분에 라이브 프로브를 연결한다.
pub fn run_bisect(
    link: &mut dyn EmulatorLink,
    base_state: &str,
    lo: u64,
    hi: u64,
    pred: &Predicate,
) -> Result<BisectResult, LinkError> {
    // 술어 길이는 LE 정수로 비교 가능한 1~8바이트여야 한다. 회귀 경로와 동일한 계약을
    // bisect에도 적용해, 범위 밖 길이가 조용히 절단/제로패딩되어 무의미한 경계를 내지 않게 한다.
    if pred.length == 0 || pred.length > 8 {
        return Err(LinkError::Protocol(format!(
            "length는 1~8이어야: {}",
            pred.length
        )));
    }
    bisect(lo, hi, |f| probe_state(link, base_state, f, pred))
}

pub fn hex_to_bytes(s: &str) -> Result<Vec<u8>, String> {
    // ASCII 보장 — 멀티바이트 UTF-8이 끼면 아래 바이트 슬라이싱이 char boundary를 갈라
    // 패닉한다. hex는 본디 ASCII이므로 비-ASCII는 디코드 에러로 거부한다.
    if !s.is_ascii() {
        return Err("hex는 ASCII여야".into());
    }
    if !s.len().is_multiple_of(2) {
        return Err("홀수 길이 hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}
