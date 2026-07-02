use serde_json::Value;
use std::path::Path;

/// 큰 결과를 파일로 빼고 요약을 반환한다(context 위생). 도구가 output_path를 받았을 때 핸들러가 호출.
pub fn offload_result(value: &Value, path: &Path) -> Result<Value, String> {
    let json = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, &json).map_err(|e| e.to_string())?;
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
mod tests {
    use super::*;

    #[test]
    fn offload_writes_file_and_returns_summary() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("out.json");
        let v = serde_json::json!({"rows": [{"a":1},{"a":2},{"a":3},{"a":4}]});
        let s = offload_result(&v, &p).unwrap();
        assert_eq!(s["output_path"], serde_json::json!(p.display().to_string()));
        assert!(s["bytes"].as_u64().unwrap() > 0);
        assert_eq!(s["count"], serde_json::json!(4)); // 첫 배열 필드 길이
        assert_eq!(s["head"].as_array().unwrap().len(), 3); // head=첫 3
                                                            // 파일에 전체가 보존
        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn offload_top_level_array() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("arr.json");
        let v = serde_json::json!([1, 2, 3, 4, 5]);
        let s = offload_result(&v, &p).unwrap();
        assert_eq!(s["count"], serde_json::json!(5));
        assert_eq!(s["head"].as_array().unwrap().len(), 3);
        assert!(s["offloaded"].as_bool().unwrap());
    }

    #[test]
    fn offload_creates_parent_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("a/b/c/out.json");
        let v = serde_json::json!({"x": 1});
        let s = offload_result(&v, &p).unwrap();
        assert!(p.exists());
        assert_eq!(s["output_path"], serde_json::json!(p.display().to_string()));
    }

    #[test]
    fn offload_no_array_no_count() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("scalar.json");
        let v = serde_json::json!({"x": 42, "y": "hello"});
        let s = offload_result(&v, &p).unwrap();
        assert!(s.get("count").is_none());
        assert!(s.get("head").is_none());
        assert!(s["bytes"].as_u64().unwrap() > 0);
    }
}
