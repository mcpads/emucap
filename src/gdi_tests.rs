use super::*;

fn write_valid_gdi(directory: &Path) -> std::path::PathBuf {
    for name in ["track 01.bin", "track02.raw", "track03.bin"] {
        std::fs::write(directory.join(name), vec![0_u8; 4704]).unwrap();
    }
    let gdi = directory.join("disc.gdi");
    std::fs::write(
        &gdi,
        "3\n1 0 4 2352 \"track 01.bin\" 0\n2 450 0 2352 track02.raw 0\n3 45000 4 2352 track03.bin 0\n",
    )
    .unwrap();
    gdi
}

#[test]
fn identity_covers_descriptor_and_every_gdi_track() {
    let directory = tempfile::tempdir().unwrap();
    let gdi = write_valid_gdi(directory.path());
    let before = graph_identity(&gdi).unwrap();
    assert_eq!(before.files.len(), 4);

    std::fs::write(directory.path().join("track03.bin"), vec![1_u8; 4704]).unwrap();
    let after = graph_identity(&gdi).unwrap();
    assert_ne!(before.sha256, after.sha256);
    assert_ne!(before.files[3].sha256, after.files[3].sha256);
}

#[test]
fn rejects_count_mismatch_invalid_offsets_and_path_escape() {
    let directory = tempfile::tempdir().unwrap();
    let gdi = write_valid_gdi(directory.path());
    std::fs::write(&gdi, "3\n1 0 4 2352 track.bin 0\n").unwrap();
    assert!(validate_graph(&gdi).is_err());

    std::fs::write(directory.path().join("track.bin"), b"tiny").unwrap();
    std::fs::write(
        &gdi,
        "3\n1 0 4 2352 track.bin 10\n2 450 0 2352 track.bin 0\n3 45000 4 2352 track.bin 0\n",
    )
    .unwrap();
    assert!(validate_graph(&gdi).is_err());

    std::fs::write(
        &gdi,
        "3\n1 0 4 2352 ../../etc/passwd 0\n2 450 0 2352 track.bin 0\n3 45000 4 2352 track.bin 0\n",
    )
    .unwrap();
    assert!(validate_graph(&gdi).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_gdi_tracks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let gdi = write_valid_gdi(directory.path());
    std::fs::remove_file(directory.path().join("track03.bin")).unwrap();
    symlink(outside.path(), directory.path().join("track03.bin")).unwrap();
    assert_eq!(
        validate_graph(&gdi).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
