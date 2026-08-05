use super::continuity::{ExecutionState, RuntimeBindingState, TransportState};
use super::runtime::ProcessState;
use super::task_entry::{
    admit_generation_transition, classify_entry, EntryDisposition, EntryLeaseState, EntryReason,
    EntryState, ListenerState, RuntimeObservation, RuntimeProcessObservation, TransitionAdmission,
    TransitionIntent,
};

fn current(emulator: ProcessState, bridge: Option<ProcessState>) -> RuntimeProcessObservation {
    RuntimeProcessObservation { emulator, bridge }
}

fn disposition(state: EntryState, reason: EntryReason) -> EntryDisposition {
    EntryDisposition { state, reason }
}

#[test]
fn entry_classifier_covers_empty_terminal_and_owned_cleanup_paths() {
    let empty = RuntimeObservation::empty(ListenerState::Bound);
    assert_eq!(
        classify_entry(&empty),
        disposition(EntryState::ReadyForContent, EntryReason::ReadyNoHistory)
    );

    let terminal = RuntimeObservation {
        current: Some(current(ProcessState::Exited, None)),
        lease: EntryLeaseState::Absent,
        ..empty.clone()
    };
    assert_eq!(
        classify_entry(&terminal),
        disposition(EntryState::ReadyForContent, EntryReason::TerminalHistory)
    );

    let cleanup = RuntimeObservation {
        current: Some(current(ProcessState::Exited, Some(ProcessState::Alive))),
        lease: EntryLeaseState::Available,
        ..empty
    };
    assert_eq!(
        classify_entry(&cleanup),
        disposition(
            EntryState::ReadyForContent,
            EntryReason::OwnedHelperCleanupPending
        )
    );
}

#[test]
fn entry_classifier_prioritizes_metadata_candidates_and_listener_safety() {
    let invalid = RuntimeObservation {
        runtime_metadata_valid: false,
        runtime_candidate_count: 2,
        listener: ListenerState::Blocked,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&invalid),
        disposition(
            EntryState::RepairRuntimeMetadata,
            EntryReason::RuntimeMetadataInvalid
        )
    );

    let candidates = RuntimeObservation {
        runtime_candidate_count: 2,
        listener: ListenerState::Blocked,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&candidates),
        disposition(
            EntryState::TransitionBlocked,
            EntryReason::RuntimeCandidateAmbiguity
        )
    );

    let blocked = RuntimeObservation {
        execution: ExecutionState::Crashed,
        failure_context_available: true,
        ..RuntimeObservation::empty(ListenerState::Blocked)
    };
    assert_eq!(
        classify_entry(&blocked),
        disposition(EntryState::TransitionBlocked, EntryReason::ListenerBlocked)
    );

    let uncertain = RuntimeObservation {
        transport: TransportState::Stalled,
        control_observation_uncertain: true,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&uncertain),
        disposition(
            EntryState::TransitionBlocked,
            EntryReason::TransportUncertain
        )
    );

    let preserved_failure = RuntimeObservation {
        execution: ExecutionState::Crashed,
        failure_context_available: true,
        transport: TransportState::Stalled,
        control_observation_uncertain: true,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&preserved_failure),
        disposition(EntryState::InspectFailure, EntryReason::FailurePreserved)
    );
}

#[test]
fn completed_stop_proof_outweighs_only_post_stop_transport_uncertainty() {
    let terminal = RuntimeObservation {
        transport: TransportState::Stalled,
        control_observation_uncertain: true,
        termination_completed: true,
        current: Some(current(ProcessState::Exited, Some(ProcessState::Exited))),
        lease: EntryLeaseState::Held,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&terminal),
        disposition(EntryState::ReadyForContent, EntryReason::TerminalHistory)
    );

    let no_completed_stop = RuntimeObservation {
        termination_completed: false,
        ..terminal.clone()
    };
    assert_eq!(
        classify_entry(&no_completed_stop),
        disposition(
            EntryState::TransitionBlocked,
            EntryReason::TransportUncertain
        )
    );

    let live_process = RuntimeObservation {
        termination_completed: true,
        current: Some(current(ProcessState::Alive, None)),
        ..terminal
    };
    assert_eq!(
        classify_entry(&live_process),
        disposition(
            EntryState::TransitionBlocked,
            EntryReason::TransportUncertain
        )
    );
}

