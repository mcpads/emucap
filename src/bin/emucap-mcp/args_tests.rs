use super::*;

#[test]
fn parse_variants() {
    assert_eq!(parse_num_str("8471").unwrap(), 8471);
    assert_eq!(parse_num_str("0x2117").unwrap(), 0x2117);
    assert_eq!(parse_num_str("0X2117").unwrap(), 0x2117);
    assert_eq!(parse_num_str("$2117").unwrap(), 0x2117);
    assert_eq!(parse_num_str("0x80_420b").unwrap(), 0x0080_420b);
    assert_eq!(parse_num_str(" 0x420B ").unwrap(), 0x420b);
    assert!(parse_num_str("zzz").is_err());
    assert!(parse_num_str("0x").is_err());
    // 따옴표째 이중인코딩 방어(회귀 가드): 양끝 리터럴 따옴표를 벗기고 파싱
    assert_eq!(parse_num_str("\"$80BC95\"").unwrap(), 0x80BC95);
    assert_eq!(parse_num_str("\"0x2117\"").unwrap(), 0x2117);
    assert_eq!(parse_num_str("'0x10'").unwrap(), 0x10);
    assert_eq!(parse_num_str("\" $420B \"").unwrap(), 0x420B); // 따옴표+공백 혼합
}

#[test]
fn deser_num_double_quoted_hex() {
    // MCP 클라이언트가 값을 따옴표째 이중인코딩한 케이스 재현(start 값이 JSON 문자열 "$80BC95")
    let d: BreakpointArgs = serde_json::from_str(
        r#"{"kind":"write","memory_type":"x","start":"\"$80BC95\"","end":"0"}"#,
    )
    .unwrap();
    assert_eq!(d.start.get(), 0x80BC95);
}

#[test]
fn deser_num_int_and_hex_string() {
    // 정수와 16진 문자열을 같은 필드에서 모두 수용
    let a: ReadMemoryArgs =
        serde_json::from_str(r#"{"memory_type":"snesMemory","address":"0x2117","length":16}"#)
            .unwrap();
    assert_eq!(a.address.get(), 0x2117);
    assert_eq!(a.length.get(), 16);

    let b: ReadMemoryArgs =
        serde_json::from_str(r#"{"memory_type":"x","address":8471,"length":"0x10"}"#).unwrap();
    assert_eq!(b.address.get(), 8471);
    assert_eq!(b.length.get(), 16);

    // $ 접두 + Option<Num> 값조건
    let c: BreakpointArgs = serde_json::from_str(
        r#"{"kind":"write","memory_type":"x","start":"$802117","end":"0x802117","value":"0x60"}"#,
    )
    .unwrap();
    assert_eq!(c.start.get(), 0x0080_2117);
    assert_eq!(c.end.get(), 0x0080_2117);
    assert_eq!(c.value.map(Num::get), Some(0x60));
    assert_eq!(c.value_mask.map(Num::get), None);
}

#[test]
fn frame_args_reject_over_cap() {
    // 상한 초과는 deserialize 단계에서 거부(무한 deferred 루프·raw_call wedge 방지, H2).
    let over = MAX_FRAME_ARG + 1;
    assert!(
        serde_json::from_str::<RunFramesArgs>(&format!(r#"{{"n":{over}}}"#)).is_err(),
        "run_frames n 상한 초과는 거부해야"
    );
    assert!(
        serde_json::from_str::<StepArgs>(&format!(r#"{{"frames":{over}}}"#)).is_err(),
        "step frames 상한 초과는 거부해야"
    );
    assert!(
        serde_json::from_str::<StepInstructionsArgs>(&format!(r#"{{"count":{over}}}"#)).is_err(),
        "step_instructions count 상한 초과는 거부해야"
    );
    assert!(
        serde_json::from_str::<HoldUntilArgs>(&format!(
            r#"{{"buttons":["a"],"memory_type":"x","address":0,"length":1,"max_frames":{over}}}"#
        ))
        .is_err(),
        "hold_until max_frames 상한 초과는 거부해야"
    );
    assert!(
        serde_json::from_str::<ProbeArgs>(&format!(
            r#"{{"state":"s","frame":{over},"memory_type":"x","address":0,"length":1}}"#
        ))
        .is_err(),
        "probe frame 상한 초과는 거부해야(deferred 프로브가 링크를 붙잡음)"
    );
    assert!(
        serde_json::from_str::<BisectArgs>(&format!(
            r#"{{"state":"s","lo":0,"hi":{over},"memory_type":"x","address":0,"op":"eq","value":0}}"#
        ))
        .is_err(),
        "bisect hi 상한 초과는 거부해야"
    );
}

#[test]
fn frame_args_accept_at_cap_and_defaults() {
    // 상한 이내는 통과, 필드 부재 시 기본값(상한 이내)도 통과 — clamp가 정상 사용을 깨지 않아야.
    let r: RunFramesArgs = serde_json::from_str(&format!(r#"{{"n":{MAX_FRAME_ARG}}}"#)).unwrap();
    assert_eq!(r.n, MAX_FRAME_ARG);
    let s: StepArgs = serde_json::from_str("{}").unwrap();
    assert_eq!(s.frames, 1, "step frames 기본값");
    let si: StepInstructionsArgs = serde_json::from_str("{}").unwrap();
    assert_eq!(si.count, 1, "step_instructions count 기본값");
    let h: HoldUntilArgs =
        serde_json::from_str(r#"{"buttons":["a"],"memory_type":"x","address":0,"length":1}"#)
            .unwrap();
    assert_eq!(h.max_frames, 300, "hold_until max_frames 기본값");
}

#[test]
fn input_hold_frame_cap_is_tighter_than_run_frames() {
    // 입력 hold 프레임(press/tap)은 run_frames보다 작은 상한 — 링크 deadline 안에 들어야 MCP 포기 후
    // 버튼이 눌린 채 안 남는다. MAX_INPUT_HOLD_FRAMES+1은 press/tap 거부, run_frames 통과.
    let over_input = MAX_INPUT_HOLD_FRAMES + 1;
    assert!(
        serde_json::from_str::<PressArgs>(&format!(r#"{{"buttons":["a"],"frames":{over_input}}}"#))
            .is_err(),
        "press_buttons frames는 입력 상한 초과를 거부해야"
    );
    assert!(
        serde_json::from_str::<TapArgs>(&format!(
            r#"{{"buttons":["a"],"press_frames":{over_input}}}"#
        ))
        .is_err(),
        "tap press_frames는 입력 상한 초과를 거부해야"
    );
    assert!(
        serde_json::from_str::<RunFramesArgs>(&format!(r#"{{"n":{over_input}}}"#)).is_ok(),
        "run_frames(입력 없음)는 입력 상한보다 큰 값도 통과 — 입력 상한이 더 tight"
    );
}

#[test]
fn watch_register_accepts_max_instructions() {
    let w: WatchRegisterArgs =
        serde_json::from_str(r#"{"register":"sp","max_instructions":5000000}"#).unwrap();
    assert_eq!(w.max_instructions, Some(5_000_000));
    let d: WatchRegisterArgs = serde_json::from_str("{}").unwrap();
    assert_eq!(d.max_instructions, None, "미지정 시 None(어댑터 기본 사용)");
}
