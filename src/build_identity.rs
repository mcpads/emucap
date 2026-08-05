/// Source revision embedded by `build.rs`. Release and runtime-proof binaries are built only from
/// a clean committed revision; `-dirty` remains visible for local development builds.
pub const BUILD_HASH: &str = include_str!(concat!(env!("OUT_DIR"), "/emucap_build_hash"));
