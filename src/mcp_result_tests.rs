use serde_json::json;

use super::*;

#[test]
fn json_results_keep_text_compatibility_and_add_structured_content() {
    let value = json!({"ok": true, "count": 3});
    let result = json_result(value.clone());

    assert_eq!(result.structured_content, Some(value.clone()));
    assert_eq!(result.content.len(), 1);
    let serialized = value.to_string();
    assert_eq!(
        result.content[0].as_text().map(|text| text.text.as_str()),
        Some(serialized.as_str())
    );
    assert_eq!(result.is_error, Some(false));
}

#[test]
fn link_errors_preserve_the_adapter_error_code() {
    let result = link_error_result(LinkError::Emulator {
        kind: "bad_params".into(),
        message: "invalid range".into(),
    });

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.as_ref().unwrap()["error"]["code"],
        "bad_params"
    );
}

#[test]
fn explicit_false_outcomes_are_tool_errors_without_losing_diagnostics() {
    let diagnostics = json!({
        "launched": false,
        "reason": "binary missing",
        "next_action": "build the adapter"
    });
    let result = boolean_outcome_result(diagnostics.clone(), "launched");

    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.structured_content, Some(diagnostics));
}

#[test]
fn explicit_true_outcomes_remain_successful() {
    let result = boolean_outcome_result(json!({"stopped": true}), "stopped");
    assert_eq!(result.is_error, Some(false));
}

#[test]
fn images_publish_machine_readable_metadata_after_the_image_block() {
    let result = tool_output_result(ToolOutput::Image {
        png_base64: "QUJD".into(),
        saved_path: Some("/tmp/shot.png".into()),
        provenance: json!({"sha256": "abc", "byte_len": 3}),
    });

    assert!(result
        .content
        .first()
        .and_then(|block| block.as_image())
        .is_some());
    let metadata = result.structured_content.unwrap();
    assert_eq!(metadata["kind"], "image");
    assert_eq!(metadata["saved_path"], "/tmp/shot.png");
    assert_eq!(metadata["provenance"]["sha256"], "abc");
}
