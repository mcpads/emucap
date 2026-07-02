use super::regression::*;
use crate::analysis::bisect::{CmpOp, Predicate};

fn pred(len: u64, op: CmpOp, value: u64) -> Predicate {
    Predicate {
        memory_type: "wram".into(),
        address: 0,
        length: len,
        op,
        value,
    }
}

fn res(id: &str, v: Verdict) -> CaseResult {
    CaseResult {
        id: id.into(),
        verdict: v,
    }
}

#[test]
fn bucket_classifies_signal_vs_invalid() {
    assert_eq!(Verdict::Pass.bucket(), Bucket::Passed);
    assert_eq!(Verdict::Fail.bucket(), Bucket::Failed);
    assert_eq!(Verdict::InvalidRead.bucket(), Bucket::Invalid);
    assert_eq!(Verdict::Unsupported.bucket(), Bucket::Invalid);
}

#[test]
fn evaluate_absent_passes_when_bug_gone() {
    // eval(bad)=false(값 1 != 9) → absent면 PASS
    let v = evaluate(&[1, 0], &pred(2, CmpOp::Eq, 9), Expect::Absent);
    assert_eq!(v, Verdict::Pass);
}

#[test]
fn evaluate_absent_fails_when_bug_present() {
    // eval(bad)=true(값 9 == 9) → absent면 FAIL
    let v = evaluate(&[9, 0], &pred(2, CmpOp::Eq, 9), Expect::Absent);
    assert_eq!(v, Verdict::Fail);
}

#[test]
fn evaluate_present_passes_when_still_reproduces() {
    let v = evaluate(&[9, 0], &pred(2, CmpOp::Eq, 9), Expect::Present);
    assert_eq!(v, Verdict::Pass);
}

#[test]
fn evaluate_present_fails_when_bug_gone() {
    // eval(bad)=false(값 1 != 9) → present면 FAIL(상태가 바뀜)
    let v = evaluate(&[1, 0], &pred(2, CmpOp::Eq, 9), Expect::Present);
    assert_eq!(v, Verdict::Fail);
}

#[test]
fn evaluate_short_read_is_invalid_read() {
    // length=2인데 1바이트만 → 조용한 패딩 금지
    let v = evaluate(&[9], &pred(2, CmpOp::Eq, 9), Expect::Absent);
    assert_eq!(v, Verdict::InvalidRead);
}

#[test]
fn parse_movie_reads_frame_button_sets() {
    let m = parse_movie("10:left,a\n12:\n15:right").unwrap();
    assert_eq!(m.frames.len(), 3);
    assert_eq!(m.frames[0].frame, 10);
    assert_eq!(
        m.frames[0].buttons,
        vec!["left".to_string(), "a".to_string()]
    );
    assert_eq!(m.frames[1].buttons, Vec::<String>::new()); // 빈 집합(입력 없음)
    assert_eq!(m.frames[2].frame, 15);
}

#[test]
fn parse_movie_skips_blank_lines_and_sorts() {
    let m = parse_movie("\n15:a\n\n10:b\n").unwrap();
    assert_eq!(m.frames[0].frame, 10); // 프레임 순 정렬
    assert_eq!(m.frames[1].frame, 15);
}

#[test]
fn parse_movie_rejects_bad_frame() {
    assert!(parse_movie("xx:a").is_err());
}

#[test]
fn case_roundtrips_through_disk() {
    let tmp = std::env::temp_dir().join(format!("regr_rt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let case = Case {
        format_version: CASE_FORMAT_VERSION,
        id: "abc".into(),
        description: "데모".into(),
        rom: RomRef {
            sha1: "deadbeef".into(),
            path_hint: "game.sfc".into(),
        },
        repro: Repro::Savestate {
            state_sha1: "ff".into(),
            advance_frames: 60,
        },
        predicate: pred(2, CmpOp::Eq, 9),
        expect: Expect::Absent,
    };
    save_case(&tmp, &case).unwrap();
    let back = load_case(&tmp).unwrap();
    assert_eq!(back, case);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn summary_buckets_and_exit_rule() {
    let s = Summary::from_results(vec![
        res("a", Verdict::Pass),
        res("b", Verdict::Pass),
        res("c", Verdict::Fail),
        res("d", Verdict::MissingPayload),
    ]);
    assert_eq!((s.passed, s.failed, s.invalid), (2, 1, 1));
    assert!(!s.ok()); // 실패·무효 있으면 실패
}

#[test]
fn summary_empty_suite_is_not_ok() {
    let s = Summary::from_results(vec![]);
    assert!(!s.ok()); // 검증 0건은 통과 아님
}

#[test]
fn summary_all_pass_is_ok() {
    let s = Summary::from_results(vec![res("a", Verdict::Pass)]);
    assert!(s.ok());
}
