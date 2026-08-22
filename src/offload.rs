use serde_json::Value;
use std::path::Path;

/// 큰 결과를 파일로 빼고 요약을 반환한다(context 위생). 도구가 output_path를 받았을 때 핸들러가 호출.
pub fn offload_result(value: &Value, path: &Path) -> Result<Value, String> {
    let json = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    crate::path_safety::atomic_write_file(path, json.as_bytes()).map_err(|e| e.to_string())?;
    let mut summary = serde_json::json!({
        "output_path": path.display().to_string(),
        "bytes": json.len(),
        "offloaded": true,
    });
    // value 자체가 배열이거나, object의 첫 배열 필드를 미리보기(count + head=첫3)로.
    let arr = value.as_array().or_else(|| {
        value
            .as_object()
            .and_then(|o| o.values().find_map(Value::as_array))
    });
    if let Some(a) = arr {
        if let Some(obj) = summary.as_object_mut() {
            obj.insert("count".into(), serde_json::json!(a.len()));
            obj.insert(
                "head".into(),
                serde_json::json!(a.iter().take(3).collect::<Vec<_>>()),
            );
        }
    }
    Ok(summary)
}

#[cfg(test)]
#[path = "offload_tests.rs"]
mod tests;
