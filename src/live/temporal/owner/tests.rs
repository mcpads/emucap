use super::*;
fn attachment(session: u64) -> Attachment {
    Attachment {
        broker_instance: "broker".into(),
        registration: 1,
        session,
    }
}
fn key(id: &str) -> OperationKey {
    OperationKey {
        runtime: "native".into(),
        owner_id: "core".into(),
        operation_id: id.into(),
    }
}
fn active() -> ProducerOwnership {
    let mut owner = ProducerOwnership::new("native".into()).unwrap();
    owner.attach(attachment(1)).unwrap();
    owner.begin(&attachment(1), key("parent")).unwrap();
    owner
}
#[test]
fn detach_covers_attempted_writes_between_phases_and_blocks_replacement_until_clean() {
    let mut owner = active();
    owner
        .record_input_attempt(&attachment(1), &key("parent"), 2)
        .unwrap();
    // The native write may fail or Core may disappear before any child advance is sent.
    let plan = owner.detach(&attachment(1)).unwrap().unwrap();
    assert_eq!(plan.input_ports, BTreeSet::from([2]));
    assert_eq!(owner.attach(attachment(2)), Err(OwnershipError::Busy));
    assert!(owner
        .record_input_attempt(&attachment(1), &key("parent"), 0)
        .is_err());
    owner
        .finish_cleanup(
            &plan,
            CleanupEvidence {
                stop_verified: true,
                released_ports: BTreeSet::from([2]),
            },
        )
        .unwrap();
    owner.attach(attachment(2)).unwrap();
    owner.begin(&attachment(2), key("next")).unwrap();
    assert!(owner.detach(&attachment(1)).unwrap().is_none());
    owner.record_effect(&attachment(2), &key("next")).unwrap();
}
#[test]
fn cleanup_failures_survive_attachment_replacement() {
    for evidence in [
        CleanupEvidence::default(),
        CleanupEvidence {
            stop_verified: true,
            released_ports: BTreeSet::new(),
        },
        CleanupEvidence {
            stop_verified: true,
            released_ports: BTreeSet::from([1, 2]),
        },
    ] {
        let mut owner = active();
        owner
            .record_input_attempt(&attachment(1), &key("parent"), 2)
            .unwrap();
        let plan = owner.detach(&attachment(1)).unwrap().unwrap();
        assert_eq!(
            owner.finish_cleanup(&plan, evidence),
            Err(OwnershipError::Retired)
        );
        assert_eq!(owner.attach(attachment(2)), Err(OwnershipError::Retired));
        assert_eq!(
            owner.begin(&attachment(1), key("next")),
            Err(OwnershipError::Retired)
        );
    }
}
#[test]
fn step_and_unused_parent_acquire_no_input_release() {
    for effect in [false, true] {
        let mut owner = active();
        if effect {
            owner.record_effect(&attachment(1), &key("parent")).unwrap();
        }
        let plan = owner.start_cleanup(&attachment(1), &key("parent")).unwrap();
        assert!(plan.input_ports.is_empty());
        owner
            .finish_cleanup(
                &plan,
                CleanupEvidence {
                    stop_verified: effect,
                    released_ports: BTreeSet::new(),
                },
            )
            .unwrap();
        assert!(owner.terminal(&attachment(1), &key("parent")).is_some());
        assert_eq!(
            owner.begin(&attachment(1), key("parent")),
            Err(OwnershipError::NotActive)
        );
        owner.begin(&attachment(1), key("next")).unwrap();
        assert!(owner.terminal(&attachment(1), &key("parent")).is_none());
        assert_eq!(
            owner.finish_cleanup(&plan, CleanupEvidence::default()),
            Err(OwnershipError::NotActive)
        );
    }
}
#[test]
fn invalid_identity_stale_owner_and_duplicate_begin_have_no_extra_effects() {
    let mut owner = active();
    owner.begin(&attachment(1), key("parent")).unwrap();
    assert_eq!(
        owner.begin(&attachment(1), key("other")),
        Err(OwnershipError::Busy)
    );
    assert_eq!(
        owner.record_effect(&attachment(2), &key("parent")),
        Err(OwnershipError::WrongAttachment)
    );
    let mut wrong = key("other");
    wrong.runtime = "replacement".into();
    assert_eq!(
        owner.begin(&attachment(1), wrong),
        Err(OwnershipError::InvalidIdentity)
    );
    let plan = owner.detach(&attachment(1)).unwrap().unwrap();
    assert!(!plan.effects_started);
    owner
        .finish_cleanup(&plan, CleanupEvidence::default())
        .unwrap();
    assert!(owner.attach(attachment(0)).is_err());
    owner.attach(attachment(2)).unwrap();
}
