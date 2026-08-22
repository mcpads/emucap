use super::*;

#[test]
fn parses_quoted_and_unquoted_file_directives_case_insensitively() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(
        &cue,
        "file \"Track 01.bin\" BINARY\n  TRACK 01 MODE1/2352\nFILE\ttrack02.bin BINARY\nTRACK 02 AUDIO\n",
    )
    .unwrap();
    let references = referenced_files(&cue).unwrap();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].declared_name, "Track 01.bin");
    assert_eq!(references[1].declared_name, "track02.bin");
}

#[test]
fn parses_the_utf8_bom_consumed_by_mednafen() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    std::fs::write(
        &cue,
        "\u{feff}FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n",
    )
    .unwrap();
    assert_eq!(referenced_files(&cue).unwrap().len(), 1);
}

#[test]
fn validation_fails_loudly_when_a_referenced_track_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"missing.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    let error = validate_graph(&cue).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn graph_identity_covers_the_entry_file_and_all_unique_referenced_files() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    let track1 = dir.path().join("track01.bin");
    let track2 = dir.path().join("track02.bin");
    std::fs::write(&track1, b"track-one").unwrap();
    std::fs::write(&track2, b"track-two").unwrap();
    std::fs::write(
        &cue,
        "FILE \"track01.bin\" BINARY\nTRACK 01 MODE1/2352\nFILE \"track02.bin\" BINARY\nTRACK 02 AUDIO\nFILE \"track01.bin\" BINARY\nTRACK 03 AUDIO\n",
    )
    .unwrap();

    let before = graph_identity(&cue).unwrap();
    assert_eq!(before.files.len(), 3);
    assert_eq!(
        before.size,
        cue.metadata().unwrap().len()
            + track1.metadata().unwrap().len()
            + track2.metadata().unwrap().len()
    );

    std::fs::write(&track2, b"track-two-changed").unwrap();
    let after = graph_identity(&cue).unwrap();
    assert_ne!(before.sha1, after.sha1);
    assert_ne!(before.sha256, after.sha256);
    assert_ne!(before.files[2].sha1, after.files[2].sha1);
    assert_ne!(before.files[2].sha256, after.files[2].sha256);
}

#[test]
fn rejects_references_that_escape_the_cue_directory() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(
        &cue,
        "FILE \"../outside.bin\" BINARY\nTRACK 01 MODE1/2352\n",
    )
    .unwrap();
    let error = referenced_files(&cue).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_track_member() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside.bin");
    std::fs::write(&outside, b"track").unwrap();
    symlink(&outside, dir.path().join("track.bin")).unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    let error = validate_graph(&cue).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn rejects_file_lists_that_define_no_tracks() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(dir.path().join("first.bin"), b"first").unwrap();
    std::fs::write(dir.path().join("second.bin"), b"second").unwrap();
    std::fs::write(
        &cue,
        "FILE \"first.bin\" BINARY\nFILE \"second.bin\" BINARY\n",
    )
    .unwrap();

    assert_eq!(
        referenced_files(&cue).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_absolute_and_parent_system_file_references() {
    let dir = tempfile::tempdir().unwrap();
    for declared_name in [
        "/etc/passwd",
        "../../etc/passwd",
        "C:/Windows/System32/config/SAM",
    ] {
        let cue = dir.path().join("disc.cue");
        std::fs::write(
            &cue,
            format!("FILE \"{declared_name}\" BINARY\nTRACK 01 MODE1/2352\n"),
        )
        .unwrap();
        let error = referenced_files(&cue).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{declared_name}");
    }
}

#[test]
fn rejects_track_before_file_and_nonconsecutive_track_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(&cue, "TRACK 01 MODE1/2352\n").unwrap();
    assert!(referenced_files(&cue).is_err());

    std::fs::write(
        &cue,
        "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\nTRACK 03 AUDIO\n",
    )
    .unwrap();
    assert!(referenced_files(&cue).is_err());
}

#[test]
fn rejects_malformed_file_directives_instead_of_hashing_arbitrary_names() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    for contents in [
        "FILE \"unterminated.bin BINARY\nTRACK 01 MODE1/2352\n",
        "FILE \"track.bin\" BINARY ignored\nTRACK 01 MODE1/2352\n",
        "FILE \"track.bin\" EXECUTABLE\nTRACK 01 MODE1/2352\n",
    ] {
        std::fs::write(&cue, contents).unwrap();
        assert_eq!(
            referenced_files(&cue).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn rejects_embedded_string_terminators_before_loader_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(dir.path().join("track.bin"), b"track").unwrap();
    std::fs::write(
        &cue,
        b"FILE \"track.bin\" BINARY\0ignored\nTRACK 01 MODE1/2352\n",
    )
    .unwrap();

    assert_eq!(
        validate_graph(&cue).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_lines_that_a_supported_loader_would_split_and_reinterpret() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    let mut line = "REM ".to_string();
    let padding = MAX_CUE_LINE_BYTES - line.len();
    line.push_str(&"x".repeat(padding));
    line.push_str("FILE \"hidden.bin\" BINARY\nTRACK 01 MODE1/2352\n");
    std::fs::write(&cue, line).unwrap();

    assert!(referenced_files(&cue).is_err());
}

#[test]
fn rejects_empty_tracks_and_oversized_sparse_tracks_before_hashing() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    let track = dir.path().join("track.bin");
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    std::fs::write(&track, []).unwrap();
    assert!(validate_graph(&cue).is_err());

    let file = std::fs::File::create(&track).unwrap();
    file.set_len(MAX_TRACK_BYTES + 1).unwrap();
    assert!(validate_graph(&cue).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_a_fifo_track_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("track.bin");
    let fifo_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo_path is a valid NUL-terminated path and the mode is conventional owner-only.
    assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
    let cue = dir.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();

    assert_eq!(
        validate_graph(&cue).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_the_cue_entry_as_its_own_track() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"disc.cue\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();

    assert!(validate_graph(&cue).is_err());
    assert!(graph_identity(&cue).is_err());
}

#[test]
fn mednafen_identity_includes_only_its_valid_implicit_sbi_sidecar() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    let track = directory.path().join("track.bin");
    let sbi = directory.path().join("disc.sbi");
    std::fs::write(&track, b"track").unwrap();
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    std::fs::write(&sbi, b"SBI\0").unwrap();

    let explicit = graph_identity(&cue).unwrap();
    let before = mednafen_graph_identity(&cue).unwrap();
    assert_eq!(explicit.files.len(), 2);
    assert_eq!(before.files.len(), 3);

    std::fs::write(
        &sbi,
        [
            b'S', b'B', b'I', 0, 0x00, 0x02, 0x03, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
    )
    .unwrap();
    let after = mednafen_graph_identity(&cue).unwrap();
    assert_ne!(before.sha256, after.sha256);
    assert_eq!(explicit.sha256, graph_identity(&cue).unwrap().sha256);
}

#[test]
fn mednafen_rejects_an_invalid_implicit_sbi_before_launch() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.CuE");
    std::fs::write(directory.path().join("track.bin"), b"track").unwrap();
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    std::fs::write(directory.path().join("disc.SbI"), b"not-an-sbi").unwrap();

    assert!(validate_graph(&cue).is_ok());
    assert!(validate_mednafen_graph(&cue).is_err());
    assert!(mednafen_graph_identity(&cue).is_err());
}
