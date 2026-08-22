use super::path_safety::{
    atomic_copy_file, atomic_write_file, is_hyphenated_ascii_id, is_portable_file_name,
    is_portable_relative_member, open_regular_member_no_follow,
    read_bounded_regular_file_no_follow, read_bounded_utf8_regular_file_no_follow,
    regular_member_path,
};

#[test]
fn path_derived_identifiers_allow_only_alphanumeric_hyphen_segments() {
    for valid in ["A", "01JTEST", "run-01JTEST", "capture-abc123"] {
        assert!(is_hyphenated_ascii_id(valid, 96), "{valid:?}");
    }
    for invalid in [
        "",
        ".",
        "..",
        "-run",
        "run-",
        "run--id",
        "run_id",
        "run.id",
        "a/b",
        "a\\b",
        "/absolute",
        "CON",
        "lpt1",
    ] {
        assert!(!is_hyphenated_ascii_id(invalid, 96), "{invalid:?}");
    }
    assert!(!is_hyphenated_ascii_id(&"a".repeat(97), 96));
}

#[test]
fn relative_members_cannot_escape_or_change_platform_meaning() {
    for valid in [
        "inputs.movie",
        "snapshots/wram.bin",
        "track 01.bin",
        "한글/트랙 01.bin",
    ] {
        assert!(is_portable_relative_member(valid), "{valid:?}");
    }
    for invalid in [
        "",
        ".",
        "../outside",
        "a/../../outside",
        "a\\..\\outside",
        "/tmp/file",
        "CON",
        "media/aux.bin",
        "track:01.bin",
        "track?.bin",
        "trailing. ",
    ] {
        assert!(!is_portable_relative_member(invalid), "{invalid:?}");
    }
}

#[test]
fn portable_file_names_allow_extensions_but_not_paths() {
    for valid in ["inputs.movie", "state-01.mss", "track_01.bin"] {
        assert!(is_portable_file_name(valid, 64), "{valid:?}");
    }
    for invalid in [
        ".hidden",
        "trailing.",
        "../state.mss",
        "dir/state.mss",
        "a\\b.mss",
    ] {
        assert!(!is_portable_file_name(invalid, 64), "{invalid:?}");
    }
}

#[test]
fn bounded_regular_reads_reject_oversize_and_invalid_utf8() {
    let root = tempfile::tempdir().unwrap();
    let oversized = root.path().join("oversized.bin");
    let invalid_utf8 = root.path().join("invalid.txt");
    std::fs::write(&oversized, b"12345").unwrap();
    std::fs::write(&invalid_utf8, [0xff]).unwrap();

    assert!(read_bounded_regular_file_no_follow(&oversized, 4).is_err());
    assert!(read_bounded_utf8_regular_file_no_follow(&invalid_utf8, 4).is_err());
}

#[cfg(unix)]
#[test]
fn regular_member_rejects_symlinks_in_owned_components() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    symlink(outside.path(), root.path().join("member.bin")).unwrap();
    assert!(regular_member_path(root.path(), "member.bin").is_err());
}

#[cfg(unix)]
#[test]
fn regular_member_rejects_a_unix_socket_without_connecting() {
    let root = tempfile::tempdir().unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(root.path().join("member.bin")).unwrap();

    assert!(open_regular_member_no_follow(root.path(), "member.bin").is_err());
}

#[cfg(unix)]
#[test]
fn atomic_write_rejects_a_destination_symlink_without_changing_its_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside.json");
    std::fs::write(&outside, b"original").unwrap();
    let output = root.path().join("output.json");
    symlink(&outside, &output).unwrap();
    assert!(atomic_write_file(&output, b"replacement").is_err());
    assert_eq!(std::fs::read(outside).unwrap(), b"original");
}

#[cfg(unix)]
#[test]
fn atomic_copy_rejects_a_destination_symlink_without_changing_its_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.bin");
    let outside = root.path().join("outside.bin");
    let output = root.path().join("output.bin");
    std::fs::write(&source, b"replacement").unwrap();
    std::fs::write(&outside, b"original").unwrap();
    symlink(&outside, &output).unwrap();

    assert!(atomic_copy_file(&source, &output).is_err());
    assert_eq!(std::fs::read(outside).unwrap(), b"original");
}
