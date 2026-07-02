use std::path::Path;

use anyhow::Context;

use emucap::bundle::manifest::Manifest;
use emucap::bundle::summary::{render_json, render_table, summarize};

pub fn run(dir: &Path, json: bool) -> anyhow::Result<()> {
    let manifest_path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "manifest.json 읽기 실패(먼저 finalize 하세요): {}",
            manifest_path.display()
        )
    })?;
    let manifest: Manifest = serde_json::from_str(&text).context("manifest.json 파싱 실패")?;
    let summary = summarize(&manifest);
    if json {
        println!("{}", render_json(&summary));
    } else {
        print!("{}", render_table(&summary));
    }
    Ok(())
}
