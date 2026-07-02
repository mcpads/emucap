use assert_cmd::Command;

#[test]
fn add_rejects_bad_length() {
    let tmp = tempfile::tempdir().unwrap();
    let rom = tmp.path().join("g.sfc");
    std::fs::write(&rom, b"x").unwrap();
    Command::cargo_bin("emucap")
        .unwrap()
        .args([
            "regression",
            "add",
            tmp.path().to_str().unwrap(),
            "--id",
            "c1",
            "--desc",
            "d",
            "--from-savestate",
            rom.to_str().unwrap(),
            "--predicate",
            "wram:0:16:eq:0",
            "--rom",
            rom.to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn add_input_replay_copies_start_savestate() {
    // --from-input + --start <base.mss> 케이스는 start savestate를 케이스 디렉토리로
    // 복사해야 한다. 안 그러면 러너가 .mss를 못 찾아 항상 MissingPayload(조용한 실패).
    let tmp = tempfile::tempdir().unwrap();
    let rom = tmp.path().join("g.sfc");
    std::fs::write(&rom, b"rom-bytes").unwrap();
    let movie = tmp.path().join("m.movie");
    std::fs::write(&movie, b"frames").unwrap();
    let base = tmp.path().join("base.mss");
    std::fs::write(&base, b"savestate-bytes").unwrap();
    let suite = tmp.path().join("suite");
    Command::cargo_bin("emucap")
        .unwrap()
        .args([
            "regression",
            "add",
            suite.to_str().unwrap(),
            "--id",
            "c1",
            "--desc",
            "d",
            "--from-input",
            movie.to_str().unwrap(),
            "--start",
            base.to_str().unwrap(),
            "--predicate",
            "wram:0:2:eq:9",
            "--rom",
            rom.to_str().unwrap(),
        ])
        .assert()
        .success();
    // 케이스 디렉토리에 .mss(start savestate)가 복사되어 있어야 한다.
    let case_dir = suite.join("c1");
    let mss_count = std::fs::read_dir(&case_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().map(|x| x == "mss").unwrap_or(false))
        .count();
    assert_eq!(
        mss_count, 1,
        "start savestate가 케이스 디렉토리에 .mss로 복사되어야 한다"
    );
}

#[test]
fn add_rejects_path_traversal_id() {
    // id가 경로 구성요소를 벗어나면(디렉토리 탈출) 거부해야 한다.
    let tmp = tempfile::tempdir().unwrap();
    let rom = tmp.path().join("g.sfc");
    std::fs::write(&rom, b"x").unwrap();
    let suite = tmp.path().join("suite");
    Command::cargo_bin("emucap")
        .unwrap()
        .args([
            "regression",
            "add",
            suite.to_str().unwrap(),
            "--id",
            "../escape",
            "--desc",
            "d",
            "--from-savestate",
            rom.to_str().unwrap(),
            "--predicate",
            "wram:0:2:eq:9",
            "--rom",
            rom.to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn add_then_reject_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let rom = tmp.path().join("g.sfc");
    std::fs::write(&rom, b"x").unwrap();
    let args = [
        "regression",
        "add",
        tmp.path().to_str().unwrap(),
        "--id",
        "c1",
        "--desc",
        "d",
        "--from-savestate",
        rom.to_str().unwrap(),
        "--predicate",
        "wram:0:2:eq:9",
        "--rom",
        rom.to_str().unwrap(),
    ];
    Command::cargo_bin("emucap")
        .unwrap()
        .args(args)
        .assert()
        .success();
    Command::cargo_bin("emucap")
        .unwrap()
        .args(args)
        .assert()
        .failure(); // 충돌
}
