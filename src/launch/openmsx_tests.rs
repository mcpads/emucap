use super::*;
use crate::test_env::{lock_env, EnvGuard};

#[cfg(unix)]
fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, b"fixture").unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(windows)]
fn make_runnable(path: &Path) {
    std::fs::write(path, b"fixture").unwrap();
}

fn write_pinned_build(repo: &Path) -> PathBuf {
    let adapter = repo.join("adapters/openmsx");
    let binary_dir = adapter.join("work/openmsx-21.0/derived/test/openMSX.app/Contents/MacOS");
    std::fs::create_dir_all(&binary_dir).unwrap();
    std::fs::write(
        adapter.join("upstream.lock"),
        "OPENMSX_VERSION=21.0\n\
         OPENMSX_URL=https://example.invalid/openmsx.tar.gz\n\
         OPENMSX_SHA256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
         OPENMSX_SDL2_COMPAT_PATCH_URL=https://example.invalid/compat.patch\n\
         OPENMSX_SDL2_COMPAT_PATCH_SHA256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
         OPENMSX_EMUCAP_PATCH_SHA256=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\n\
         OPENMSX_FRAME_PROBE_PATCH_SHA256=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\n\
         OPENMSX_HOST_API=3\n",
    )
    .unwrap();
    let binary = binary_dir.join(if cfg!(windows) {
        "openmsx.exe"
    } else {
        "openmsx"
    });
    make_runnable(&binary);
    std::fs::write(
        binary_dir.join("emucap-openmsx-build.json"),
        serde_json::to_vec(&BuildMetadata {
            upstream: "https://example.invalid/openmsx.tar.gz".into(),
            version: "21.0".into(),
            host_api: REQUIRED_HOST_API,
            archive_sha256: "a".repeat(64),
            sdl2_compat_patch_sha256: "b".repeat(64),
            emucap_patch_sha256: "c".repeat(64),
            frame_probe_patch_sha256: "d".repeat(64),
            native_patch: true,
        })
        .unwrap(),
    )
    .unwrap();
    binary
}

#[test]
fn compatible_build_requires_exact_pinned_sidecar_and_native_patch() {
    let repo = tempfile::tempdir().unwrap();
    let binary = write_pinned_build(repo.path());
    assert!(require_compatible_build(repo.path(), &binary).is_ok());

    let sidecar = binary.parent().unwrap().join("emucap-openmsx-build.json");
    let mut metadata: BuildMetadata =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    metadata.native_patch = false;
    std::fs::write(sidecar, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(require_compatible_build(repo.path(), &binary).is_err());
}

#[test]
fn resolver_finds_only_repo_build_or_explicit_override() {
    let _lock = lock_env();
    let _guard = EnvGuard::new(&["EMUCAP_OPENMSX_BIN"]);
    std::env::remove_var("EMUCAP_OPENMSX_BIN");
    let repo = tempfile::tempdir().unwrap();
    let binary = write_pinned_build(repo.path());
    assert_eq!(resolve_binary(repo.path()), Some(binary.clone()));

    let override_binary = repo.path().join(if cfg!(windows) {
        "custom.exe"
    } else {
        "custom"
    });
    make_runnable(&override_binary);
    std::env::set_var("EMUCAP_OPENMSX_BIN", &override_binary);
    assert_eq!(resolve_binary(repo.path()), Some(override_binary));
}

#[test]
fn launch_spec_isolates_and_passes_exact_process_identity_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("openmsx");
    let bridge = temp.path().join("emucap-openmsx-bridge");
    let rom = temp.path().join("game.rom");
    let log = temp.path().join("openmsx.log");
    let runtime_home = temp.path().join("runtime");
    let pid_file = runtime_home.join("emulator.pid");
    let launch = Launch {
        binary: &binary,
        bridge: &bridge,
        repo_root: temp.path(),
        system: "msx",
        content: &rom,
        log_path: &log,
        port: 47901,
        name: Some("msx-test"),
        session_token: Some("token"),
        build: Some("abc123"),
        runtime: None,
        display: false,
    };
    let session_manifest = runtime_home.join("generation/session.json");
    let spec = launch_spec(&launch, &session_manifest, &runtime_home, &pid_file);
    assert_eq!(
        spec.args,
        vec![
            "47901",
            binary.to_str().unwrap(),
            session_manifest.to_str().unwrap(),
            runtime_home.to_str().unwrap(),
            "0",
            pid_file.to_str().unwrap(),
        ]
    );
    assert!(spec
        .env
        .contains(&("EMUCAP_SESSION_TOKEN".into(), "token".into())));
    assert!(spec
        .env
        .contains(&("EMUCAP_BUILD_HASH".into(), "abc123".into())));
    assert!(spec.env.contains(&("EMUCAP_SYSTEM".into(), "msx".into())));
}

