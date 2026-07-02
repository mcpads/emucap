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