#[test]
fn entry_classifier_keeps_live_and_crashed_executions_out_of_new_content() {
    let live = RuntimeObservation {
        transport: TransportState::Connected,
        runtime_binding: RuntimeBindingState::Bound,
        adapter_connected: true,
        current: Some(current(ProcessState::Alive, None)),
        lease: EntryLeaseState::Held,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&live),
        disposition(
            EntryState::InspectExisting,
            EntryReason::LiveManagedGeneration
        )
    );

    let unmanaged = RuntimeObservation {
        transport: TransportState::Connected,
        runtime_binding: RuntimeBindingState::Unmanaged,
        adapter_connected: true,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&unmanaged),
        disposition(
            EntryState::InspectExisting,
            EntryReason::LiveUnmanagedGeneration
        )
    );

    let crashed = RuntimeObservation {
        execution: ExecutionState::Crashed,
        failure_context_available: true,
        adapter_connected: true,
        current: Some(current(ProcessState::Alive, None)),
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&crashed),
        disposition(EntryState::InspectFailure, EntryReason::FailurePreserved)
    );
}

#[test]
fn entry_classifier_distinguishes_reattach_from_unverifiable_or_dead_bridge() {
    let reattach = RuntimeObservation {
        current: Some(current(ProcessState::Alive, Some(ProcessState::Alive))),
        lease: EntryLeaseState::Absent,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&reattach),
        disposition(EntryState::ReattachExisting, EntryReason::TransportReattach)
    );

    for (bridge, reason) in [
        (ProcessState::Unknown, EntryReason::BridgeIdentityUnknown),
        (ProcessState::Exited, EntryReason::BridgeExited),
    ] {
        let observation = RuntimeObservation {
            current: Some(current(ProcessState::Alive, Some(bridge))),
            ..RuntimeObservation::empty(ListenerState::Bound)
        };
        assert_eq!(
            classify_entry(&observation),
            disposition(EntryState::TransitionBlocked, reason)
        );
    }

    let unknown_process = RuntimeObservation {
        current: Some(current(ProcessState::Unknown, None)),
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        classify_entry(&unknown_process),
        disposition(
            EntryState::TransitionBlocked,
            EntryReason::ProcessIdentityUnknown
        )
    );
}

#[test]
fn entry_classifier_distinguishes_absent_from_unknown_or_occupied_lease() {
    for (lease, expected) in [
        (
            EntryLeaseState::Absent,
            disposition(EntryState::ReadyForContent, EntryReason::TerminalHistory),
        ),
        (
            EntryLeaseState::Held,
            disposition(EntryState::ReadyForContent, EntryReason::TerminalHistory),
        ),
        (
            EntryLeaseState::Available,
            disposition(EntryState::ReadyForContent, EntryReason::TerminalHistory),
        ),
        (
            EntryLeaseState::Occupied,
            disposition(EntryState::TransitionBlocked, EntryReason::LeaseOccupied),
        ),
        (
            EntryLeaseState::Unknown,
            disposition(EntryState::TransitionBlocked, EntryReason::LeaseUnknown),
        ),
    ] {
        let observation = RuntimeObservation {
            current: Some(current(ProcessState::Exited, None)),
            lease,
            ..RuntimeObservation::empty(ListenerState::Bound)
        };
        assert_eq!(classify_entry(&observation), expected);
    }
}

#[test]
fn transition_admission_reuses_entry_safety_for_launch_and_replace() {
    let empty = RuntimeObservation::empty(ListenerState::Bound);
    assert_eq!(
        admit_generation_transition(&empty, TransitionIntent::Launch),
        TransitionAdmission::Allowed
    );

    let live = RuntimeObservation {
        transport: TransportState::Connected,
        runtime_binding: RuntimeBindingState::Bound,
        adapter_connected: true,
        current: Some(current(ProcessState::Alive, None)),
        lease: EntryLeaseState::Available,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        admit_generation_transition(&live, TransitionIntent::Launch),
        TransitionAdmission::Rejected(EntryReason::LiveManagedGeneration)
    );
    assert_eq!(
        admit_generation_transition(&live, TransitionIntent::Replace),
        TransitionAdmission::AcquireLease
    );

    let terminal = RuntimeObservation {
        current: Some(current(ProcessState::Exited, None)),
        lease: EntryLeaseState::Available,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        admit_generation_transition(&terminal, TransitionIntent::Launch),
        TransitionAdmission::AcquireLease
    );
}

#[test]
fn transition_admission_never_uses_unmanaged_or_unknown_ownership() {
    let unmanaged = RuntimeObservation {
        transport: TransportState::Connected,
        runtime_binding: RuntimeBindingState::Unmanaged,
        adapter_connected: true,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        admit_generation_transition(&unmanaged, TransitionIntent::Replace),
        TransitionAdmission::Rejected(EntryReason::LiveUnmanagedGeneration)
    );

    let unknown_lease = RuntimeObservation {
        current: Some(current(ProcessState::Exited, None)),
        lease: EntryLeaseState::Unknown,
        ..RuntimeObservation::empty(ListenerState::Bound)
    };
    assert_eq!(
        admit_generation_transition(&unknown_lease, TransitionIntent::Launch),
        TransitionAdmission::Rejected(EntryReason::LeaseUnknown)
    );
}
