use super::dump::*;
use std::fs;

#[test]
fn load_reads_regions_and_bins() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("wram.bin"), [1u8, 2, 3]).unwrap();
    fs::write(tmp.path().join("cram.bin"), [9u8, 9]).unwrap();
    let meta = r#"[
      { "name": "wram", "memory_type": "snesWorkRam", "base_address": 8257536, "size": 3 },
      { "name": "cram", "memory_type": "snesCgRam", "base_address": 0, "size": 2 }
    ]"#;
    fs::write(tmp.path().join("regions.json"), meta).unwrap();

    let set = load(tmp.path()).unwrap();
    assert_eq!(set.regions.get("wram").unwrap().bytes, vec![1, 2, 3]);
    assert_eq!(set.regions.get("wram").unwrap().base_address, 8257536);
    assert_eq!(set.regions.get("cram").unwrap().bytes, vec![9, 9]);
}

#[test]
fn load_errors_when_meta_missing() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(load(tmp.path()), Err(DumpError::MetaNotFound(_))));
}

#[test]
fn load_errors_when_bin_missing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("regions.json"),
        r#"[{ "name": "wram", "memory_type": "snesWorkRam", "base_address": 0, "size": 1 }]"#,
    )
    .unwrap();
    assert!(matches!(load(tmp.path()), Err(DumpError::BinNotFound(_))));
}
