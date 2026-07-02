use super::model::*;
use super::{index, query, store};
use tempfile::TempDir;

fn run(id: &str, rom: &str, goal: &str) -> Run {
    Run {
        format_version: RUN_FORMAT_VERSION,
        id: id.into(),
        rom_sha1: rom.into(),
        goal: Some(goal.into()),
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
        repro_status: Some(ReproStatus::ReplayableWithInterventions),
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    }
}

#[test]
fn query_runs_filters_by_rom_and_goal() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    store::save_run(root, &run("01A", "sha_a", "font")).unwrap();
    store::save_run(root, &run("01B", "sha_a", "text")).unwrap();
    store::save_run(root, &run("01C", "sha_b", "font")).unwrap();
    let conn = index::open_index(&root.join("index.sqlite")).unwrap();
    index::reindex(root, &conn).unwrap();

    let all = query::query_runs(
        &conn,
        &query::RunFilter {
            rom_sha1: Some("sha_a".into()),
            goal: None,
            status: None,
        },
    )
    .unwrap();
    assert_eq!(all.len(), 2);

    let font = query::query_runs(
        &conn,
        &query::RunFilter {
            rom_sha1: Some("sha_a".into()),
            goal: Some("font".into()),
            status: None,
        },
    )
    .unwrap();
    assert_eq!(font.len(), 1);
    assert_eq!(font[0].id, "01A");
}
