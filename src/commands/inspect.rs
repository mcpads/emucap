use std::path::Path;

use anyhow::Context;

use emucap::bundle::manifest::parse_manifest;
use emucap::bundle::summary::{render_bundle_json, render_bundle_table, summarize_bundle};

pub fn run(dir: &Path, json: bool) -> anyhow::Result<()> {
    let manifest_path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "manifest.json 읽기 실패(먼저 finalize 하세요): {}",
            manifest_path.display()
        )
    })?;
    let manifest = parse_manifest(&text).context("failed to parse manifest.json")?;
    let summary = summarize_bundle(&manifest);
    if json {
        println!("{}", render_bundle_json(&summary));
    } else {
        print!("{}", render_bundle_table(&summary));
    }
    Ok(())
}
