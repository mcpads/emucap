use super::diff::*;

fn set(name: &str, base: u64, bytes: &[u8]) -> RegionSet {
    let mut s = RegionSet::new();
    s.insert(name, base, bytes.to_vec());
    s
}

#[test]
fn identical_regions_have_no_diff() {
    let a = set("wram", 0x7e0000, &[1, 2, 3, 4]);
    let b = set("wram", 0x7e0000, &[1, 2, 3, 4]);
    let r = diff(&a, &b, &[], &Baseline::empty());
    assert_eq!(r.regions[0].differing, 0);
    assert!(r.regions[0].first_divergence.is_none());
}

#[test]
fn one_byte_diff_reports_first_divergence() {
    let a = set("wram", 0x7e0000, &[1, 2, 3, 4]);
    let b = set("wram", 0x7e0000, &[1, 2, 9, 4]);
    let r = diff(&a, &b, &[], &Baseline::empty());
    assert_eq!(r.regions[0].differing, 1);
    let d = r.regions[0].first_divergence.as_ref().unwrap();
    assert_eq!(d.offset, 2);
    assert_eq!(d.address, 0x7e0002);
    assert_eq!(d.a, 3);
    assert_eq!(d.b, 9);
}

#[test]
fn diff_does_not_overflow_on_huge_base_address() {
    // base_address가 u64 끝 근처면 base_address + offset이 오버플로한다(debug 패닉,
    // release wrap). saturating으로 패닉/wrap 없이 처리해야 한다.
    let a = set("r", u64::MAX, &[1, 2]);
    let b = set("r", u64::MAX, &[1, 9]); // offset 1 다름
    let r = diff(&a, &b, &[], &Baseline::empty());
    let d = r.regions[0].first_divergence.as_ref().unwrap();
    assert_eq!(d.offset, 1);
    assert_eq!(
        d.address,
        u64::MAX,
        "base+offset 오버플로를 saturating으로 처리해야"
    );
}

#[test]
fn ignore_excludes_range() {
    let a = set("wram", 0, &[1, 2, 3, 4]);
    let b = set("wram", 0, &[1, 9, 9, 4]); // offset 1,2 다름
    let ig = vec![IgnoreSpec {
        region: "wram".into(),
        start: 1,
        end: 3,
    }];
    let r = diff(&a, &b, &ig, &Baseline::empty());
    assert_eq!(r.regions[0].differing, 0, "ignore된 1..3의 차이는 제외");
    assert!(r.regions[0].first_divergence.is_none());
}

#[test]
fn first_divergence_skips_ignored() {
    let a = set("wram", 0, &[1, 2, 3, 4]);
    let b = set("wram", 0, &[9, 2, 9, 4]); // offset 0, 2 다름
    let ig = vec![IgnoreSpec {
        region: "wram".into(),
        start: 0,
        end: 1,
    }];
    let r = diff(&a, &b, &ig, &Baseline::empty());
    let d = r.regions[0].first_divergence.as_ref().unwrap();
    assert_eq!(d.offset, 2, "offset 0은 ignore, 첫 분기점은 2");
}

#[test]
fn size_mismatch_compares_common_length() {
    let a = set("vram", 0, &[1, 2, 3, 4, 5]);
    let b = set("vram", 0, &[1, 2, 3]);
    let r = diff(&a, &b, &[], &Baseline::empty());
    assert_eq!(r.regions[0].compared, 3);
    assert_eq!(r.regions[0].a_len, 5);
    assert_eq!(r.regions[0].b_len, 3);
    assert_eq!(r.regions[0].differing, 0);
}

#[test]
fn unmatched_region_reported() {
    let a = set("wram", 0, &[1]);
    let b = set("cram", 0, &[1]);
    let r = diff(&a, &b, &[], &Baseline::empty());
    assert!(r.regions.is_empty());
    assert!(r.unmatched.contains(&"wram".to_string()));
    assert!(r.unmatched.contains(&"cram".to_string()));
}

#[test]
fn parse_ignore_ok_and_err() {
    let i = parse_ignore("wram:256-512").unwrap();
    assert_eq!(
        i,
        IgnoreSpec {
            region: "wram".into(),
            start: 256,
            end: 512
        }
    );
    assert!(parse_ignore("wram256-512").is_err());
    assert!(parse_ignore("wram:512-256").is_err());
}

#[test]
fn divergences_lists_all_offsets() {
    let a = set("wram", 0, &[1, 2, 3, 4]);
    let b = set("wram", 0, &[9, 2, 9, 4]); // offset 0, 2 다름
    let r = diff(&a, &b, &[], &Baseline::empty());
    assert_eq!(r.regions[0].divergences, vec![0, 2]);
}

#[test]
fn baseline_subtracts_expected_divergences() {
    // 정상 지점: A_good vs B_good → offset 1이 "예상 차이"(패치가 의도적으로 바꾼 곳).
    let ag = set("wram", 0, &[0, 0, 0, 0]);
    let bg = set("wram", 0, &[0, 7, 0, 0]);
    let baseline = Baseline::from_report(&diff(&ag, &bg, &[], &Baseline::empty()));

    // 버그 지점: offset 1(예상) + offset 3(새 버그)이 다름.
    let ab = set("wram", 0, &[0, 0, 0, 0]);
    let bb = set("wram", 0, &[0, 7, 0, 5]);
    let r = diff(&ab, &bb, &[], &baseline);
    assert_eq!(
        r.regions[0].divergences,
        vec![3],
        "예상 차이(1)는 빠지고 새 차이(3)만"
    );
    assert_eq!(r.regions[0].first_divergence.as_ref().unwrap().offset, 3);
}

#[test]
fn render_json_is_machine_readable() {
    let a = set("wram", 0, &[1, 2]);
    let b = set("wram", 0, &[1, 9]);
    let json = render_json(&diff(&a, &b, &[], &Baseline::empty()));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["regions"][0]["differing"], 1);
    assert_eq!(v["regions"][0]["first_divergence"]["offset"], 1);
}
