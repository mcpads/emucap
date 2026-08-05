use std::fs;

use super::recording_capability::{RecordingInputMovieCapability, INPUT_MOVIE_FORMAT};
use super::recording_input::*;

fn capability() -> RecordingInputMovieCapability {
    RecordingInputMovieCapability {
        format: INPUT_MOVIE_FORMAT.into(),
        port: 0,
        max_frames: 300,
        max_bytes: 1024,
        max_buttons_per_frame: 8,
    }
}

#[test]
fn acquires_canonical_identity_from_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("movie.txt");
    fs::write(&path, "1:B,A\n0:Right\n").unwrap();
    let acquired = acquire_recording_movie(&path, 2, &capability()).unwrap();
    assert_eq!(acquired.canonical_bytes, b"0:right\n1:a,b\n");
    assert_eq!(
        acquired.identity.bytes,
        acquired.canonical_bytes.len() as u64
    );
    assert_eq!(acquired.identity.sha256.len(), 64);
}

#[test]
fn rejects_relative_sparse_and_over_bound_movies() {
    assert!(matches!(
        acquire_recording_movie(std::path::Path::new("movie.txt"), 1, &capability()),
        Err(RecordingInputError::RelativePath)
    ));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("movie.txt");
    fs::write(&path, "0:a\n2:b\n").unwrap();
    assert!(matches!(
        acquire_recording_movie(&path, 2, &capability()),
        Err(RecordingInputError::Invalid(_))
    ));
    let mut small = capability();
    small.max_bytes = 2;
    assert!(matches!(
        acquire_recording_movie(&path, 2, &small),
        Err(RecordingInputError::ByteLimit(2))
    ));
}

#[cfg(unix)]
#[test]
fn rejects_a_symlink_source() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    let link = dir.path().join("movie.txt");
    fs::write(&source, "0:a\n").unwrap();
    symlink(&source, &link).unwrap();
    assert!(matches!(
        acquire_recording_movie(&link, 1, &capability()),
        Err(RecordingInputError::UnsafeFile)
    ));
}
