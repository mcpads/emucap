use super::*;

#[test]
fn toc_identity_covers_all_track_file_directives() {
    let directory = tempfile::tempdir().unwrap();
    let toc = directory.path().join("disc.toc");
    std::fs::write(directory.path().join("data.bin"), b"data").unwrap();
    std::fs::write(directory.path().join("audio.wav"), b"audio").unwrap();
    std::fs::write(
        &toc,
        "CD_ROM_XA\nTRACK MODE1_RAW\nDATAFILE \"data.bin\"\nTRACK AUDIO\nAUDIOFILE \"audio.wav\" 00:00:00\n",
    )
    .unwrap();
    let before = graph_identity(&toc).unwrap();
    assert_eq!(before.files.len(), 3);
    std::fs::write(directory.path().join("audio.wav"), b"changed").unwrap();
    assert_ne!(before.sha256, graph_identity(&toc).unwrap().sha256);
}

#[test]
fn toc_rejects_fileless_tracks_and_escaping_paths() {
    let directory = tempfile::tempdir().unwrap();
    let toc = directory.path().join("disc.toc");
    std::fs::write(&toc, "TRACK AUDIO\nTRACK MODE1\nDATAFILE data.bin\n").unwrap();
    assert!(references(&toc).is_err());

    std::fs::write(&toc, "TRACK MODE1\nDATAFILE ../../etc/passwd\n").unwrap();
    assert!(references(&toc).is_err());
}
