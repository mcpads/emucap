use std::io;

use serde::{Deserialize, Serialize};

use super::{
    lock_with_deadline, now_unix_ms, open_private_lock, process_state, read_json_if_exists,
    validate_launch_id, write_atomic_json, CurrentManifest, ProcessIdentity, ProcessState,
    RuntimeStore, SCHEMA_VERSION,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminationState {
    Requested,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessTermination {
    pub pid: u32,
    pub before: ProcessState,
    pub after: ProcessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<crate::launch::TerminationMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationTermination {
    pub completed: bool,
    pub emulator: ProcessTermination,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<ProcessTermination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminationRecord {
    pub schema_version: u32,
    pub launch_id: String,
    pub port: u16,
    pub reason: String,
    pub state: TerminationState,
    pub requested_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<GenerationTermination>,
}

impl TerminationRecord {
    pub fn requested(port: u16, launch_id: impl Into<String>, previous: Option<&Self>) -> Self {
        let now = now_unix_ms();
        Self {
            schema_version: SCHEMA_VERSION,
            launch_id: launch_id.into(),
            port,
            reason: "requested_stop".into(),
            state: TerminationState::Requested,
            requested_at_unix_ms: previous
                .map(|record| record.requested_at_unix_ms)
                .unwrap_or(now),
            updated_at_unix_ms: now,
            completed_at_unix_ms: None,
            result: None,
        }
    }

    pub fn finish(mut self, result: GenerationTermination) -> Self {
        let now = now_unix_ms();
        self.state = if result.completed {
            TerminationState::Completed
        } else {
            TerminationState::Failed
        };
        self.updated_at_unix_ms = now;
        self.completed_at_unix_ms = result.completed.then_some(now);
        self.result = Some(result);
        self
    }
}

impl RuntimeStore {
    pub fn read_termination(
        &self,
        port: u16,
        launch_id: &str,
    ) -> io::Result<Option<TerminationRecord>> {
        validate_launch_id(launch_id)?;
        let record: Option<TerminationRecord> =
            read_json_if_exists(&self.termination_path(port, launch_id))?;
        if let Some(record) = record.as_ref() {
            validate_termination_record(record, None)?;
        }
        Ok(record)
    }

    pub fn write_current_termination(&self, record: &TerminationRecord) -> io::Result<()> {
        validate_launch_id(&record.launch_id)?;
        validate_termination_record(record, None)?;
        let current_lock = self.session_dir(record.port).join(".current.lock");
        let lock = open_private_lock(&current_lock)?;
        lock_with_deadline(&lock, std::time::Duration::from_millis(250))?;
        let result = (|| {
            let current = self.read_current(record.port)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "runtime current generation does not exist",
                )
            })?;
            if current.launch_id != record.launch_id {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "runtime current generation changed before termination record update",
                ));
            }
            validate_termination_record(record, Some(&current))?;
            let generation = self.generation_dir(record.port, &record.launch_id);
            self.create_managed_dir(&generation)?;
            write_atomic_json(
                &self.termination_path(record.port, &record.launch_id),
                record,
            )
        })();
        let _ = fs2::FileExt::unlock(&lock);
        result
    }
}