#[test]
fn content_validation_accepts_only_first_profile_cartridges() {
    let temp = tempfile::tempdir().unwrap();
    for extension in ["rom", "mx1", "mx2", "ri", "sg"] {
        let path = temp.path().join(format!("test.{extension}"));
        std::fs::write(&path, b"rom").unwrap();
        assert_eq!(
            validate_content_for_profile(OpenMsxProfile::CbiosMsx2p, &path).unwrap(),
            MediaKind::Cartridge,
            ".{extension}"
        );
    }
    for extension in ["dsk", "cas", "zip"] {
        let path = temp.path().join(format!("test.{extension}"));
        std::fs::write(&path, b"media").unwrap();
        assert!(
            validate_content_for_profile(OpenMsxProfile::CbiosMsx2p, &path).is_err(),
            ".{extension}"
        );
    }
}

#[test]
fn real_machine_profiles_and_media_are_not_aliases() {
    let cases = [
        ("msx", OpenMsxProfile::CbiosMsx2p, "C-BIOS_MSX2+", "MSX2+"),
        ("msx1", OpenMsxProfile::Msx1, "Philips_VG_8020", "MSX"),
        ("msx2", OpenMsxProfile::Msx2, "Philips_NMS_8250", "MSX2"),
        (
            "msx2p",
            OpenMsxProfile::Msx2p,
            "Panasonic_FS-A1WSX",
            "MSX2+",
        ),
        (
            "msxtr",
            OpenMsxProfile::MsxTurboR,
            "Panasonic_FS-A1GT",
            "MSXturboR",
        ),
    ];
    for (system, profile, machine, machine_type) in cases {
        assert_eq!(OpenMsxProfile::for_system(system), Some(profile));
        assert_eq!(profile.system(), system);
        assert_eq!(profile.machine(), machine);
        assert_eq!(profile.machine_type(), machine_type);
    }
    assert!(OpenMsxProfile::for_system("msx2+").is_none());

    assert!(OpenMsxProfile::Msx1.supports(MediaKind::Cassette));
    assert!(!OpenMsxProfile::Msx1.supports(MediaKind::Disk));
    for profile in [OpenMsxProfile::Msx2, OpenMsxProfile::Msx2p] {
        assert!(profile.supports(MediaKind::Cartridge));
        assert!(profile.supports(MediaKind::Disk));
        assert!(profile.supports(MediaKind::Cassette));
    }
    assert!(OpenMsxProfile::MsxTurboR.supports(MediaKind::Disk));
    assert!(!OpenMsxProfile::MsxTurboR.supports(MediaKind::Cassette));
}

#[test]
fn firmware_inventory_uses_content_identity_and_canonical_staging_name() {
    use sha1::{Digest, Sha1};

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("nested/arbitrary-name.bin");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"firmware-fixture").unwrap();
    let sha1 = hex::encode(Sha1::digest(b"firmware-fixture"));
    let requirement = FirmwareRequirement {
        canonical_name: "canonical.rom",
        accepted_sha1: vec![sha1.clone()],
    };
    let resolved = resolve_firmware_inventory(temp.path(), &[requirement]).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].source, source);
    assert_eq!(resolved[0].canonical_name, "canonical.rom");
    assert_eq!(resolved[0].sha1, sha1);
}

#[test]
fn firmware_inventory_rejects_two_different_accepted_images() {
    use sha1::{Digest, Sha1};

    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("one.rom"), b"one").unwrap();
    std::fs::write(temp.path().join("two.rom"), b"two").unwrap();
    let requirement = FirmwareRequirement {
        canonical_name: "machine.rom",
        accepted_sha1: vec![
            hex::encode(Sha1::digest(b"one")),
            hex::encode(Sha1::digest(b"two")),
        ],
    };
    let error = resolve_firmware_inventory(temp.path(), &[requirement]).unwrap_err();
    assert!(error
        .to_string()
        .contains("multiple accepted firmware identities"));
}

#[test]
fn mutable_media_is_copied_and_source_identity_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.dsk");
    let working = temp.path().join("session/media/disk-a.dsk");
    std::fs::write(&source, b"disk-source").unwrap();
    let prepared = prepare_media(MediaKind::Disk, &source, &working).unwrap();
    assert_ne!(prepared.source_path, prepared.mounted_path);
    assert_eq!(prepared.source_sha1, prepared.mounted_sha1);

    std::fs::write(&prepared.mounted_path, b"guest-write").unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), b"disk-source");
}

#[test]
fn firmware_root_must_be_absolute_before_inventory() {
    let error = validate_firmware_root(Path::new("relative/firmware")).unwrap_err();
    assert!(error.to_string().contains("absolute"));
}
