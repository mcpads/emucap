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
    // id는 스위트 내부의 단일 디렉토리명이어야 한다 — 슬래시·`..` 등으로 디렉토리를
    // 탈출하면 거부한다.
    let id_is_safe = {
        use std::path::Component;
        let mut comps = Path::new(id).components();
        matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
    };
    if !id_is_safe {
        anyhow::bail!("id는 경로를 벗어나지 않는 단일 이름이어야: {id}");
    }
    let dir = suite_dir.join(id);
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
            std::fs::copy(mss, dir.join(format!("{state_sha1}.mss")))?;
            Repro::Savestate {
                state_sha1,
                advance_frames: advance,
            }
        }
        (None, Some(movie)) => {
            std::fs::copy(movie, dir.join("inputs.movie"))?;
            let start = match start {
                None => "reset".to_string(),
                Some(s) => {
                    // savestate 케이스와 동일하게 start 베이스 .mss도 케이스 디렉토리로
                    // 복사한다 — 안 그러면 러너가 {sha1}.mss를 못 찾아 항상 MissingPayload.
                    let h = emucap::rom::sha1_of_file(s)?;
                    std::fs::copy(s, dir.join(format!("{h}.mss")))?;
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
mod tests {
    use super::parse_predicate;

    #[test]
    fn parse_predicate_accepts_hex_like_mcp() {
        // #45: CLI도 0x/$ 16진을 받아야(MCP와 동일). address·value 16진, length 10진.
        let p = parse_predicate("wram:0x7e0010:2:eq:$1234").unwrap();
        assert_eq!(p.address, 0x7e0010);
        assert_eq!(p.value, 0x1234);
        assert_eq!(p.length, 2);
        // 10진도 여전히 동작
        let d = parse_predicate("wram:100:1:eq:5").unwrap();
        assert_eq!(d.address, 100);
        assert_eq!(d.value, 5);
    }
}
