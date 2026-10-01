//! Producer-side parent admission. Native I/O stays with the adapter's sole owner.
use crate::live::{control_session::Attachment, reconnect::cancellation::OperationKey};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OwnershipError {
    #[error("invalid operation or attachment identity")]
    InvalidIdentity,
    #[error("attachment does not own this producer")]
    WrongAttachment,
    #[error("another parent operation owns native control")]
    Busy,
    #[error("parent operation is not active")]
    NotActive,
    #[error("native control is retired after unverified cleanup")]
    Retired,
}

/// The adapter must record an input attempt before touching native input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupPlan {
    pub key: OperationKey,
    pub attachment: Attachment,
    pub input_ports: BTreeSet<u64>,
    pub effects_started: bool,
}
#[derive(Debug, Clone, Default)]
pub struct CleanupEvidence {
    pub stop_verified: bool,
    pub released_ports: BTreeSet<u64>,
}
#[derive(Debug)]
struct Parent {
    plan: CleanupPlan,
    stopping: bool,
}

pub struct ProducerOwnership {
    runtime: String,
    attachment: Option<Attachment>,
    parent: Option<Parent>,
    terminal: Option<CleanupPlan>,
    retired: bool,
}
impl ProducerOwnership {
    pub fn new(runtime: String) -> Result<Self, OwnershipError> {
        if runtime.is_empty() || runtime.len() > 256 {
            return Err(OwnershipError::InvalidIdentity);
        }
        Ok(Self {
            runtime,
            attachment: None,
            parent: None,
            terminal: None,
            retired: false,
        })
    }
    fn healthy(&self) -> Result<(), OwnershipError> {
        if self.retired {
            Err(OwnershipError::Retired)
        } else {
            Ok(())
        }
    }
    pub fn attach(&mut self, attachment: Attachment) -> Result<(), OwnershipError> {
        self.healthy()?;
        attachment
            .validate()
            .map_err(|_| OwnershipError::InvalidIdentity)?;
        if self.attachment.as_ref() == Some(&attachment) {
            return Ok(());
        }
        if self.parent.is_some() || self.attachment.is_some() {
            return Err(OwnershipError::Busy);
        }
        self.terminal = None;
        self.attachment = Some(attachment);
        Ok(())
    }
    pub fn begin(
        &mut self,
        attachment: &Attachment,
        key: OperationKey,
    ) -> Result<(), OwnershipError> {
        self.healthy()?;
        key.validate()
            .map_err(|_| OwnershipError::InvalidIdentity)?;
        if key.runtime != self.runtime {
            return Err(OwnershipError::InvalidIdentity);
        }
        if self.attachment.as_ref() != Some(attachment) {
            return Err(OwnershipError::WrongAttachment);
        }
        if let Some(parent) = &self.parent {
            return if parent.plan.key == key && !parent.stopping {
                Ok(())
            } else {
                Err(OwnershipError::Busy)
            };
        }
        if self.terminal.as_ref().is_some_and(|last| last.key == key) {
            return Err(OwnershipError::NotActive);
        }
        self.terminal = None;
        self.parent = Some(Parent {
            plan: CleanupPlan {
                key,
                attachment: attachment.clone(),
                input_ports: BTreeSet::new(),
                effects_started: false,
            },
            stopping: false,
        });
        Ok(())
    }
    fn active(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
    ) -> Result<&mut Parent, OwnershipError> {
        self.healthy()?;
        if self.attachment.as_ref() != Some(attachment) {
            return Err(OwnershipError::WrongAttachment);
        }
        self.parent
            .as_mut()
            .filter(|p| &p.plan.key == key && !p.stopping)
            .ok_or(OwnershipError::NotActive)
    }
    pub fn active_parent_key(&self) -> Option<OperationKey> {
        self.parent.as_ref().map(|parent| parent.plan.key.clone())
    }
    pub fn authorize_unscoped(
        &self,
        attachment: &Attachment,
        observation: bool,
    ) -> Result<(), OwnershipError> {
        self.healthy()?;
        if self.attachment.as_ref() != Some(attachment) {
            return Err(OwnershipError::WrongAttachment);
        }
        if self.parent.is_some() && !observation {
            return Err(OwnershipError::Busy);
        }
        Ok(())
    }
    pub fn authorize_observation(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
    ) -> Result<(), OwnershipError> {
        self.active(attachment, key).map(|_| ())
    }

    pub fn record_effect(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
    ) -> Result<(), OwnershipError> {
        self.active(attachment, key)?.plan.effects_started = true;
        Ok(())
    }
    pub fn record_input_attempt(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
        port: u64,
    ) -> Result<(), OwnershipError> {
        let parent = self.active(attachment, key)?;
        parent.plan.input_ports.insert(port);
        parent.plan.effects_started = true;
        Ok(())
    }
    /// An explicit finish or disconnect freezes the obligation set before native cleanup.
    pub fn start_cleanup(
        &mut self,
        attachment: &Attachment,
        key: &OperationKey,
    ) -> Result<CleanupPlan, OwnershipError> {
        self.healthy()?;
        if let Some(parent) = self
            .parent
            .as_mut()
            .filter(|p| &p.plan.key == key && &p.plan.attachment == attachment)
        {
            parent.stopping = true;
            return Ok(parent.plan.clone());
        }
        Err(OwnershipError::NotActive)
    }
    /// Stale detach cannot alter the active route or acquire unrelated input obligations.
    pub fn detach(
        &mut self,
        attachment: &Attachment,
    ) -> Result<Option<CleanupPlan>, OwnershipError> {
        self.healthy()?;
        if self.attachment.as_ref() != Some(attachment) {
            return Ok(None);
        }
        self.attachment = None;
        self.terminal = None;
        Ok(self.parent.as_mut().map(|parent| {
            parent.stopping = true;
            parent.plan.clone()
        }))
    }
    /// A terminal result is retained for same-attachment finish retries, never reapplied.
    pub fn terminal(&self, attachment: &Attachment, key: &OperationKey) -> Option<&CleanupPlan> {
        self.terminal
            .as_ref()
            .filter(|last| &last.key == key && &last.attachment == attachment)
    }
    pub fn finish_cleanup(
        &mut self,
        plan: &CleanupPlan,
        evidence: CleanupEvidence,
    ) -> Result<(), OwnershipError> {
        self.healthy()?;
        let parent = self
            .parent
            .as_ref()
            .filter(|p| p.stopping && p.plan == *plan)
            .ok_or(OwnershipError::NotActive)?;
        let clean = (!parent.plan.effects_started || evidence.stop_verified)
            && evidence.released_ports == parent.plan.input_ports;
        if !clean {
            self.retired = true;
            return Err(OwnershipError::Retired);
        }
        let closed = self.parent.take().unwrap().plan;
        if self.attachment.as_ref() == Some(&closed.attachment) {
            self.terminal = Some(closed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