impl CurrentManifest {
    pub fn terminate_owned_processes(&self) -> io::Result<()> {
        let result = self.terminate_owned_processes_report()?;
        if result.completed {
            Ok(())
        } else {
            let errors = [
                result.emulator.error.as_deref(),
                result
                    .bridge
                    .as_ref()
                    .and_then(|process| process.error.as_deref()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
            Err(io::Error::other(if errors.is_empty() {
                "one or more generation-owned processes did not exit".into()
            } else {
                errors
            }))
        }
    }

    pub fn validate_termination_targets(&self) -> io::Result<()> {
        if self.process_state() == ProcessState::Unknown {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "emulator process identity is unknown",
            ));
        }
        if self.bridge_process_state() == Some(ProcessState::Unknown) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bridge process identity is unknown",
            ));
        }
        Ok(())
    }

    pub fn terminate_owned_processes_report(&self) -> io::Result<GenerationTermination> {
        self.terminate_owned_processes_with(|process| {
            crate::launch::terminate_detached_checked(process.pid, || {
                process_state(process) == ProcessState::Alive
            })
        })
    }

    pub(super) fn terminate_owned_processes_with<F>(
        &self,
        mut terminate: F,
    ) -> io::Result<GenerationTermination>
    where
        F: FnMut(&ProcessIdentity) -> io::Result<crate::launch::TerminationMethod>,
    {
        // Refuse before the first signal when any owned target cannot be identified. State can
        // still change after this preflight, so terminate_process rechecks each identity directly
        // before signalling and reports a partial failure rather than switching to PID-only cleanup.
        self.validate_termination_targets()?;

        let emulator = terminate_process_with(&self.emulator, &mut terminate);
        let bridge = self
            .bridge
            .as_ref()
            .map(|process| terminate_process_with(process, &mut terminate));
        let completed = emulator.after == ProcessState::Exited
            && bridge
                .as_ref()
                .is_none_or(|process| process.after == ProcessState::Exited);
        Ok(GenerationTermination {
            completed,
            emulator,
            bridge,
        })
    }
}

fn terminate_process_with<F>(process: &ProcessIdentity, terminate: &mut F) -> ProcessTermination
where
    F: FnMut(&ProcessIdentity) -> io::Result<crate::launch::TerminationMethod>,
{
    let before = process_state(process);
    match before {
        ProcessState::Exited => ProcessTermination {
            pid: process.pid,
            before,
            after: ProcessState::Exited,
            method: Some(crate::launch::TerminationMethod::AlreadyExited),
            error: None,
        },
        ProcessState::Unknown => ProcessTermination {
            pid: process.pid,
            before,
            after: ProcessState::Unknown,
            method: None,
            error: Some("process identity became unknown before termination".into()),
        },
        ProcessState::Alive => {
            let terminated = terminate(process);
            let after = process_state(process);
            match terminated {
                Ok(method) if after == ProcessState::Exited => ProcessTermination {
                    pid: process.pid,
                    before,
                    after,
                    method: Some(method),
                    error: None,
                },
                Ok(method) => ProcessTermination {
                    pid: process.pid,
                    before,
                    after,
                    method: Some(method),
                    error: Some("process did not reach a verified exited state".into()),
                },
                Err(error) => ProcessTermination {
                    pid: process.pid,
                    before,
                    after,
                    method: None,
                    error: Some(error.to_string()),
                },
            }
        }
    }
}

fn validate_termination_record(
    record: &TerminationRecord,
    current: Option<&CurrentManifest>,
) -> io::Result<()> {
    validate_launch_id(&record.launch_id)?;
    let state_consistent = match (record.state, record.result.as_ref()) {
        (TerminationState::Requested, None) => record.completed_at_unix_ms.is_none(),
        (TerminationState::Completed, Some(result)) => {
            result.completed
                && result.emulator.after == ProcessState::Exited
                && result
                    .bridge
                    .as_ref()
                    .is_none_or(|process| process.after == ProcessState::Exited)
                && record.completed_at_unix_ms.is_some()
        }
        (TerminationState::Failed, Some(result)) => {
            !result.completed && record.completed_at_unix_ms.is_none()
        }
        _ => false,
    };
    let process_consistent = current.is_none_or(|current| {
        let terminal_matches_host = record.state != TerminationState::Completed
            || (current.process_state() == ProcessState::Exited
                && current
                    .bridge_process_state()
                    .is_none_or(|state| state == ProcessState::Exited));
        record.port == current.port
            && record.launch_id == current.launch_id
            && terminal_matches_host
            && record.result.as_ref().is_none_or(|result| {
                result.emulator.pid == current.emulator.pid
                    && match (result.bridge.as_ref(), current.bridge.as_ref()) {
                        (Some(result), Some(process)) => result.pid == process.pid,
                        (None, None) => true,
                        _ => false,
                    }
            })
    });
    if record.schema_version == SCHEMA_VERSION
        && record.reason == "requested_stop"
        && record.updated_at_unix_ms >= record.requested_at_unix_ms
        && state_consistent
        && process_consistent
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime termination record is inconsistent with its generation or terminal state",
        ))
    }
}
