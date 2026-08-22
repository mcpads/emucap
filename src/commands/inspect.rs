use std::path::Path;

use anyhow::Context;

use emucap::bundle::manifest::parse_manifest;
use emucap::bundle::summary::{render_bundle_json, render_bundle_table, summarize_bundle};

pub fn run(dir: &Path, json: bool) -> anyhow::Result<()> {
    let manifest_path = dir.join("manifest.json");
    let bytes =
        emucap::path_safety::read_bounded_regular_member(dir, "manifest.json", 16 * 1024 * 1024)
            .with_context(|| {
                format!(
                    "manifest.json 읽기 실패(먼저 finalize 하세요): {}",
                    manifest_path.display()
                )
            })?;
    let text = std::str::from_utf8(&bytes).context("manifest.json is not UTF-8")?;
    let manifest = parse_manifest(text).context("failed to parse manifest.json")?;
    let summary = summarize_bundle(&manifest);
    if json {
        println!("{}", render_bundle_json(&summary));
    } else {
        print!("{}", render_bundle_table(&summary));
    }
    Ok(())
}
