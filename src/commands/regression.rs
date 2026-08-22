use std::path::Path;

use emucap::analysis::bisect::{CmpOp, Predicate};
use emucap::analysis::regression::{save_case, Case, Expect, Repro, RomRef, CASE_FORMAT_VERSION};

/// memory_type:address:length:op:value 파싱.
fn parse_predicate(s: &str) -> Result<Predicate, String> {
    let p: Vec<&str> = s.split(':').collect();
    if p.len() != 5 {
        return Err("predicate는 memory_type:address:length:op:value".into());
    }
    // MCP와 같은 파서(0x/$ 16진 수용) — CLI만 10진을 강요하던 불일치 해소(#45).
    let length = emucap::numparse::parse_num_str(p[2]).map_err(|e| format!("length: {e}"))?;
    if length == 0 || length > 8 {
        return Err(format!("length는 1~8: {length}"));
    }
    Ok(Predicate {
        memory_type: p[0].into(),
        address: emucap::numparse::parse_num_str(p[1]).map_err(|e| format!("address: {e}"))?,
        length,
        op: CmpOp::parse(p[3])?,
        value: emucap::numparse::parse_num_str(p[4]).map_err(|e| format!("value: {e}"))?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn add(
    suite_dir: &Path,
    id: &str,
    desc: &str,
    from_savestate: Option<&Path>,
    advance: u64,
    from_input: Option<&Path>,
    start: Option<&Path>,
    anchor: Option<&str>,
    predicate: &str,
    rom: &Path,
    expect: &str,
) -> anyhow::Result<()> {
    if !emucap::path_safety::is_hyphenated_ascii_id(id, 96) {
        anyhow::bail!("id must use ASCII alphanumeric segments separated by single hyphens");
    }
    let dir = suite_dir.join(id);
    if std::fs::symlink_metadata(&dir)
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        anyhow::bail!("case directory is a symlink: {}", dir.display());
    }
    if dir.join("case.json").exists() {
        anyhow::bail!("id 충돌: {id} 이미 존재");
    }
    let pred = parse_predicate(predicate).map_err(|e| anyhow::anyhow!("{e}"))?;
    let expect = match expect {
        "absent" => Expect::Absent,
        "present" => Expect::Present,
        _ => anyhow::bail!("expect는 absent|present"),
    };
    let sha1 = emucap::rom::sha1_of_file(rom)?;
    std::fs::create_dir_all(&dir)?;

    let repro = match (from_savestate, from_input) {
        (Some(mss), None) => {
            let state_sha1 = emucap::rom::sha1_of_file(mss)?;
            emucap::path_safety::atomic_copy_file(mss, &dir.join(format!("{state_sha1}.mss")))?;
            Repro::Savestate {
                state_sha1,
                advance_frames: advance,
            }
        }
        (None, Some(movie)) => {
            emucap::path_safety::atomic_copy_file(movie, &dir.join("inputs.movie"))?;
            let start = match start {
                None => "reset".to_string(),
                Some(s) => {
                    // savestate 케이스와 동일하게 start 베이스 .mss도 케이스 디렉토리로
                    // 복사한다 — 안 그러면 러너가 {sha1}.mss를 못 찾아 항상 MissingPayload.
                    let h = emucap::rom::sha1_of_file(s)?;
                    emucap::path_safety::atomic_copy_file(s, &dir.join(format!("{h}.mss")))?;
                    h
                }
            };
            let anchor_pred = match anchor {
                Some(s) => Some(parse_predicate(s).map_err(|e| anyhow::anyhow!("{e}"))?),
                None => None,
            };
            Repro::InputReplay {
                start,
                movie: "inputs.movie".into(),
                anchor: anchor_pred,
            }
        }
        _ => anyhow::bail!("--from-savestate 또는 --from-input 중 하나만"),
    };

    let case = Case {
        format_version: CASE_FORMAT_VERSION,
        id: id.into(),
        description: desc.into(),
        rom: RomRef {
            sha1,
            path_hint: rom.display().to_string(),
        },
        repro,
        predicate: pred,
        expect,
    };
    save_case(&dir, &case)?;
    println!("케이스 추가: {}", dir.display());
    Ok(())
}

#[cfg(test)]
#[path = "regression_tests.rs"]
mod tests;
