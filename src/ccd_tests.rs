use super::*;

fn write_ccd(directory: &Path, extension: &str) -> std::path::PathBuf {
    let ccd = directory.join(format!("disc.{extension}"));
    std::fs::write(
        &ccd,
        "[CloneCD]\nVersion=3\n[Disc]\nTocEntries=3\nSessions=1\nDataTracksScrambled=0\n[Entry 0]\nSession=1\nPoint=0xA0\nADR=1\nControl=4\nPMin=1\nPSec=0\nPLBA=-150\n[Entry 1]\nSession=1\nPoint=0xA1\nADR=1\nControl=4\nPMin=1\nPSec=0\nPLBA=0\n[Entry 2]\nSession=1\nPoint=0xA2\nADR=1\nControl=4\nPMin=0\nPSec=0\nPLBA=2\n",
    )
    .unwrap();
    let (image, subchannel) = companion_names(&ccd).unwrap();
    std::fs::write(directory.join(image), vec![0_u8; 2352 * 2]).unwrap();
    std::fs::write(directory.join(subchannel), vec![0_u8; 96 * 2]).unwrap();
    ccd
}

#[test]
fn clonecd_identity_includes_implicit_img_and_sub_members() {
    let directory = tempfile::tempdir().unwrap();
    let ccd = write_ccd(directory.path(), "CcD");
    let before = graph_identity(&ccd).unwrap();
    assert_eq!(before.files.len(), 3);

    let (_, subchannel) = companion_names(&ccd).unwrap();
    std::fs::write(directory.path().join(subchannel), vec![1_u8; 96 * 2]).unwrap();
    let after = graph_identity(&ccd).unwrap();
    assert_ne!(before.sha256, after.sha256);
}

#[test]
fn clonecd_requires_all_toc_entries_and_matching_companion_sizes() {
    let directory = tempfile::tempdir().unwrap();
    let ccd = write_ccd(directory.path(), "ccd");
    let (_, subchannel) = companion_names(&ccd).unwrap();
    std::fs::write(directory.path().join(subchannel), b"short").unwrap();
    assert!(validate_graph(&ccd).is_err());

    std::fs::write(
        &ccd,
        "[Disc]\nTocEntries=3\nSessions=1\nDataTracksScrambled=0\n[Entry 0]\nX=1\n",
    )
    .unwrap();
    assert!(validate_graph(&ccd).is_err());
}

#[test]
fn clonecd_rejects_duplicate_sections_and_keys_as_ambiguous() {
    let directory = tempfile::tempdir().unwrap();
    let ccd = directory.path().join("disc.ccd");
    std::fs::write(
        &ccd,
        "[Disc]\nTocEntries=3\nTocEntries=4\nSessions=1\nDataTracksScrambled=0\n",
    )
    .unwrap();
    assert!(references(&ccd).is_err());

    std::fs::write(
        &ccd,
        "[Disc]\nTocEntries=3\nSessions=1\nDataTracksScrambled=0\n[Disc]\n",
    )
    .unwrap();
    assert!(references(&ccd).is_err());
}

#[cfg(unix)]
#[test]
fn clonecd_rejects_symlinked_implicit_members() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let ccd = write_ccd(directory.path(), "ccd");
    let (image, _) = companion_names(&ccd).unwrap();
    std::fs::remove_file(directory.path().join(&image)).unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    symlink(outside.path(), directory.path().join(image)).unwrap();
    assert_eq!(
        validate_graph(&ccd).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
