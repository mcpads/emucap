//! Exact rendered-frame callback barrier for the N64 frontend.

use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{N64Error, N64Result, FRAME_COUNT, FRAME_SEEN, SCREENSHOT_RESULT};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum FrameGateTrigger {
    #[default]
    NextFrame,
    ScreenshotCompleted,
}

#[derive(Default)]
struct FrameGate {
    arm_next: bool,
    trigger: FrameGateTrigger,
    blocked: bool,
    shutdown: bool,
    frame: u64,
    debug_update: u64,
}

pub(super) enum FrameWaitOutcome {
    Frame(u64),
    DebugUpdate(u64),
}

fn frame_gate() -> &'static (Mutex<FrameGate>, Condvar) {
    static GATE: OnceLock<(Mutex<FrameGate>, Condvar)> = OnceLock::new();
    GATE.get_or_init(|| (Mutex::new(FrameGate::default()), Condvar::new()))
}

pub(super) fn arm_frame_gate(trigger: FrameGateTrigger) -> N64Result<()> {
    let (lock, _) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    if gate.shutdown {
        return Err(N64Error::BadState("N64 frame gate is shutting down".into()));
    }
    if gate.arm_next {
        return Err(N64Error::BadState("N64 frame gate is already armed".into()));
    }
    gate.arm_next = true;
    gate.trigger = trigger;
    Ok(())
}

#[cfg(test)]
pub(super) fn wait_frame_gate(timeout: Duration) -> N64Result<u64> {
    match wait_frame_gate_or_debug_update(timeout, u64::MAX)? {
        FrameWaitOutcome::Frame(frame) => Ok(frame),
        FrameWaitOutcome::DebugUpdate(_) => unreachable!("debug updates are disabled"),
    }
}

pub(super) fn wait_frame_gate_or_debug_update(
    timeout: Duration,
    after_debug_update: u64,
) -> N64Result<FrameWaitOutcome> {
    let (lock, condvar) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    let deadline = Instant::now() + timeout;
    while !gate.blocked && gate.debug_update <= after_debug_update {
        let now = Instant::now();
        if now >= deadline || gate.shutdown {
            gate.arm_next = false;
            gate.trigger = FrameGateTrigger::NextFrame;
            return Err(N64Error::Timeout("exact frame callback barrier"));
        }
        let remaining = deadline.saturating_duration_since(now).min(timeout);
        let (next, _) = condvar
            .wait_timeout(gate, remaining)
            .unwrap_or_else(|error| error.into_inner());
        gate = next;
    }
    if gate.blocked {
        Ok(FrameWaitOutcome::Frame(gate.frame))
    } else {
        Ok(FrameWaitOutcome::DebugUpdate(gate.debug_update))
    }
}

pub(super) fn release_frame_gate() -> N64Result<u64> {
    let (lock, condvar) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    if !gate.blocked {
        return Err(N64Error::BadState(
            "N64 frame barrier is not currently frozen".into(),
        ));
    }
    let frame = gate.frame;
    gate.blocked = false;
    condvar.notify_all();
    Ok(frame)
}

pub(super) fn frame_gate_is_blocked() -> bool {
    frame_gate()
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .blocked
}

pub(super) fn cancel_frame_gate() {
    let (lock, condvar) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    gate.arm_next = false;
    gate.trigger = FrameGateTrigger::NextFrame;
    gate.blocked = false;
    condvar.notify_all();
}

pub(super) fn reset_frame_gate() {
    let (lock, condvar) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    gate.arm_next = false;
    gate.trigger = FrameGateTrigger::NextFrame;
    gate.blocked = false;
    gate.shutdown = false;
    gate.frame = 0;
    gate.debug_update = 0;
    condvar.notify_all();
}

pub(super) fn notify_debug_update(update: u64) {
    let (lock, condvar) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    gate.debug_update = update;
    condvar.notify_all();
}

pub(super) fn shutdown_frame_gate() {
    let (lock, condvar) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    gate.arm_next = false;
    gate.trigger = FrameGateTrigger::NextFrame;
    gate.blocked = false;
    gate.shutdown = true;
    condvar.notify_all();
}

pub(super) extern "C" fn frame_callback(frame: u32) {
    // Mupen64Plus numbers the first completed frame as zero. Expose a
    // completed-frame count so the initial value and first callback differ.
    let completed = u64::from(frame) + 1;
    FRAME_COUNT.store(completed, Ordering::Release);
    FRAME_SEEN.store(true, Ordering::Release);

    let (lock, condvar) = frame_gate();
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    let trigger_reached = match gate.trigger {
        FrameGateTrigger::NextFrame => true,
        FrameGateTrigger::ScreenshotCompleted => SCREENSHOT_RESULT.load(Ordering::Acquire) != -1,
    };
    if gate.arm_next && trigger_reached && !gate.shutdown {
        gate.arm_next = false;
        gate.trigger = FrameGateTrigger::NextFrame;
        gate.blocked = true;
        gate.frame = completed;
        condvar.notify_all();
        while gate.blocked && !gate.shutdown {
            gate = condvar
                .wait(gate)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}
