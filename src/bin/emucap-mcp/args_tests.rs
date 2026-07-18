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
fn write_memory_accepts_inline_or_file_source_shapes() {
    let inline: WriteMemoryArgs =
        serde_json::from_str(r#"{"memory_type":"ram","address":"0x10","hex":"deadbeef"}"#).unwrap();
    assert_eq!(inline.hex.as_deref(), Some("deadbeef"));
    assert!(inline.input_file.is_none());

    let file: WriteMemoryArgs = serde_json::from_str(
        r#"{"memory_type":"ram","address":16,"input_file":{"path":"/path/to/payload.bin","offset":"0x20","length":"0x40","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
    )
    .unwrap();
    assert!(file.hex.is_none());
    let input = file.input_file.unwrap();
    assert_eq!(input.offset.map(Num::get), Some(0x20));
    assert_eq!(input.length.get(), 0x40);
}

#[test]
fn write_memory_schema_exposes_both_input_sources() {
    let schema = serde_json::to_string(&schemars::schema_for!(WriteMemoryArgs)).unwrap();
    for field in ["hex", "input_file", "path", "offset", "length", "sha256"] {
        assert!(
            schema.contains(&format!("\"{field}\"")),
            "write_memory schema must expose {field}: {schema}"
        );
    }
}

#[test]
fn frame_args_reject_over_cap() {
    // 상한 초과는 deserialize 단계에서 거부(무한 deferred 루프·raw_call wedge 방지, H2).
    let over = MAX_SYNC_ADVANCE_COUNT + 1;
    assert!(
        serde_json::from_str::<RunFramesArgs>(&format!(r#"{{"n":{over}}}"#)).is_err(),
        "run_frames n 상한 초과는 거부해야"
    );
    assert!(
        serde_json::from_str::<StepArgs>(&format!(r#"{{"frames":{over}}}"#)).is_err(),
        "step frames 상한 초과는 거부해야"
    );
    assert!(
        serde_json::from_str::<StepArgs>(&format!(r#"{{"count":{over}}}"#)).is_err(),
        "step count 상한 초과는 거부해야"
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
}

#[test]
fn frame_args_accept_at_cap_and_defaults() {
    // 상한 이내는 통과, 필드 부재 시 기본값(상한 이내)도 통과 — clamp가 정상 사용을 깨지 않아야.
    let r: RunFramesArgs =
        serde_json::from_str(&format!(r#"{{"n":{MAX_SYNC_ADVANCE_COUNT}}}"#)).unwrap();
    assert_eq!(r.n, MAX_SYNC_ADVANCE_COUNT);
    let s: StepArgs = serde_json::from_str("{}").unwrap();
    assert_eq!(s.count, 1, "step count 기본값");
    assert_eq!(s.unit, StepUnit::Frames, "step unit 기본값");
    let si: StepArgs =
        serde_json::from_str(r#"{"count":2,"unit":"instructions","cpu":"arm7"}"#).unwrap();
    assert_eq!(si.count, 2);
    assert_eq!(si.unit, StepUnit::Instructions);
    assert_eq!(si.cpu.as_deref(), Some("arm7"));
    let h: HoldUntilArgs =
        serde_json::from_str(r#"{"buttons":["a"],"memory_type":"x","address":0,"length":1}"#)
            .unwrap();
    assert_eq!(h.max_frames, 300, "hold_until max_frames 기본값");
}

#[test]
fn input_hold_frame_cap_matches_sync_advance_cap() {
    // 합성 입력이 내부 step에서 뒤늦게 거부되지 않도록 입력 hold와 공통 advance가 같은 상한을 쓴다.
    assert_eq!(MAX_INPUT_HOLD_FRAMES, MAX_SYNC_ADVANCE_COUNT);
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
}

#[test]
fn watch_register_accepts_max_instructions() {
    let w: WatchRegisterArgs =
        serde_json::from_str(r#"{"register":"sp","max_instructions":5000000}"#).unwrap();
    assert_eq!(w.max_instructions, Some(5_000_000));
    let d: WatchRegisterArgs = serde_json::from_str("{}").unwrap();
    assert_eq!(d.max_instructions, None, "미지정 시 None(어댑터 기본 사용)");
}
