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
fn record_window_accepts_the_generic_negotiated_extension_shape() {
    let request: RecordWindowArgs = serde_json::from_str(
        r#"{"output_root":"/tmp/evidence","frames":120,"event_classes":["frame_boundary"],"limits":{"max_events":1000,"max_bytes":1048576,"max_host_ms":8000}}"#,
    )
    .unwrap();
    assert_eq!(request.output_root, "/tmp/evidence");
    assert_eq!(request.frames, 120);
    assert_eq!(request.warmup_frames, 0);
    assert_eq!(request.event_classes, ["frame_boundary"]);
    assert!(request.event_arming_overrides.is_empty());
    assert_eq!(request.limits.unwrap().max_host_ms, Some(8000));
    let extended: RecordWindowArgs = serde_json::from_str(
        r#"{"output_root":"/tmp/evidence","frames":1,"warmup_frames":3,"origin":"reset_release","input_path":"/tmp/movie.txt","event_classes":["frame_boundary","frame_completed"],"stop_on":{"event_class":"frame_completed","occurrence":1}}"#,
    )
    .unwrap();
    assert!(matches!(
        extended.origin,
        Some(RecordWindowOriginArgs::ResetRelease)
    ));
    assert_eq!(extended.input_path.as_deref(), Some("/tmp/movie.txt"));
    assert_eq!(extended.warmup_frames, 3);
    assert_eq!(extended.stop_on.unwrap().occurrence, 1);
    let scoped: RecordWindowArgs = serde_json::from_str(
        r#"{"output_root":"/tmp/evidence","frames":1,"warmup_frames":3,"event_classes":["frame_boundary"],"event_arming_overrides":[{"event_class":"frame_boundary","scope":"observation"}]}"#,
    )
    .unwrap();
    assert!(matches!(
        scoped.event_arming_overrides[0].scope,
        RecordWindowEventScopeArgs::Observation
    ));
    let filtered: RecordWindowArgs = serde_json::from_str(
        r#"{"output_root":"/tmp/evidence","frames":1,"event_classes":["frame_boundary","snes_ppu_obj_consumption_read"],"event_filters":[{"event_class":"snes_ppu_obj_consumption_read","terms":[{"kind":"u64_range","path":"address","start":"0x2000","length":256}]}]}"#,
    )
    .unwrap();
    assert_eq!(filtered.event_filters.len(), 1);
    match &filtered.event_filters[0].terms[0] {
        RecordWindowFilterTermArgs::U64Range {
            path,
            start,
            length,
        } => {
            assert_eq!(path, "address");
            assert_eq!(start.0, 0x2000);
            assert_eq!(length.0, 256);
        }
    }
    let snapshots: RecordWindowArgs = serde_json::from_str(
        r#"{"output_root":"/tmp/evidence","frames":1,"terminal_snapshots":[{"label":"terminal-wram","memory_type":"wram","address":"0x20","length":16}]}"#,
    )
    .unwrap();
    assert_eq!(snapshots.terminal_snapshots.len(), 1);
    let snapshot = &snapshots.terminal_snapshots[0];
    assert_eq!(snapshot.label, "terminal-wram");
    assert_eq!(snapshot.memory_type, "wram");
    assert_eq!(snapshot.address.0, 0x20);
    assert_eq!(snapshot.length.0, 16);
    let terminal_state: RecordWindowArgs = serde_json::from_str(
        r#"{"output_root":"/tmp/evidence","frames":1,"terminal_state_profile":"snes_ppu"}"#,
    )
    .unwrap();
    assert_eq!(
        terminal_state.terminal_state_profile.as_deref(),
        Some("snes_ppu")
    );
    assert!(serde_json::from_str::<RecordWindowArgs>(
        r#"{"output_root":"/tmp/evidence","frames":1,"origin":"reset"}"#
    )
    .is_err());
}

#[test]
fn launch_and_recording_parse_explicit_repeatability_selection() {
    let launch: LaunchArgs = serde_json::from_str(
        r#"{"content_path":"/tmp/game.sfc","system":"snes","execution_profile":"repeatable"}"#,
    )
    .unwrap();
    assert_eq!(
        launch.execution_profile,
        Some(LaunchExecutionProfileArgs::Repeatable)
    );
    assert!(
        !launch.start_frozen,
        "the launcher applies the profile implication"
    );

    let recording: RecordWindowArgs = serde_json::from_str(
        r#"{"output_root":"/tmp/evidence","frames":1,"origin":"reset_release","require_repeatable":true}"#,
    )
    .unwrap();
    assert!(recording.require_repeatable);
}

#[test]
fn launch_surfaces_accept_the_exact_indirect_media_approval_shape() {
    let json = r#"{
        "content_path":"/tmp/disc.cue",
        "system":"saturn",
        "indirect_media_approval":{
            "entry_binding":"sha256:abc",
            "adapter":"mednafen",
            "members":["disc.sbi","track.bin"]
        }
    }"#;
    let plan: LaunchPlanArgs = serde_json::from_str(json).unwrap();
    let launch: LaunchArgs = serde_json::from_str(json).unwrap();
    assert_eq!(
        plan.indirect_media_approval.as_ref().unwrap(),
        launch.indirect_media_approval.as_ref().unwrap()
    );
    assert_eq!(
        launch.indirect_media_approval.unwrap().members,
        ["disc.sbi", "track.bin"]
    );
}

#[test]
fn launch_parses_only_supported_pc98_sound_boards() {
    let launch: LaunchArgs = serde_json::from_str(
        r#"{"content_path":"/tmp/game.hdi","system":"pc98","sound":true,"pc98_sound_board":"pc9801_86"}"#,
    )
    .unwrap();
    assert_eq!(launch.pc98_sound_board, Some(Pc98SoundBoardArgs::Pc9801_86));
    assert_eq!(launch.sound, Some(true));

    assert!(serde_json::from_str::<LaunchArgs>(
        r#"{"content_path":"/tmp/game.hdi","system":"pc98","pc98_sound_board":"pc9801_118"}"#,
    )
    .is_err());
}

#[test]
fn frame_args_reject_over_cap() {
    // 상한 초과는 deserialize 단계에서 거부(무한 deferred 루프·raw_call wedge 방지, H2).
    let over = MAX_SYNC_ADVANCE_COUNT + 1;
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
