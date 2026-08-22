use super::*;

#[test]
fn cue_identity_changes_when_a_referenced_track_changes() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    let track = directory.path().join("track.bin");
    std::fs::write(&track, b"first-build").unwrap();
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();

    let before = identify_composite_content(&cue).unwrap().unwrap();
    std::fs::write(&track, b"second-build").unwrap();
    let after = identify_composite_content(&cue).unwrap().unwrap();

    assert_ne!(before.digest, after.digest);
    assert_ne!(before.members[1].digest, after.members[1].digest);
    assert_eq!(before.members[0].digest, after.members[0].digest);
    assert_ne!(before.tracking_id(), after.tracking_id());
    crate::track::store::validate_ledger_id("rom_sha1", &before.tracking_id()).unwrap();
}

#[test]
fn single_file_content_is_left_to_its_adapter() {
    let directory = tempfile::tempdir().unwrap();
    let rom = directory.path().join("game.rom");
    std::fs::write(&rom, b"rom").unwrap();
    assert!(identify_composite_content(&rom).unwrap().is_none());
}

#[test]
fn cue_identity_does_not_depend_on_the_entry_path_or_file_name() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for directory in [first.path(), second.path()] {
        std::fs::write(directory.join("track.bin"), b"same-track").unwrap();
    }
    let cue_text = "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n";
    let first_cue = first.path().join("first-name.cue");
    let second_cue = second.path().join("second-name.cue");
    std::fs::write(&first_cue, cue_text).unwrap();
    std::fs::write(&second_cue, cue_text).unwrap();

    let first_identity = identify_composite_content(&first_cue).unwrap().unwrap();
    let second_identity = identify_composite_content(&second_cue).unwrap().unwrap();

    assert_eq!(first_identity.digest, second_identity.digest);
    assert_eq!(first_identity.tracking_id(), second_identity.tracking_id());
}

#[test]
fn mednafen_cue_identity_binds_its_implicit_sbi_without_affecting_other_loaders() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    std::fs::write(directory.path().join("track.bin"), b"track").unwrap();
    std::fs::write(directory.path().join("disc.sbi"), b"SBI\0").unwrap();
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();

    let other_loader = identify_composite_content_for_adapter(&cue, Some("flycast"))
        .unwrap()
        .unwrap();
    let mednafen = identify_composite_content_for_adapter(&cue, Some("mednafen"))
        .unwrap()
        .unwrap();
    assert_eq!(other_loader.members.len(), 2);
    assert_eq!(mednafen.members.len(), 3);
    assert_ne!(other_loader.digest, mednafen.digest);
}

#[test]
fn gdi_identity_is_a_graph_not_a_descriptor_hash() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["track01.bin", "track02.raw", "track03.bin"] {
        std::fs::write(directory.path().join(name), vec![0_u8; 4704]).unwrap();
    }
    let gdi = directory.path().join("disc.gdi");
    std::fs::write(
        &gdi,
        "3\n1 0 4 2352 track01.bin 0\n2 450 0 2352 track02.raw 0\n3 45000 4 2352 track03.bin 0\n",
    )
    .unwrap();
    let before = identify_composite_content(&gdi).unwrap().unwrap();
    assert_eq!(before.scope, ContentIdentityScope::GdiGraph);
    assert_eq!(before.members.len(), 4);

    std::fs::write(directory.path().join("track03.bin"), vec![1_u8; 4704]).unwrap();
    let after = identify_composite_content(&gdi).unwrap().unwrap();
    assert_ne!(before.digest, after.digest);
}

