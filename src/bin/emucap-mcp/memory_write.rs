use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use emucap::live::link::{Capabilities, LinkError};
use emucap::live::tools::{ToolOutput, MAX_WRITE_BYTES};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::args::{WriteMemoryArgs, WriteMemoryFileArgs};

pub(crate) const FILE_LOAD_TIMEOUT_MS: u64 = 2_000;

pub(crate) struct PreparedWrite {
    pub(crate) bytes: Vec<u8>,
    source_kind: &'static str,
    sha256: String,
}

pub(crate) fn generation_marker(capabilities: &Capabilities) -> Option<String> {
    capabilities
        .identity
        .launch_id
        .as_ref()
        .map(|value| format!("launch:{value}"))
        .or_else(|| {
            capabilities
                .identity
                .session_token
                .as_ref()
                .map(|value| format!("session:{value}"))
        })
}

pub(crate) async fn prepare_write(args: &WriteMemoryArgs) -> Result<PreparedWrite, LinkError> {
    match (&args.hex, &args.input_file) {
        (Some(hex), None) => prepare_inline(hex),
        (None, Some(input_file)) => prepare_file(input_file).await,
        (Some(_), Some(_)) => Err(bad_params(
            "write_memory accepts exactly one of hex or input_file, not both",
        )),
        (None, None) => Err(bad_params(
            "write_memory requires exactly one of hex or input_file",
        )),
    }
}

fn prepare_inline(hex: &str) -> Result<PreparedWrite, LinkError> {
    if hex.is_empty() {
        return Err(bad_params("hex must contain at least one byte"));
    }
    if hex.len() & 1 != 0 {
        return Err(bad_params("hex must have even length"));
    }
    let bytes = hex::decode(hex).map_err(|_| bad_params("hex decode failed"))?;
    enforce_size(bytes.len())?;
    Ok(prepared(bytes, "hex"))
}

async fn prepare_file(input: &WriteMemoryFileArgs) -> Result<PreparedWrite, LinkError> {
    let path = PathBuf::from(&input.path);
    if !path.is_absolute() {
        return Err(bad_params("input_file.path must be an absolute path"));
    }
    let offset = input.offset.map(|value| value.get()).unwrap_or(0);
    let length = usize::try_from(input.length.get())
        .map_err(|_| bad_params("input_file.length does not fit this host"))?;
    enforce_size(length)?;
    offset
        .checked_add(length as u64)
        .ok_or_else(|| bad_params("input_file offset+length overflows"))?;
    let expected_sha256 = input.sha256.as_deref().map(normalize_sha256).transpose()?;

    let task = tokio::task::spawn_blocking(move || read_file_slice(&path, offset, length));
    let bytes = tokio::time::timeout(Duration::from_millis(FILE_LOAD_TIMEOUT_MS), task)
        .await
        .map_err(|_| LinkError::Timeout)?
        .map_err(|error| LinkError::Protocol(format!("file input worker failed: {error}")))??;
    let result = prepared(bytes, "file");
    if expected_sha256
        .as_ref()
        .is_some_and(|expected| expected != &result.sha256)
    {
        return Err(bad_params(format!(
            "input_file sha256 mismatch: expected {}, got {}",
            expected_sha256.as_deref().unwrap_or_default(),
            result.sha256
        )));
    }
    Ok(result)
}

fn read_file_slice(path: &Path, offset: u64, length: usize) -> Result<Vec<u8>, LinkError> {
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| bad_params(format!("cannot inspect input_file: {error}")))?;
    if path_metadata.file_type().is_symlink() {
        return Err(bad_params("input_file.path must not be a symbolic link"));
    }
    if !path_metadata.is_file() {
        return Err(bad_params("input_file.path must name a regular file"));
    }

    let mut file = std::fs::File::open(path)
        .map_err(|error| bad_params(format!("cannot open input_file: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| bad_params(format!("cannot inspect opened input_file: {error}")))?;
    if !metadata.is_file() {
        return Err(bad_params("opened input_file is not a regular file"));
    }
    let end = offset
        .checked_add(length as u64)
        .ok_or_else(|| bad_params("input_file offset+length overflows"))?;
    if end > metadata.len() {
        return Err(bad_params(format!(
            "input_file slice [{offset}, {end}) exceeds file size {}",
            metadata.len()
        )));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| bad_params(format!("cannot seek input_file: {error}")))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)
        .map_err(|error| bad_params(format!("cannot read exact input_file slice: {error}")))?;
    Ok(bytes)
}

fn enforce_size(length: usize) -> Result<(), LinkError> {
    if length == 0 {
        return Err(bad_params(
            "write_memory input must contain at least one byte",
        ));
    }
    if length > MAX_WRITE_BYTES {
        return Err(bad_params(format!(
            "write_memory input length {length:#x} exceeds the {MAX_WRITE_BYTES:#x} byte cap"
        )));
    }
    Ok(())
}

fn normalize_sha256(value: &str) -> Result<String, LinkError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(bad_params(
            "input_file.sha256 must be exactly 64 hexadecimal characters",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn prepared(bytes: Vec<u8>, source_kind: &'static str) -> PreparedWrite {
    let sha256 = hex::encode(Sha256::digest(&bytes));
    PreparedWrite {
        bytes,
        source_kind,
        sha256,
    }
}

pub(crate) fn with_provenance(output: ToolOutput, prepared: &PreparedWrite) -> ToolOutput {
    let ToolOutput::Json(mut value) = output else {
        return output;
    };
    let object = match value {
        Value::Object(ref mut object) => object,
        _ => {
            return ToolOutput::Json(serde_json::json!({
                "result": value,
                "input_kind": prepared.source_kind,
                "input_bytes": prepared.bytes.len(),
                "input_sha256": prepared.sha256,
            }));
        }
    };
    object.insert(
        "input_kind".into(),
        Value::String(prepared.source_kind.into()),
    );
    object.insert(
        "input_bytes".into(),
        serde_json::json!(prepared.bytes.len()),
    );
    object.insert(
        "input_sha256".into(),
        Value::String(prepared.sha256.clone()),
    );
    ToolOutput::Json(value)
}

fn bad_params(message: impl Into<String>) -> LinkError {
    LinkError::Emulator {
        kind: "bad_params".into(),
        message: message.into(),
    }
}

#[cfg(test)]
#[path = "memory_write_tests.rs"]
mod tests;
