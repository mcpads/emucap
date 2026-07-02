use super::model::*;
use super::store;
use tempfile::TempDir;

fn sample_run(id: &str, rom: &str) -> Run {
    Run {
        format_version: RUN_FORMAT_VERSION,
        id: id.into(),
        rom_sha1: rom.into(),
        goal: None,
        description: None,
        tags: vec![],
        status: RunStatus::Done,
        started_at: "t".into(),
        ended_at: Some("t2".into()),
        agent: None,
        session: None,
        connection_ref: None,
        repro_base: Some("reset".into()),
        repro_movie_ref: None,
        repro_status: Some(ReproStatus::ReplayableWithInterventions),
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    }
}

#[test]
fn save_then_load_run_round_trips() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let run = sample_run("01RUN", "sha_a");
    store::save_run(root, &run).unwrap();
    let back = store::load_run(root, "sha_a", "01RUN").unwrap();
    assert_eq!(run, back);
}

#[test]
fn walk_runs_finds_all_run_json() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &sample_run("01A", "sha_a")).unwrap();
    store::save_run(root, &sample_run("01B", "sha_a")).unwrap();
    store::save_run(root, &sample_run("01C", "sha_b")).unwrap();
    let mut ids: Vec<String> = store::walk_runs(root)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["01A", "01B", "01C"]);
}

#[test]
fn partial_tmp_file_is_ignored_by_walk() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &sample_run("01A", "sha_a")).unwrap();
    // 부분 쓰기 흉내: .tmp 파일을 run 디렉토리에 둔다
    let p = root.join("roms/sha_a/runs/01A/run.json.tmp");
    std::fs::write(p, b"{ broken").unwrap();
    assert_eq!(store::walk_runs(root).unwrap().len(), 1); // .tmp는 무시
}

#[test]
fn corrupt_run_json_errors_not_skipped() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let p = root.join("roms/sha_a/runs/01A");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("run.json"), b"{ not json").unwrap();
    assert!(store::walk_runs(root).is_err()); // 조용히 스킵 금지
}

#[test]
fn find_run_by_id_targets_one_and_isolates_corrupt() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let run = Run {
        format_version: RUN_FORMAT_VERSION,
        id: "target".into(),
        rom_sha1: "romA".into(),
        goal: None,
        description: None,
        tags: vec![],
        status: RunStatus::Done,
        started_at: "t".into(),
        ended_at: None,
        agent: None,
        session: None,
        connection_ref: None,
        repro_base: None,
        repro_movie_ref: None,
        repro_status: None,
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    };
    store::save_run(root, &run).unwrap();
    // 무관 rom에 손상 run.json
    let bad = root.join("roms").join("romB").join("runs").join("other");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("run.json"), b"{ broken").unwrap();

    // 타깃은 정상 로드(무관 손상 격리)
    let got = store::find_run_by_id(root, "target").unwrap();
    assert_eq!(got.unwrap().id, "target");
    // 미존재 → Ok(None)
    assert!(store::find_run_by_id(root, "nope").unwrap().is_none());
}

#[test]
fn find_run_by_id_errors_on_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mk = |rom: &str| Run {
        format_version: RUN_FORMAT_VERSION,
        id: "dup".into(),
        rom_sha1: rom.into(),
        goal: None,
        description: None,
        tags: vec![],
        status: RunStatus::Done,
        started_at: "t".into(),
        ended_at: None,
        agent: None,
        session: None,
        connection_ref: None,
        repro_base: None,
        repro_movie_ref: None,
        repro_status: None,
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    };
    store::save_run(root, &mk("romA")).unwrap();
    store::save_run(root, &mk("romB")).unwrap();
    assert!(matches!(
        crate::track::store::find_run_by_id(root, "dup"),
        Err(crate::track::store::TrackError::Conflict(_))
    ));
}

#[test]
fn find_run_by_id_propagates_corrupt_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = root.join("roms").join("romA").join("runs").join("target");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("run.json"), b"{ broken").unwrap();
    assert!(crate::track::store::find_run_by_id(root, "target").is_err());
}

