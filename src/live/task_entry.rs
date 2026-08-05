//! Task-entry classification shared by bootstrap presentation and launch admission.
//!
//! The classifier is pure. Runtime-file and adapter observations are collected by the caller so
//! launch can re-read them immediately before a generation transition.

use serde::{Deserialize, Serialize};

use super::continuity::{
    lease_view, ExecutionState, LinkRecord, RuntimeBindingState, TransportState,
};
use super::link::EmulatorLink;
use super::runtime::{capture_process, LeaseState, ProcessState, RuntimeStore, TerminationState};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListenerState {
    Bound,
    Blocked,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryLeaseState {
    Absent,
    Held,
    Available,
    Occupied,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeProcessObservation {
    pub emulator: ProcessState,
    pub bridge: Option<ProcessState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeObservation {
    pub listener: ListenerState,
    pub transport: TransportState,
    pub execution: ExecutionState,
    pub runtime_binding: RuntimeBindingState,
    pub adapter_connected: bool,
    pub control_observation_uncertain: bool,
    pub termination_completed: bool,
    pub failure_context_available: bool,
    pub runtime_metadata_valid: bool,
    pub runtime_candidate_count: usize,
    pub lease: EntryLeaseState,
    pub current: Option<RuntimeProcessObservation>,
}

impl RuntimeObservation {
    pub fn empty(listener: ListenerState) -> Self {
        Self {
            listener,
            transport: TransportState::Disconnected,
            execution: ExecutionState::Unknown,
            runtime_binding: RuntimeBindingState::Unobserved,
            adapter_connected: false,
            control_observation_uncertain: false,
            termination_completed: false,
            failure_context_available: false,
            runtime_metadata_valid: true,
            runtime_candidate_count: 0,
            lease: EntryLeaseState::Absent,
            current: None,
        }
    }
}

pub fn observe_runtime(
    link: &dyn EmulatorLink,
    listener: ListenerState,
    adapter_connected: bool,
) -> RuntimeObservation {
    let continuity = link.continuity();
    let mut observation = RuntimeObservation {
        listener,
        transport: if adapter_connected {
            TransportState::Connected
        } else {
            continuity.transport.state
        },
        execution: continuity.execution.state,
        runtime_binding: continuity.runtime_binding.state,
        adapter_connected,
        control_observation_uncertain: false,
        termination_completed: false,
        failure_context_available: continuity.evidence.failure_context_available,
        runtime_metadata_valid: !continuity
            .runtime_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.blocks_generation_transition),
        runtime_candidate_count: link.runtime_candidates().len(),
        lease: EntryLeaseState::Absent,
        current: None,
    };
    let Some(port) = link.endpoint_port() else {
        return observation;
    };
    let store = RuntimeStore::discover();
    let current = match store.read_current(port) {
        Ok(current) => current,
        Err(_) => {
            observation.runtime_metadata_valid = false;
            return observation;
        }
    };
    let Some(current) = current else {
        return observation;
    };
    observation.current = Some(RuntimeProcessObservation {
        emulator: current.process_state(),
        bridge: current.bridge_process_state(),
    });
    observation.termination_completed = match store.read_termination(port, &current.launch_id) {
        Ok(termination) => termination
            .as_ref()
            .is_some_and(|termination| termination.state == TerminationState::Completed),
        Err(_) => {
            observation.runtime_metadata_valid = false;
            return observation;
        }
    };
    let record = match store.read_link_json::<LinkRecord>(port, &current.launch_id) {
        Ok(record) => record.filter(|record| record.launch_id == current.launch_id),
        Err(_) => {
            observation.runtime_metadata_valid = false;
            return observation;
        }
    };
    observation.lease = record
        .as_ref()
        .and_then(|record| record.lease.as_ref())
        .map(|lease| {
            let holder = capture_process(std::process::id());
            match lease_view(lease, &holder).state {
                LeaseState::Held => EntryLeaseState::Held,
                LeaseState::Available => EntryLeaseState::Available,
                LeaseState::Occupied => EntryLeaseState::Occupied,
                LeaseState::Unknown => EntryLeaseState::Unknown,
            }
        })
        .unwrap_or_else(|| {
            if continuity.lease_record_present {
                match continuity.lease.state {
                    LeaseState::Held => EntryLeaseState::Held,
                    LeaseState::Available => EntryLeaseState::Available,
                    LeaseState::Occupied => EntryLeaseState::Occupied,
                    LeaseState::Unknown => EntryLeaseState::Unknown,
                }
            } else {
                EntryLeaseState::Absent
            }
        });
    observation
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    RepairRuntimeMetadata,
    InspectFailure,
    InspectExisting,
    ReattachExisting,
    TransitionBlocked,
    ReadyForContent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryReason {
    RuntimeMetadataInvalid,
    ListenerBlocked,
    ListenerUnavailable,
    RuntimeCandidateAmbiguity,
    FailurePreserved,
    TransportUncertain,
    LiveManagedGeneration,
    LiveUnmanagedGeneration,
    LeaseOccupied,
    LeaseUnknown,
    BridgeExited,
    BridgeIdentityUnknown,
    ProcessIdentityUnknown,
    TransportReattach,
    ReadyNoHistory,
    TerminalHistory,
    OwnedHelperCleanupPending,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryDisposition {
    pub state: EntryState,
    pub reason: EntryReason,
}

impl EntryDisposition {
    const fn new(state: EntryState, reason: EntryReason) -> Self {
        Self { state, reason }
    }

    pub fn accepts_new_content(self) -> bool {
        self.state == EntryState::ReadyForContent
    }
}

fn blocked(reason: EntryReason) -> EntryDisposition {
    EntryDisposition::new(EntryState::TransitionBlocked, reason)
}

fn lease_blocker(lease: EntryLeaseState) -> Option<EntryReason> {
    match lease {
        EntryLeaseState::Occupied => Some(EntryReason::LeaseOccupied),
        EntryLeaseState::Unknown => Some(EntryReason::LeaseUnknown),
        EntryLeaseState::Absent | EntryLeaseState::Held | EntryLeaseState::Available => None,
    }
}

fn completed_terminal_generation(observation: &RuntimeObservation) -> bool {
    observation.termination_completed
        && observation.current.is_some_and(|current| {
            current.emulator == ProcessState::Exited
                && current
                    .bridge
                    .is_none_or(|bridge| bridge == ProcessState::Exited)
        })
}

pub fn classify_entry(observation: &RuntimeObservation) -> EntryDisposition {
    if !observation.runtime_metadata_valid {
        return EntryDisposition::new(
            EntryState::RepairRuntimeMetadata,
            EntryReason::RuntimeMetadataInvalid,
        );
    }
    if observation.runtime_candidate_count > 0 {
        return blocked(EntryReason::RuntimeCandidateAmbiguity);
    }
    match observation.listener {
        ListenerState::Blocked => return blocked(EntryReason::ListenerBlocked),
        ListenerState::Unavailable => return blocked(EntryReason::ListenerUnavailable),
        ListenerState::Bound => {}
    }
    if observation.execution == ExecutionState::Crashed && observation.failure_context_available {
        return EntryDisposition::new(EntryState::InspectFailure, EntryReason::FailurePreserved);
    }
    if observation.control_observation_uncertain && !completed_terminal_generation(observation) {
        return blocked(EntryReason::TransportUncertain);
    }
    if observation.adapter_connected && observation.runtime_binding != RuntimeBindingState::Bound {
        return EntryDisposition::new(
            EntryState::InspectExisting,
            EntryReason::LiveUnmanagedGeneration,
        );
    }

    match observation.current {
        None if observation.adapter_connected => EntryDisposition::new(
            EntryState::InspectExisting,
            EntryReason::LiveUnmanagedGeneration,
        ),
        None => EntryDisposition::new(EntryState::ReadyForContent, EntryReason::ReadyNoHistory),
        Some(current) => match current.emulator {
            ProcessState::Unknown => blocked(EntryReason::ProcessIdentityUnknown),
            ProcessState::Alive => {
                if observation.adapter_connected {
                    return EntryDisposition::new(
                        EntryState::InspectExisting,
                        EntryReason::LiveManagedGeneration,
                    );
                }
                if let Some(reason) = lease_blocker(observation.lease) {
                    return blocked(reason);
                }
                match current.bridge {
                    Some(ProcessState::Unknown) => blocked(EntryReason::BridgeIdentityUnknown),
                    Some(ProcessState::Exited) => blocked(EntryReason::BridgeExited),
                    Some(ProcessState::Alive) | None => EntryDisposition::new(
                        EntryState::ReattachExisting,
                        EntryReason::TransportReattach,
                    ),
                }
            }
            ProcessState::Exited => {
                if let Some(reason) = lease_blocker(observation.lease) {
                    return blocked(reason);
                }
                match current.bridge {
                    Some(ProcessState::Unknown) => blocked(EntryReason::BridgeIdentityUnknown),
                    Some(ProcessState::Alive) => EntryDisposition::new(
                        EntryState::ReadyForContent,
                        EntryReason::OwnedHelperCleanupPending,
                    ),
                    Some(ProcessState::Exited) | None => EntryDisposition::new(
                        EntryState::ReadyForContent,
                        EntryReason::TerminalHistory,
                    ),
                }
            }
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionIntent {
    Launch,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionAdmission {
    Allowed,
    AcquireLease,
    Rejected(EntryReason),
}

pub fn admit_generation_transition(
    observation: &RuntimeObservation,
    intent: TransitionIntent,
) -> TransitionAdmission {
    let disposition = classify_entry(observation);
    match disposition.state {
        EntryState::RepairRuntimeMetadata
        | EntryState::InspectFailure
        | EntryState::TransitionBlocked => {
            return TransitionAdmission::Rejected(disposition.reason);
        }
        EntryState::InspectExisting
            if observation.current.is_none()
                || observation.runtime_binding != RuntimeBindingState::Bound =>
        {
            return TransitionAdmission::Rejected(EntryReason::LiveUnmanagedGeneration);
        }
        EntryState::InspectExisting | EntryState::ReattachExisting
            if intent == TransitionIntent::Launch =>
        {
            return TransitionAdmission::Rejected(EntryReason::LiveManagedGeneration);
        }
        EntryState::InspectExisting
        | EntryState::ReattachExisting
        | EntryState::ReadyForContent => {}
    }

    let Some(current) = observation.current else {
        return if observation.adapter_connected {
            TransitionAdmission::Rejected(EntryReason::LiveUnmanagedGeneration)
        } else {
            TransitionAdmission::Allowed
        };
    };

    if current.emulator == ProcessState::Unknown {
        return TransitionAdmission::Rejected(EntryReason::ProcessIdentityUnknown);
    }
    if current.bridge == Some(ProcessState::Unknown) {
        return TransitionAdmission::Rejected(EntryReason::BridgeIdentityUnknown);
    }
    if current.emulator == ProcessState::Alive
        && current.bridge == Some(ProcessState::Exited)
        && intent == TransitionIntent::Launch
    {
        return TransitionAdmission::Rejected(EntryReason::BridgeExited);
    }

    match observation.lease {
        EntryLeaseState::Held => TransitionAdmission::Allowed,
        EntryLeaseState::Absent | EntryLeaseState::Available => TransitionAdmission::AcquireLease,
        EntryLeaseState::Occupied => TransitionAdmission::Rejected(EntryReason::LeaseOccupied),
        EntryLeaseState::Unknown => TransitionAdmission::Rejected(EntryReason::LeaseUnknown),
    }
}
