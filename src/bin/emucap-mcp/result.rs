use emucap::live::link::LinkError;
use emucap::live::tools::ToolOutput;
use rmcp::model::{CallToolResult, Content};

/// 분석 verb의 문자열 에러를 CallToolResult로(verify_determinism의 인자 검증용).
pub(crate) fn track_err(msg: impl std::fmt::Display) -> CallToolResult {
    let mut r = CallToolResult::success(vec![Content::text(format!("{msg}"))]);
    r.is_error = Some(true);
    r
}

pub(crate) fn err_result(e: LinkError) -> CallToolResult {
    let mut r = CallToolResult::success(vec![Content::text(format!("{e}"))]);
    r.is_error = Some(true);
    r
}

pub(crate) fn output_result(out: ToolOutput) -> CallToolResult {
    match out {
        ToolOutput::Json(v) => CallToolResult::success(vec![Content::text(v.to_string())]),
        ToolOutput::Image {
            png_base64,
            saved_path,
            provenance,
        } => {
            let mut content = vec![Content::image(png_base64, "image/png")];
            if let Some(p) = saved_path {
                content.push(Content::text(format!("saved: {p}")));
            }
            content.push(Content::text(format!("provenance: {provenance}")));
            CallToolResult::success(content)
        }
    }
}
