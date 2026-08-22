use super::*;

fn write_cue(directory: &Path, name: &str, track_name: &str, bytes: &[u8]) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join(track_name), bytes).unwrap();
    std::fs::write(
        directory.join(name),
        format!("FILE \"{track_name}\" BINARY\nTRACK 01 MODE1/2352\n"),
    )
    .unwrap();
}

#[test]
fn nested_playlist_identity_covers_descriptors_and_disc_tracks() {
    let directory = tempfile::tempdir().unwrap();
    let discs = directory.path().join("discs");
    write_cue(&discs, "one.cue", "one.bin", b"one");
    write_cue(&discs, "two.cue", "two.bin", b"two");
    std::fs::write(discs.join("set.m3u"), "one.cue\ntwo.cue\n").unwrap();
    let playlist = directory.path().join("games.m3u");
    std::fs::write(&playlist, "#MEDNAFEN_DEFAULT\ndiscs/set.m3u\n").unwrap();

    let before = graph_identity(&playlist).unwrap();
    assert_eq!(before.files.len(), 6);
    std::fs::write(discs.join("two.bin"), b"changed").unwrap();
    assert_ne!(before.sha256, graph_identity(&playlist).unwrap().sha256);
}

#[test]
fn playlist_rejects_cycles_parent_escape_and_unknown_members() {
    let directory = tempfile::tempdir().unwrap();
    let playlist = directory.path().join("games.m3u");
    std::fs::write(&playlist, "games.m3u\n").unwrap();
    assert!(references(&playlist).is_err());

    std::fs::write(&playlist, "../outside.cue\n").unwrap();
    assert!(references(&playlist).is_err());

    std::fs::write(directory.path().join("disc.iso"), b"iso").unwrap();
    std::fs::write(&playlist, "disc.iso\n").unwrap();
    assert!(references(&playlist).is_err());
}

#[test]
fn playlist_accepts_a_utf8_bom_and_binds_mednafen_sbi_files() {
    let directory = tempfile::tempdir().unwrap();
    write_cue(directory.path(), "disc.cue", "track.bin", b"track");
    std::fs::write(directory.path().join("disc.sbi"), b"SBI\0").unwrap();
    let playlist = directory.path().join("games.m3u");
    std::fs::write(&playlist, "\u{feff}disc.cue\n").unwrap();

    let identity = graph_identity(&playlist).unwrap();
    assert_eq!(identity.files.len(), 4);
}

#[test]
fn playlist_rejects_excessive_disc_and_recursion_work() {
    let directory = tempfile::tempdir().unwrap();
    write_cue(directory.path(), "disc.cue", "track.bin", b"track");
    let playlist = directory.path().join("games.m3u");
    std::fs::write(&playlist, "disc.cue\n".repeat(MAX_DISCS + 1)).unwrap();
    assert!(references(&playlist).is_err());

    for depth in 0..=MAX_RECURSION_DEPTH + 1 {
        let next = if depth == MAX_RECURSION_DEPTH + 1 {
            "disc.cue".to_string()
        } else {
            format!("level-{}.m3u", depth + 1)
        };
        std::fs::write(
            directory.path().join(format!("level-{depth}.m3u")),
            format!("{next}\n"),
        )
        .unwrap();
    }
    std::fs::write(&playlist, "level-0.m3u\n").unwrap();
    assert!(references(&playlist).is_err());
}

#[cfg(unix)]
#[test]
fn playlist_rejects_a_symlinked_nested_descriptor() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    symlink(outside.path(), directory.path().join("disc.cue")).unwrap();
    let playlist = directory.path().join("games.m3u");
    std::fs::write(&playlist, "disc.cue\n").unwrap();
    assert!(references(&playlist).is_err());
}