#[test]
fn walk_findings_lenient_skips_foreign_and_corrupt() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let f = Finding {
        id: "01F".into(),
        rom_sha1: "sha_a".into(),
        run_id: None,
        claim: "c".into(),
        evidence_refs: vec![],
        promoted: false,
        created_at: "t".into(),
    };
    store::save_finding(root, &f).unwrap();
    // 사용자가 떨군 비-Finding JSON + 손상 JSON
    std::fs::write(root.join("findings/notes.json"), b"{\"unrelated\": true}").unwrap();
    std::fs::write(root.join("findings/broken.json"), b"{ not json").unwrap();
    // strict는 에러
    assert!(store::walk_findings(root).is_err());
    // lenient는 유효 1개 + skipped 2개
    let (out, skipped) = store::walk_findings_lenient(root).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, "01F");
    assert_eq!(skipped.len(), 2);
}

#[test]
fn walk_roms_lenient_skips_corrupt() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_rom(
        root,
        &Rom {
            sha1: "sha_a".into(),
            platform: "snes".into(),
            title: None,
            first_seen: "t".into(),
        },
    )
    .unwrap();
    // 손상 rom.json
    let bad = root.join("roms/sha_b");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("rom.json"), b"{ broken").unwrap();
    assert!(store::walk_roms(root).is_err());
    let (out, skipped) = store::walk_roms_lenient(root).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(skipped.len(), 1);
}

#[test]
fn track_root_prefers_explicit_then_git_root_then_cwd() {
    use std::path::PathBuf;
    // 명시 override가 최우선
    assert_eq!(
        store::resolve_track_root(Some("/x/custom".into()), Some(PathBuf::from("/repo"))),
        PathBuf::from("/x/custom")
    );
    // override 없으면 git root의 .emucap (cwd가 roms/여도 commit 가능한 repo 루트)
    assert_eq!(
        store::resolve_track_root(None, Some(PathBuf::from("/repo/patch-proj"))),
        PathBuf::from("/repo/patch-proj/.emucap")
    );
    // git root도 없으면 cwd 상대 .emucap (폴백)
    assert_eq!(
        store::resolve_track_root(None, None),
        PathBuf::from(".emucap")
    );
}

#[test]
fn track_root_source_reflects_resolution_and_warns_only_on_cwd_fallback() {
    use super::store::TrackRootSource;
    use std::path::PathBuf;
    // 명시 override → env source, 경고 없음
    let (path, src) = store::resolve_track_root_with_source(
        Some("/x/custom".into()),
        Some(PathBuf::from("/repo")),
    );
    assert_eq!(path, PathBuf::from("/x/custom"));
    assert_eq!(src, TrackRootSource::Env);
    assert_eq!(src.as_str(), "env");
    assert!(src.warning().is_none());
    // git root → git_root source, 경고 없음
    let (path, src) =
        store::resolve_track_root_with_source(None, Some(PathBuf::from("/repo/proj")));
    assert_eq!(path, PathBuf::from("/repo/proj/.emucap"));
    assert_eq!(src, TrackRootSource::GitRoot);
    assert_eq!(src.as_str(), "git_root");
    assert!(src.warning().is_none());
    // git root 없음 → cwd_fallback source, 위치 모호 경고
    let (path, src) = store::resolve_track_root_with_source(None, None);
    assert_eq!(path, PathBuf::from(".emucap"));
    assert_eq!(src, TrackRootSource::CwdFallback);
    assert_eq!(src.as_str(), "cwd_fallback");
    assert!(src.warning().is_some());
}

#[test]
fn artifact_path_resolves_relative_against_git_root() {
    use std::path::{Path, PathBuf};
    // 절대경로는 그대로
    assert_eq!(
        store::resolve_artifact_path(Path::new("/abs/shot.png"), Some(Path::new("/repo"))),
        PathBuf::from("/abs/shot.png")
    );
    // 상대경로는 git root 기준(서버 cwd 비의존)
    assert_eq!(
        store::resolve_artifact_path(Path::new("docs/shot.png"), Some(Path::new("/repo/proj"))),
        PathBuf::from("/repo/proj/docs/shot.png")
    );
    // git root 없으면 상대 그대로(cwd 상대 폴백)
    assert_eq!(
        store::resolve_artifact_path(Path::new("docs/shot.png"), None),
        PathBuf::from("docs/shot.png")
    );
}
