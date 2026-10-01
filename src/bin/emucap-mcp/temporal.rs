use crate::request_ownership::{Admission, CancelOnDrop};
use crate::{
    args::{StepArgs, TapArgs},
    error_result, link_error_result, tool_output_result, SharedLink,
};
use emucap::live::{
    link::{EmulatorLink, LinkError, RequestCancellation},
    tools::{self, ToolOutput},
};
use rmcp::{
    model::CallToolResult,
    service::{RequestContext, RoleServer},
};
use std::sync::TryLockError;

pub enum Operation {
    Step(StepArgs),
    Tap(TapArgs),
}
impl Operation {
    fn method(&self) -> &'static str {
        match self {
            Self::Step(args) if args.unit == tools::StepUnit::Instructions => "step_instructions",
            _ => "step",
        }
    }
    fn run(
        self,
        link: &mut dyn EmulatorLink,
        cancellation: RequestCancellation,
    ) -> Result<ToolOutput, LinkError> {
        if cancellation.is_cancelled() {
            return Err(LinkError::Cancelled);
        }
        crate::ensure_capabilities_loaded(link)?;
        if cancellation.is_cancelled() {
            return Err(LinkError::Cancelled);
        }
        let controlled = link
            .capabilities()
            .features
            .temporal_cancellation
            .as_ref()
            .is_some_and(|cap| cap.methods.iter().any(|method| method == self.method()));
        let owner = if controlled {
            Some(
                link.attachment_id()
                    .ok_or_else(|| {
                        LinkError::Protocol("temporal attachment identity is unavailable".into())
                    })?
                    .to_string(),
            )
        } else {
            None
        };
        match (self, owner) {
            (Self::Step(a), Some(owner)) => tools::step_with_cancellation(
                link,
                a.count,
                a.unit,
                a.cpu.as_deref(),
                cancellation,
                &owner,
            ),
            (Self::Tap(a), Some(owner)) => tools::tap_with_cancellation(
                link,
                a.port,
                &a.buttons,
                a.press_frames,
                a.after_frames,
                cancellation,
                &owner,
            ),
            (Self::Step(a), None) => tools::step(link, a.count, a.unit, a.cpu.as_deref()),
            (Self::Tap(a), None) => {
                tools::tap(link, a.port, &a.buttons, a.press_frames, a.after_frames)
            }
        }
    }
}

pub async fn execute(
    link: SharedLink,
    operation: Operation,
    context: RequestContext<RoleServer>,
) -> CallToolResult {
    let admission = Admission::take(&context);
    let cancellation = Admission::cancellation(&context);
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    if context.ct.is_cancelled() {
        cancellation.cancel();
    }
    let worker_cancellation = cancellation.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        let mut link = match link.try_lock() {
            Ok(link) => link,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(LinkError::Busy),
        };
        operation.run(&mut *link, worker_cancellation)
    });
    let mut cancelled = false;
    loop {
        tokio::select! {
            result=&mut worker => return match result {
                Ok(Ok(output))=>tool_output_result(output),
                Ok(Err(error))=>link_error_result(error),
                Err(error)=>error_result("temporal_worker",error),
            },
            _=context.ct.cancelled(), if !cancelled => { cancellation.cancel(); cancelled=true; },
        }
    }
}

#[cfg(test)]
#[path = "temporal/tests.rs"]
pub(crate) mod tests;
