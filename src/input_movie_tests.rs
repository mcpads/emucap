use super::input_movie::*;

#[test]
fn parser_preserves_existing_sparse_regression_shape() {
    let movie = parse_movie("10:left,a\n12:\n15:right").unwrap();
    assert_eq!(movie.frames.len(), 3);
    assert_eq!(movie.frames[0].frame, 10);
    assert_eq!(movie.frames[0].buttons, ["left", "a"]);
    assert!(movie.frames[1].buttons.is_empty());
}

#[test]
fn recording_movie_is_dense_and_canonical() {
    let movie = canonical_recording_movie("1:B,A\n0:Right\n2:\n", 3, 4).unwrap();
    assert_eq!(movie.bytes, b"0:right\n1:a,b\n2:\n");
    assert_eq!(movie.movie.frames[1].buttons, ["a", "b"]);
}

#[test]
fn recording_movie_rejects_sparse_duplicate_and_invalid_buttons() {
    assert!(canonical_recording_movie("0:a\n2:b\n", 2, 4).is_err());
    assert!(canonical_recording_movie("0:a,a\n", 1, 4).is_err());
    assert!(canonical_recording_movie("0:a/b\n", 1, 4).is_err());
    assert!(canonical_recording_movie("0:a,b\n", 1, 1).is_err());
}

#[test]
fn recording_movie_requires_one_row_per_admitted_frame() {
    assert!(canonical_recording_movie("", 1, 4).is_err());
    assert!(canonical_recording_movie("0:\n1:\n", 1, 4).is_err());
}
