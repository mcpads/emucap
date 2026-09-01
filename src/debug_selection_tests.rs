use super::*;
use serde_json::json;

fn targets() -> Vec<CpuTarget> {
    vec![
        CpuTarget {
            id: "arm9".into(),
            aliases: vec!["main".into()],
            default: true,
            disassembly_modes: vec!["auto".into(), "arm".into(), "thumb".into()],
        },
        CpuTarget {
            id: "arm7".into(),
            aliases: vec![],
            default: false,
            disassembly_modes: vec!["auto".into(), "arm".into(), "thumb".into()],
        },
    ]
}

#[test]
fn cpu_alias_resolves_to_the_advertised_target() {
    assert_eq!(
        resolve_cpu_target(&targets(), Some("MAIN")).unwrap(),
        Some("arm9".into())
    );
    assert_eq!(
        resolve_cpu_target(&targets(), Some("arm7")).unwrap(),
        Some("arm7".into())
    );
    assert!(resolve_cpu_target(&targets(), Some("rsp")).is_err());
}

#[test]
fn disassembly_mode_is_bound_to_the_selected_target() {
    assert_eq!(
        resolve_disassembly_mode(&targets(), Some("arm7"), Some("THUMB")).unwrap(),
        Some("thumb".into())
    );
    assert!(resolve_disassembly_mode(&targets(), Some("arm9"), Some("mips")).is_err());
}

#[test]
fn prefixed_state_is_projected_and_reports_canonical_groups() {
    let response = json!({
        "state": {
            "cpu.pc": 1,
            "cpu.sp": 2,
            "ppu.scanline": 3,
            "dma[0].source": 4
        },
        "frame": 9
    });
    let projected = project_state_groups(
        response,
        &["PPU".into(), "dma".into()],
        &["cpu".into(), "ppu".into(), "dma".into()],
    )
    .unwrap();
    assert_eq!(projected["groups_applied"], json!(["ppu", "dma"]));
    assert_eq!(
        projected["state"],
        json!({"ppu.scanline":3, "dma[0].source":4})
    );
    assert_eq!(projected["frame"], 9);
}

#[test]
fn advertised_scalar_state_key_is_a_projectable_group() {
    let projected = project_state_groups(
        json!({
            "state": {"frameCount": 17},
            "groups_applied": ["frameCount"]
        }),
        &["framecount".into()],
        &["cpu".into(), "frameCount".into(), "ppu".into()],
    )
    .unwrap();

    assert_eq!(projected["state"], json!({"frameCount": 17}));
    assert_eq!(projected["groups_applied"], json!(["frameCount"]));
}

#[test]
fn unprefixed_single_cpu_state_is_projected_as_cpu() {
    let projected = project_state_groups(
        json!({"cpu":"r4300", "state":{"pc":1,"r0":2}, "frame":7}),
        &["cpu".into()],
        &["cpu".into()],
    )
    .unwrap();
    assert_eq!(projected["state"], json!({"pc":1,"r0":2}));
    assert_eq!(projected["groups_applied"], json!(["cpu"]));
}

#[test]
fn top_level_register_group_is_projected_without_dropping_metadata() {
    let projected = project_state_groups(
        json!({"M68K":{"PC":1}, "Z80":{"PC":2}, "frame":3}),
        &["m68k".into()],
        &["M68K".into(), "Z80".into()],
    )
    .unwrap();
    assert_eq!(projected["M68K"], json!({"PC":1}));
    assert!(projected.get("Z80").is_none());
    assert_eq!(projected["frame"], 3);
    assert_eq!(projected["groups_applied"], json!(["M68K"]));
}

#[test]
fn capability_metadata_rejects_ambiguous_aliases_and_defaults() {
    let duplicate = json!({
        "state_groups":["cpu"],
        "cpu_targets":[
            {"id":"arm9","aliases":["main"],"default":true,"disassembly_modes":["auto"]},
            {"id":"arm7","aliases":["MAIN"],"default":false,"disassembly_modes":["auto"]}
        ]
    });
    assert!(parse_debug_capabilities(&duplicate).is_err());
    let no_default = json!({
        "cpu_targets":[{"id":"main","default":false,"disassembly_modes":[]}]
    });
    assert!(parse_debug_capabilities(&no_default).is_err());
}
