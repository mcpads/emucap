use super::state_diff::*;
use serde_json::json;
use std::collections::BTreeMap;

fn map(pairs: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn reports_differing_logic_keys() {
    let a = map(&[("ppu.bgMode", json!(1)), ("cpu.a", json!(16))]);
    let b = map(&[("ppu.bgMode", json!(3)), ("cpu.a", json!(16))]);
    let sd = state_diff(&a, &b, &[]);
    assert_eq!(sd.diffs.len(), 1);
    assert_eq!(sd.diffs[0].key, "ppu.bgMode");
    assert_eq!(sd.diffs[0].a, json!(1));
    assert_eq!(sd.diffs[0].b, json!(3));
}

#[test]
fn ignores_timing_noise_keys() {
    // frameCount·cycleCount·masterClock·scanline·spc.* 는 노이즈로 제외.
    let a = map(&[
        ("frameCount", json!(100)),
        ("cpu.cycleCount", json!(1000)),
        ("masterClock", json!(50)),
        ("ppu.scanline", json!(0)),
        ("spc.pc", json!(1380)),
        ("ppu.bgMode", json!(1)),
    ]);
    let b = map(&[
        ("frameCount", json!(200)),
        ("cpu.cycleCount", json!(2000)),
        ("masterClock", json!(99)),
        ("ppu.scanline", json!(50)),
        ("spc.pc", json!(9999)),
        ("ppu.bgMode", json!(1)),
    ]);
    let sd = state_diff(&a, &b, &[]);
    assert_eq!(sd.diffs.len(), 0, "타이밍·사운드 차이는 모두 노이즈");
    assert!(sd.ignored >= 5);
}

#[test]
fn extra_ignore_filters_additional_keys() {
    let a = map(&[
        ("ppu.bgMode", json!(1)),
        ("dmaController.channel[0].srcAddress", json!(10)),
    ]);
    let b = map(&[
        ("ppu.bgMode", json!(2)),
        ("dmaController.channel[0].srcAddress", json!(20)),
    ]);
    let sd = state_diff(&a, &b, &["bgmode".into()]);
    assert_eq!(sd.diffs.len(), 1);
    assert_eq!(sd.diffs[0].key, "dmaController.channel[0].srcAddress");
}

#[test]
fn reports_keys_present_on_one_side() {
    let a = map(&[("cpu.a", json!(1)), ("cpu.newReg", json!(5))]);
    let b = map(&[("cpu.a", json!(1))]);
    let sd = state_diff(&a, &b, &[]);
    assert!(sd.only_in_a.contains(&"cpu.newReg".to_string()));
    assert!(sd.only_in_b.is_empty());
}
