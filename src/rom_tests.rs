use super::sha1_of_file;
use std::io::Write;

#[test]
fn hashes_known_content() {
    // "abc"의 SHA-1은 a9993e364706816aba3e25717850c26c9cd0d89d
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"abc").unwrap();
    let hash = sha1_of_file(f.path()).unwrap();
    assert_eq!(hash, "a9993e364706816aba3e25717850c26c9cd0d89d");
}

#[test]
fn errors_on_missing_file() {
    let p = std::path::Path::new("/nonexistent/does-not-exist.sfc");
    assert!(sha1_of_file(p).is_err());
}
