use super::bisect::*;
use crate::live::link::FakeLink;
use serde_json::json;

// ── 순수 이분 ─────────────────────────────────────────────
fn flips_at(k: u64) -> impl FnMut(u64) -> Result<bool, ()> {
    move |f| Ok(f >= k)
}

#[test]
fn finds_boundary() {
    let r = bisect(0, 1000, flips_at(617)).unwrap();
    assert_eq!(r.first_bad, Some(617));
}

#[test]
fn boundary_at_lo_plus_one() {
    let r = bisect(0, 16, flips_at(1)).unwrap();
    assert_eq!(r.first_bad, Some(1));
}

#[test]
fn lo_already_bad() {
    let r = bisect(10, 100, flips_at(0)).unwrap();
    assert_eq!(r.first_bad, Some(10));
    assert_eq!(r.probes.len(), 1, "lo가 bad면 즉시 종료");
}

#[test]
fn all_good_no_boundary() {
    let r = bisect(0, 100, flips_at(1000)).unwrap();
    assert_eq!(r.first_bad, None);
}

#[test]
fn probe_count_is_logarithmic() {
    let r = bisect(0, 1024, flips_at(500)).unwrap();
    // 2(lo,hi) + ~log2(1024)=10 안팎
    assert!(
        r.probes.len() <= 14,
        "프로브 {} 개 — 로그 규모",
        r.probes.len()
    );
}

#[test]
fn propagates_probe_error() {
    let r: Result<BisectResult, &str> = bisect(0, 10, |_| Err("boom"));
    assert_eq!(r, Err("boom"));
}

// ── 술어 eval ─────────────────────────────────────────────
fn pred(op: CmpOp, value: u64, len: u64) -> Predicate {
    Predicate {
        memory_type: "snesWorkRam".into(),
        address: 0,
        length: len,
        op,
        value,
    }
}

#[test]
fn eval_le_integer() {
    // [0x34, 0x12] = 0x1234 (LE)
    assert!(pred(CmpOp::Eq, 0x1234, 2).eval(&[0x34, 0x12]));
    assert!(!pred(CmpOp::Eq, 0x1234, 2).eval(&[0x12, 0x34]));
}

#[test]
fn eval_each_op() {
    assert!(pred(CmpOp::Ne, 0, 1).eval(&[5]));
    assert!(pred(CmpOp::Lt, 10, 1).eval(&[5]));
    assert!(pred(CmpOp::Gt, 3, 1).eval(&[5]));
    assert!(pred(CmpOp::Ge, 5, 1).eval(&[5]));
    assert!(pred(CmpOp::Le, 5, 1).eval(&[5]));
    assert!(!pred(CmpOp::Eq, 5, 1).eval(&[6]));
}

#[test]
fn parse_op() {
    assert_eq!(CmpOp::parse("ne").unwrap(), CmpOp::Ne);
    assert!(CmpOp::parse("xx").is_err());
}

// ── 라이브 프로브(FakeLink 배선) ──────────────────────────
#[test]
fn probe_state_evaluates_read_through_link() {
    // 원자적 probe가 hex "00"을 돌려주면, "== 0" 술어는 bad(참).
    let mut link = FakeLink::ok(json!({ "hex": "00" }));
    let p = pred(CmpOp::Eq, 0, 1);
    assert!(probe_state(&mut link, "/tmp/base.mss", 120, &p).unwrap());
    // 단일 원자 명령 "probe"로 호출하는지(결정론 배선 확인)
    assert_eq!(link.last_method.as_deref(), Some("probe"));
}

#[test]
fn probe_state_good_when_predicate_false() {
    let mut link = FakeLink::ok(json!({ "hex": "ff" }));
    let p = pred(CmpOp::Eq, 0, 1);
    assert!(!probe_state(&mut link, "/tmp/base.mss", 0, &p).unwrap());
}

// ── 길이·디코드 검증 ──────────────────────────────────────
#[test]
fn run_bisect_rejects_out_of_range_length() {
    use crate::live::link::LinkError;
    let mut link = FakeLink::ok(json!({ "hex": "00" }));
    for bad_len in [0u64, 9, 16] {
        let p = pred(CmpOp::Eq, 0, bad_len);
        let r = run_bisect(&mut link, "/tmp/base.mss", 0, 100, &p);
        assert!(
            matches!(r, Err(LinkError::Protocol(_))),
            "length {bad_len}는 1~8 밖이라 Protocol 에러여야: {r:?}"
        );
    }
}

#[test]
fn probe_bytes_rejects_length_mismatch() {
    // length=4를 요청했는데 probe가 1바이트만 반환 → 묵시 제로패딩 대신 에러여야 한다.
    let mut link = FakeLink::ok(json!({ "hex": "00" }));
    let p = pred(CmpOp::Eq, 0, 4);
    let r = probe_bytes(&mut link, "/tmp/base.mss", 0, &p);
    assert!(
        r.is_err(),
        "반환 바이트 수가 length와 다르면 에러여야: {r:?}"
    );
}

#[test]
fn hex_to_bytes_rejects_non_ascii_without_panic() {
    // 멀티바이트 UTF-8은 바이트 슬라이싱이 char boundary를 가를 수 있어 — 패닉 없이 에러를 반환하는지 검증.
    assert!(
        hex_to_bytes("aÀb").is_err(),
        "non-ASCII hex는 패닉 없이 Err여야"
    );
}
