use std::path::Path;

use anyhow::Context;

use emucap::bundle::manifest::Manifest;
use emucap::track::id::{IdGen, UlidGen};
use emucap::track::model::*;
use emucap::track::{index, query, repro, store};

pub fn reindex() -> anyhow::Result<()> {
    let root = store::root_from_env();
    let conn = index::open_index(&root.join("index.sqlite")).context("인덱스 열기 실패")?;
    let n = index::reindex(&root, &conn).context("reindex 실패")?;
    println!("reindexed: runs {n}");
    Ok(())
}

pub fn import(bundle: &Path) -> anyhow::Result<()> {
    let root = store::root_from_env();
    let gen = UlidGen;
    let text = std::fs::read_to_string(bundle.join("manifest.json"))
        .with_context(|| format!("manifest.json 읽기 실패: {}", bundle.display()))?;
    let manifest: Manifest = serde_json::from_str(&text).context("manifest 파싱 실패")?;

    // rom.json 보장(있으면 first_seen·title 보존 — create_run과 동일 불변식). NotFound만 생성
    // 트리거로 좁힌다 — 손상·IO를 '없음'으로 오인해 기존 first_seen을 조용히 덮어쓰지 않는다.
    match store::load_rom(&root, &manifest.rom.sha1) {
        Ok(_) => {}
        Err(store::TrackError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            store::save_rom(
                &root,
                &Rom {
                    sha1: manifest.rom.sha1.clone(),
                    platform: manifest.platform.clone(),
                    title: None,
                    first_seen: emucap::track::clock::now_rfc3339(),
                },
            )?;
        }
        Err(e) => return Err(e.into()),
    }

    let run = Run {
        format_version: RUN_FORMAT_VERSION,
        id: gen.new_id(),
        rom_sha1: manifest.rom.sha1.clone(),
        goal: Some("imported".into()),
        description: Some(format!("imported from {}", bundle.display())),
        tags: vec!["imported".into()],
        status: RunStatus::Done,
        started_at: emucap::track::clock::now_rfc3339(),
        ended_at: Some(emucap::track::clock::now_rfc3339()),
        agent: None,
        session: None,
        connection_ref: None,
        repro_base: Some("reset".into()),
        repro_movie_ref: None,
        repro_status: Some(repro::derive_status(&[])),
        gates: vec![],
        metrics: vec![],
        artifacts: vec![],
        interventions: vec![],
    };
    store::save_run(&root, &run)?;
    println!("imported run {} (rom {})", run.id, run.rom_sha1);
    Ok(())
}

pub fn ls(rom: Option<&str>, goal: Option<&str>) -> anyhow::Result<()> {
    let root = store::root_from_env();
    let conn = index::open_index(&root.join("index.sqlite"))?;
    let (_, skipped) = index::reindex_lenient(&root, &conn)?;
    let rows = query::query_runs(
        &conn,
        &query::RunFilter {
            rom_sha1: rom.map(String::from),
            goal: goal.map(String::from),
            status: None,
        },
    )?;
    println!("runs: {}", rows.len());
    if !skipped.is_empty() {
        println!("skipped (손상/이질 JSON): {}", skipped.len());
    }
    for r in &rows {
        println!(
            "{}  rom={}  goal={}  status={}  repro={}",
            r.id,
            r.rom_sha1,
            r.goal.as_deref().unwrap_or("-"),
            r.status,
            r.repro_status.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

pub fn show(rom: &str, run_id: &str) -> anyhow::Result<()> {
    let root = store::root_from_env();
    let run = store::load_run(&root, rom, run_id)
        .with_context(|| format!("run 로드 실패: {rom}/{run_id}"))?;
    println!("{}", serde_json::to_string_pretty(&run)?);
    Ok(())
}

pub fn compare(id_a: &str, id_b: &str) -> anyhow::Result<()> {
    let root = emucap::track::store::root_from_env();
    let cmp = emucap::track::compare::compare_runs(&root, id_a, id_b)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{}", serde_json::to_string_pretty(&cmp)?);
    Ok(())
}

pub fn summarize(goal: Option<&str>, tag: Option<&str>, rom: Option<&str>) -> anyhow::Result<()> {
    let root = emucap::track::store::root_from_env();
    let filter = emucap::track::summary::SummaryFilter {
        goal: goal.map(String::from),
        tag: tag.map(String::from),
        rom_sha1: rom.map(String::from),
    };
    let s = emucap::track::summary::summarize_runs(&root, &filter)?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}
