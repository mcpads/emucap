//! 숫자 문자열 파서 — MCP 도구 인자와 CLI(regression 등)가 *같은* 규칙으로 주소·길이·값을
//! 해석하도록 한 곳에 둔다. 10진, `0x`/`0X`/`$` 16진, `_` 자릿수 구분, 선행 `+`, 그리고 일부
//! 클라이언트의 따옴표 이중인코딩을 받아들인다. 숫자 문자열 해석은 이 모듈에서만 수행한다.

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
        return Err(format!("empty numeric value: {s:?}"));
    }
    u64::from_str_radix(&digits, radix)
        .map_err(|e| format!("failed to parse {s:?} as decimal or 0x/$ hexadecimal: {e}"))
}

#[cfg(test)]
#[path = "numparse_tests.rs"]
mod tests;
