use super::*;
use crate::test_env::{lock_env, EnvGuard};

fn write_root(repo: &Path) -> PathBuf {
    let adapter = repo.join("adapters/mupen64plus");
    let root = adapter.join("work/mupen64plus-bundle-src-2.6.0/test");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        adapter.join("upstream.lock"),
        "M64P_VERSION=2.6.0\n\
         M64P_BUNDLE_URL=https://example.invalid/m64p.tar.gz\n\
         M64P_BUNDLE_SHA256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
         M64P_CORE_COMMIT=1111111111111111111111111111111111111111\n\
         M64P_TEST_ROM_SHA256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
         M64P_HOST_API=1\n",
    )
    .unwrap();
    for name in [
        "libmupen64plus.dylib",
        "mupen64plus-rsp-hle.dylib",
        "mupen64plus-video-rice.dylib",
    ] {
        std::fs::write(root.join(name), b"fixture").unwrap();
    }
    std::fs::write(
        root.join("emucap-mupen64plus-build.json"),
        serde_json::to_vec(&BuildMetadata {
            upstream: "https://example.invalid/m64p.tar.gz".into(),
            version: "2.6.0".into(),
            core_commit: "1111111111111111111111111111111111111111".into(),
            host_api: REQUIRED_HOST_API,
            bundle_sha256: "a".repeat(64),
            test_rom_sha256: "b".repeat(64),
            debugger: true,
        })
        .unwrap(),
    )
    .unwrap();
    root
}

#[test]
fn compatible_root_requires_pinned_metadata_and_display_plugin_only_when_visible() {
    let repo = tempfile::tempdir().unwrap();
    let root = write_root(repo.path());
    assert!(require_compatible_root(repo.path(), &root, true).is_ok());
    std::fs::remove_file(root.join("mupen64plus-video-rice.dylib")).unwrap();
    assert!(require_compatible_root(repo.path(), &root, false).is_ok());
    assert!(require_compatible_root(repo.path(), &root, true).is_err());
}

#[test]
fn launch_spec_isolated_and_headless_by_default() {
    let _lock = lock_env();
    let _guard = EnvGuard::new(&["EMUCAP_EMU_HOME"]);
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", temp.path().join("emulators"));
    let binary = temp.path().join("emucap-mupen64plus");
    let root = temp.path().join("m64p");
    let rom = temp.path().join("test.v64");
    let log = temp.path().join("n64.log");
    let runtime_home = emu_home_dir("mupen64plus", 47890);
    let launch = Launch {
        binary: &binary,
        repo_root: temp.path(),
        root: &root,
        content: &rom,
        log_path: &log,
        port: 47890,
        name: Some("n64-test"),
        session_token: Some("token"),
        build: Some("abc123"),
        runtime: None,
        display: false,
    };
    let spec = launch_spec(&launch, &runtime_home);
    assert_eq!(
        spec.args,
        vec![
            "47890",
            rom.to_str().unwrap(),
            root.to_str().unwrap(),
            runtime_home.to_str().unwrap()
        ]
    );
    assert!(spec
        .env
        .contains(&("EMUCAP_N64_DISPLAY".into(), "0".into())));
    assert!(spec
        .env
        .contains(&("EMUCAP_SESSION_TOKEN".into(), "token".into())));
    assert!(spec
        .env
        .contains(&("EMUCAP_BUILD_HASH".into(), "abc123".into())));
}

#[test]
fn content_validation_accepts_only_n64_cartridge_extensions() {
    let temp = tempfile::tempdir().unwrap();
    for extension in ["z64", "n64", "v64"] {
        let path = temp.path().join(format!("test.{extension}"));
        std::fs::write(&path, b"rom").unwrap();
        assert!(validate_content(&path).is_ok());
    }
    let zip = temp.path().join("test.zip");
    std::fs::write(&zip, b"rom").unwrap();
    assert!(validate_content(&zip).is_err());
}