#[test]
fn malformed_supported_descriptors_do_not_degrade_to_single_file_identity() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("malicious.cue");
    std::fs::write(
        &cue,
        "FILE \"../../etc/passwd\" BINARY\nTRACK 01 MODE1/2352\n",
    )
    .unwrap();
    assert_eq!(
        identify_composite_content(&cue).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn cue_member_is_disclosed_without_reading_its_metadata_or_content() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"unrelated.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();

    let IndirectMediaAdmission::Review {
        approval,
        members,
        newly_declared,
    } = inspect_indirect_media(&cue, "flycast", None).unwrap()
    else {
        panic!("descriptor media must require review");
    };
    assert_eq!(newly_declared, ["unrelated.bin"]);
    assert_eq!(members[0].declared_name, "unrelated.bin");
    assert!(!directory.path().join("unrelated.bin").exists());

    let error = validate_approved_composite_content_for_adapter(&cue, "flycast", Some(&approval))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn descriptor_change_invalidates_the_previous_approval_before_member_hashing() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    std::fs::write(&cue, "FILE \"first.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    let IndirectMediaAdmission::Review { approval, .. } =
        inspect_indirect_media(&cue, "flycast", None).unwrap()
    else {
        panic!("descriptor media must require review");
    };

    std::fs::write(&cue, "FILE \"second.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    let error = inspect_indirect_media(&cue, "flycast", Some(&approval)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!directory.path().join("first.bin").exists());
    assert!(!directory.path().join("second.bin").exists());
}

#[test]
fn m3u_requires_each_new_descriptor_frontier_before_media_access() {
    let directory = tempfile::tempdir().unwrap();
    let playlist = directory.path().join("set.m3u");
    let cue = directory.path().join("disc.cue");
    std::fs::write(&playlist, "disc.cue\n").unwrap();
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();

    let IndirectMediaAdmission::Review {
        approval: descriptor_approval,
        newly_declared,
        ..
    } = inspect_indirect_media(&playlist, "mednafen", None).unwrap()
    else {
        panic!("root playlist must require descriptor review");
    };
    assert_eq!(newly_declared, ["disc.cue"]);

    let IndirectMediaAdmission::Review {
        approval: media_approval,
        newly_declared,
        ..
    } = inspect_indirect_media(&playlist, "mednafen", Some(&descriptor_approval)).unwrap()
    else {
        panic!("approved descriptor must expose a new media frontier");
    };
    assert_eq!(newly_declared, ["disc.sbi", "track.bin"]);
    assert!(!directory.path().join("track.bin").exists());

    let error = identify_approved_composite_content_for_adapter(
        &playlist,
        "mednafen",
        Some(&media_approval),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn approval_is_bound_to_the_selected_entry_and_adapter() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.cue");
    let second = directory.path().join("second.cue");
    let text = "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n";
    std::fs::write(&first, text).unwrap();
    std::fs::write(&second, text).unwrap();
    let IndirectMediaAdmission::Review { approval, .. } =
        inspect_indirect_media(&first, "flycast", None).unwrap()
    else {
        panic!("descriptor media must require review");
    };

    assert_eq!(
        inspect_indirect_media(&second, "flycast", Some(&approval))
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        inspect_indirect_media(&first, "mednafen", Some(&approval))
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn approved_cue_can_establish_its_exact_identity() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    std::fs::write(directory.path().join("track.bin"), b"track").unwrap();
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    let IndirectMediaAdmission::Review { approval, .. } =
        inspect_indirect_media(&cue, "flycast", None).unwrap()
    else {
        panic!("descriptor media must require review");
    };

    let identity =
        identify_approved_composite_content_for_adapter(&cue, "flycast", Some(&approval))
            .unwrap()
            .unwrap();
    assert_eq!(identity.members.len(), 2);
}

#[test]
fn presentation_only_descriptor_edits_keep_authority_but_change_byte_identity() {
    let directory = tempfile::tempdir().unwrap();
    let cue = directory.path().join("disc.cue");
    std::fs::write(directory.path().join("track.bin"), b"track").unwrap();
    std::fs::write(&cue, "FILE \"track.bin\" BINARY\nTRACK 01 MODE1/2352\n").unwrap();
    let IndirectMediaAdmission::Review { approval, .. } =
        inspect_indirect_media(&cue, "flycast", None).unwrap()
    else {
        panic!("descriptor media must require review");
    };
    let before = identify_approved_composite_content_for_adapter(&cue, "flycast", Some(&approval))
        .unwrap()
        .unwrap();

    std::fs::write(
        &cue,
        "REM presentation changed\nFILE \"track.bin\" BINARY\n  TRACK 01 MODE1/2352\n",
    )
    .unwrap();
    let IndirectMediaAdmission::Approved {
        approval: unchanged,
        ..
    } = inspect_indirect_media(&cue, "flycast", Some(&approval)).unwrap()
    else {
        panic!("an unchanged member set must retain its authority");
    };
    let after = identify_approved_composite_content_for_adapter(&cue, "flycast", Some(&unchanged))
        .unwrap()
        .unwrap();

    assert_eq!(unchanged, approval);
    assert_ne!(after.digest, before.digest);
}
