use super::*;

#[test]
fn native_raster_must_be_the_last_completed_frame_with_an_explicit_domain() {
    let boundary = parse_boundary("1945 725 1944 6d616368696e6531 1 16384").unwrap();
    assert_eq!(
        boundary,
        RasterBoundary {
            frame: 1945,
            cycle: 725,
            raster: 1944,
            machine: "machine1".into(),
            pc: 16384,
        }
    );
    // Stability alone does not authorize an old, partial, or absent raster.
    for value in [
        "1945 725 1933 6d616368696e6531 1 16384",
        "1945 725 1945 6d616368696e6531 1 16384",
        "1945 725 -1 6d616368696e6531 1 16384",
        "1945 725 1944 6d616368696e6531 0 16384",
        "1945 725 1944 6d616368696e6531",
        "1945 725 1944 zz 1 16384",
        "0 0 18446744073709551615 6d31 1 16384",
    ] {
        assert!(parse_boundary(value).is_err(), "{value}");
    }
}
