use super::*;

#[test]
fn graph_digest_changes_with_a_member_but_not_the_entry_file_name() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for directory in [first.path(), second.path()] {
        std::fs::write(directory.join("track.bin"), b"track").unwrap();
    }
    let first_entry = first.path().join("first.test");
    let second_entry = second.path().join("second.test");
    std::fs::write(&first_entry, b"descriptor").unwrap();
    std::fs::write(&second_entry, b"descriptor").unwrap();
    let references = vec![MediaReference {
        declared_name: "track.bin".into(),
    }];

    let first_identity = graph_identity(
        &first_entry,
        "TEST",
        "entry.test",
        b"test-domain\0",
        &references,
    )
    .unwrap();
    let second_identity = graph_identity(
        &second_entry,
        "TEST",
        "entry.test",
        b"test-domain\0",
        &references,
    )
    .unwrap();
    assert_eq!(first_identity.sha256, second_identity.sha256);

    std::fs::write(second.path().join("track.bin"), b"changed").unwrap();
    let changed = graph_identity(
        &second_entry,
        "TEST",
        "entry.test",
        b"test-domain\0",
        &references,
    )
    .unwrap();
    assert_ne!(first_identity.sha256, changed.sha256);
}

#[test]
fn validation_rejects_an_entry_listed_as_its_own_member() {
    let directory = tempfile::tempdir().unwrap();
    let entry = directory.path().join("disc.test");
    std::fs::write(&entry, b"descriptor").unwrap();
    let references = vec![MediaReference {
        declared_name: "disc.test".into(),
    }];

    assert!(validate_references(&entry, "TEST", &references).is_err());
    assert!(graph_identity(&entry, "TEST", "entry.test", b"test-domain\0", &references,).is_err());
}

#[cfg(unix)]
#[test]
fn validation_rejects_symlinked_intermediate_directories() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("track.bin"), b"secret").unwrap();
    symlink(outside.path(), root.path().join("tracks")).unwrap();
    let entry = root.path().join("disc.test");
    std::fs::write(&entry, b"descriptor").unwrap();
    let references = vec![MediaReference {
        declared_name: "tracks/track.bin".into(),
    }];

    assert_eq!(
        validate_references(&entry, "TEST", &references)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[cfg(unix)]
#[test]
fn descriptor_reads_reject_a_symlinked_entry() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.test");
    let entry = directory.path().join("disc.test");
    std::fs::write(&target, b"descriptor").unwrap();
    symlink(&target, &entry).unwrap();

    assert!(read_descriptor(&entry, "TEST", MAX_DESCRIPTOR_BYTES).is_err());
}

#[test]
fn text_descriptors_reject_embedded_string_terminators() {
    assert_eq!(
        descriptor_text(b"FILE \"track.bin\" BINARY\0ignored", "CUE")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
