use std::path::Path;

use anyhow::Context;

pub fn run(dir: &Path, rom: Option<&Path>) -> anyhow::Result<()> {
    let manifest = emucap::bundle::finalize::finalize(dir, rom)
        .with_context(|| format!("번들 확정 실패: {}", dir.display()))?;
    println!(
        "확정됨: {} (슬라이스 {}개, ROM {})",
        dir.display(),
        manifest.slices.len(),
        manifest.rom.sha1
    );
    Ok(())
}
