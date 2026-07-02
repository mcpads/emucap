//! 숫자 문자열 파서 — MCP 도구 인자와 CLI(regression 등)가 *같은* 규칙으로 주소·길이·값을
//! 해석하도록 한 곳에 둔다. 10진, `0x`/`0X`/`$` 16진, `_` 자릿수 구분, 선행 `+`, 그리고 일부
//! 클라이언트의 따옴표 이중인코딩을 받아들인다. (정본은 여기 하나다.)

/// 10진 또는 `0x`/`$` 16진 문자열을 u64로 파싱한다.
pub fn parse_num_str(s: &str) -> Result<u64, String> {
    let t = s.trim();
    // 방어: 일부 MCP 클라이언트가 hex 값을 따옴표째 이중인코딩해 보낸다(예: 값이 "\"$80BC95\""로
    // 도착 → 양끝에 리터럴 큰따옴표). 정상 숫자열엔 따옴표가 없으므로, 양끝 짝 따옴표 한 겹을 벗긴다.
    let t = match t
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| t.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
    {
        Some(inner) => inner.trim(),
        None => t,
    };
    let t = t.strip_prefix('+').unwrap_or(t);
    let (radix, digits) = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (16, h)
    } else if let Some(h) = t.strip_prefix('$') {
        (16, h)
    } else {
        (10, t)
    };
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return Err(format!("빈 숫자: {s:?}"));
    }
    u64::from_str_radix(&digits, radix)
        .map_err(|e| format!("숫자 파싱 실패 {s:?} (10진 또는 0x/$ 16진): {e}"))
}

#[cfg(test)]
mod tests {
    use super::parse_num_str;

    #[test]
    fn decimal_and_hex_forms() {
        assert_eq!(parse_num_str("8471").unwrap(), 8471);
        assert_eq!(parse_num_str("0x2117").unwrap(), 0x2117);
        assert_eq!(parse_num_str("0X2117").unwrap(), 0x2117);
        assert_eq!(parse_num_str("$2117").unwrap(), 0x2117);
        assert_eq!(parse_num_str("0x80_420b").unwrap(), 0x0080_420b);
        assert_eq!(parse_num_str(" 0x420B ").unwrap(), 0x420b);
        assert_eq!(parse_num_str("+16").unwrap(), 16);
        // 따옴표 이중인코딩
        assert_eq!(parse_num_str("\"$80BC95\"").unwrap(), 0x80_BC95);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_num_str("zzz").is_err());
        assert!(parse_num_str("0x").is_err());
        assert!(parse_num_str("").is_err());
    }
}
