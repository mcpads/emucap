use super::*;
use crate::args::Num;
use emucap::live::link::EmulatorIdentity;

fn args(hex: Option<&str>, input_file: Option<WriteMemoryFileArgs>) -> WriteMemoryArgs {
    WriteMemoryArgs {
        memory_type: "ram".into(),
        address: Num(0),
        hex: hex.map(String::from),
        input_file,
    }
}

fn file_args(path: &Path, offset: u64, length: u64, sha256: Option<String>) -> WriteMemoryFileArgs {
    WriteMemoryFileArgs {
        path: path.display().to_string(),
        offset: Some(Num(offset)),
        length: Num(length),
        sha256,
    }
}

#[tokio::test]
async fn inline_input_is_decoded_and_hashed() {
    let prepared = prepare_write(&args(Some("deadbeef"), None)).await.unwrap();
    assert_eq!(prepared.bytes, [0xde, 0xad, 0xbe, 0xef]);
    let ToolOutput::Json(value) = with_provenance(
        ToolOutput::Json(serde_json::json!({"written": 4})),
        &prepared,
    ) else {
        panic!("JSON expected");
    };
    assert_eq!(value["input_kind"], "hex");
    assert_eq!(value["input_bytes"], 4);
    assert_eq!(
        value["input_sha256"],
        "5f78c33274e43fa9de5659265c1d917e25c03722dcb0b8d27db8d5feaa813953"
    );
}

#[tokio::test]
async fn exactly_one_input_source_is_required() {
    assert!(prepare_write(&args(None, None)).await.is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("payload.bin");
    std::fs::write(&path, [1]).unwrap();
    let both = args(Some("01"), Some(file_args(&path, 0, 1, None)));
    assert!(prepare_write(&both).await.is_err());
}

#[tokio::test]
async fn file_slice_is_snapshotted_and_expected_hash_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("payload.bin");
    std::fs::write(&path, [0, 1, 2, 3, 4]).unwrap();
    let expected = hex::encode(Sha256::digest([1, 2, 3]));
    let input = args(
        None,
        Some(file_args(&path, 1, 3, Some(expected.to_uppercase()))),
    );
    let prepared = prepare_write(&input).await.unwrap();
    assert_eq!(prepared.bytes, [1, 2, 3]);
    assert_eq!(prepared.source_kind, "file");
    assert_eq!(prepared.sha256, expected);
}

#[tokio::test]
async fn file_rejection_happens_before_any_write_can_be_prepared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("payload.bin");
    std::fs::write(&path, [1, 2, 3]).unwrap();

    let out_of_range = args(None, Some(file_args(&path, 2, 2, None)));
    assert!(prepare_write(&out_of_range).await.is_err());

    let wrong_hash = args(None, Some(file_args(&path, 0, 3, Some("0".repeat(64)))));
    assert!(prepare_write(&wrong_hash).await.is_err());

    let relative = args(
        None,
        Some(WriteMemoryFileArgs {
            path: "payload.bin".into(),
            offset: None,
            length: Num(1),
            sha256: None,
        }),
    );
    assert!(prepare_write(&relative).await.is_err());
}

#[tokio::test]
async fn malformed_and_oversized_inputs_fail_during_staging() {
    assert!(prepare_write(&args(Some("0"), None)).await.is_err());
    assert!(prepare_write(&args(Some("zz"), None)).await.is_err());
    assert!(prepare_write(&args(Some(""), None)).await.is_err());
    let oversized = "00".repeat(MAX_WRITE_BYTES + 1);
    assert!(prepare_write(&args(Some(&oversized), None)).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn symbolic_link_input_is_rejected() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.bin");
    let link = dir.path().join("link.bin");
    std::fs::write(&target, [1]).unwrap();
    symlink(&target, &link).unwrap();
    assert!(
        prepare_write(&args(None, Some(file_args(&link, 0, 1, None))))
            .await
            .is_err()
    );
}

#[test]
fn generation_marker_prefers_launch_id_then_session_token() {
    let mut capabilities = Capabilities::empty();
    assert_eq!(generation_marker(&capabilities), None);
    capabilities.identity = EmulatorIdentity {
        session_token: Some("token".into()),
        ..Default::default()
    };
    assert_eq!(
        generation_marker(&capabilities).as_deref(),
        Some("session:token")
    );
    capabilities.identity.launch_id = Some("generation".into());
    assert_eq!(
        generation_marker(&capabilities).as_deref(),
        Some("launch:generation")
    );
}
