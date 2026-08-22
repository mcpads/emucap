use super::*;

#[test]
fn parses_quoted_and_unquoted_file_directives_case_insensitively() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(
        &cue,
        "file \"Track 01.bin\" BINARY\n  TRACK 01 MODE1/2352\nFILE\ttrack02.bin BINARY\n",
    )
    .unwrap();
    let references = referenced_files(&cue).unwrap();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].declared_name, "Track 01.bin");
    assert_eq!(references[1].declared_name, "track02.bin");
}

#[test]
fn validation_fails_loudly_when_a_referenced_track_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"missing.bin\" BINARY\n").unwrap();
    let error = validate_graph(&cue).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().contains("missing.bin"));
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
        "FILE \"track01.bin\" BINARY\nFILE \"track02.bin\" BINARY\nFILE \"track01.bin\" BINARY\n",
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
    assert_ne!(before.files[2].sha1, after.files[2].sha1);
}

#[test]
fn rejects_references_that_escape_the_cue_directory() {
    let dir = tempfile::tempdir().unwrap();
    let cue = dir.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"../outside.bin\" BINARY\n").unwrap();
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
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\n").unwrap();
    let error = validate_graph(&cue).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
