use super::*;

#[test]
fn pinned_lock_matches_the_build_contract() {
    assert_eq!(
        lock_value("NP2KAI_BUILD_PROFILE"),
        Some("license-clean-libretro")
    );
    assert_eq!(lock_value("NP2KAI_COMMIT").map(str::len), Some(40));
    assert_eq!(lock_value("NP2KAI_PATCHSET_SHA256").map(str::len), Some(64));
}

#[test]
fn default_paths_stay_inside_the_np2kai_adapter() {
    let root = Path::new("/repo");
    assert!(default_core_path(root).starts_with(root.join("adapters/np2kai/work")));
    assert!(default_build_info_path(root).starts_with(root.join("adapters/np2kai/work")));
}

#[test]
fn hdi_geometry_requires_consistent_header_and_file_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.hdi");
    let mut bytes = vec![0_u8; 32 + 16];
    for (offset, value) in [(8, 32_u32), (12, 16), (16, 2), (20, 2), (24, 2), (28, 2)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(inspect_hdi_geometry(&path).unwrap().unwrap().sector_size, 2);
    bytes.pop();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(inspect_hdi_geometry(&path).unwrap(), None);
}

#[test]
fn managed_working_copy_is_byte_identical_and_source_bound() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.hdi");
    std::fs::write(&source, b"immutable source bytes").unwrap();
    let runtime = dir.path().join("runtime");
    let (copy, digest) = prepare_working_copy(&source, &runtime).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"immutable source bytes");
    assert_eq!(digest, sha256_file(&source).unwrap());
    assert_ne!(copy, source);
}

#[test]
fn managed_working_copy_rejects_non_hdi_media_before_staging() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("disk.hdm");
    std::fs::write(&source, b"not an HDI image").unwrap();

    let error = prepare_working_copy(&source, &dir.path().join("runtime")).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!dir.path().join("runtime/media/content.hdi").exists());
}

#[cfg(unix)]
#[test]
fn managed_working_copy_rejects_a_preexisting_destination_symlink() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.hdi");
    let victim = dir.path().join("victim.hdi");
    std::fs::write(&source, b"new media").unwrap();
    std::fs::write(&victim, b"must remain unchanged").unwrap();
    let runtime = dir.path().join("runtime");
    std::fs::create_dir_all(runtime.join("media")).unwrap();
    symlink(&victim, runtime.join("media/content.hdi")).unwrap();

    assert!(prepare_working_copy(&source, &runtime).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"must remain unchanged");
}

#[test]
fn build_identity_rejects_incomplete_profile_attestations() {
    let digest = "a".repeat(64);
    let mut identity = BuildIdentity {
        upstream: "upstream".into(),
        commit: "c".repeat(40),
        archive_sha256: digest.clone(),
        patchset_sha256: digest.clone(),
        build_profile: "license-clean-libretro".into(),
        compiled_defines_sha256: digest.clone(),
        compiled_sources_sha256: digest.clone(),
        license_manifest_sha256: digest.clone(),
        core_sha256: digest,
        required_defines: vec![
            "USE_MAME_BSD".into(),
            "SUPPORT_FPU_SOFTFLOAT3".into(),
            "SUPPORT_EMUCAP_DEBUG".into(),
        ],
        excluded_components: vec![
            "fmgen".into(),
            "mame-gpl-sound".into(),
            "dosbox-fpu".into(),
            "softfloat-legacy".into(),
            "trident-tgui".into(),
        ],
    };
    validate_build_identity(&identity).unwrap();
    identity.excluded_components.pop();
    assert!(validate_build_identity(&identity).is_err());
    identity.excluded_components.push("trident-tgui".into());
    identity.license_manifest_sha256 = "not-a-digest".into();
    assert!(validate_build_identity(&identity).is_err());
}
