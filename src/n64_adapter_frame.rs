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
    resume_hook: Option<unsafe extern "C" fn()>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FrameWaitOutcome {
    Frame(u64),
    DebugUpdate(u64),
    Cancelled,
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
    match wait_frame_gate_or_debug_update(timeout, u64::MAX, None)? {
        FrameWaitOutcome::Frame(frame) => Ok(frame),
        FrameWaitOutcome::DebugUpdate(_) => unreachable!("debug updates are disabled"),
        FrameWaitOutcome::Cancelled => unreachable!("cancellation is not supplied"),
    }
}

pub(super) fn wait_frame_gate_or_debug_update(
    timeout: Duration,
    after_debug_update: u64,
    cancellation: Option<&crate::live::link::RequestCancellation>,
) -> N64Result<FrameWaitOutcome> {
    wait_for_gate(frame_gate(), timeout, after_debug_update, cancellation)
}

fn wait_for_gate(
    gate_pair: &(Mutex<FrameGate>, Condvar),
    timeout: Duration,
    after_debug_update: u64,
    cancellation: Option<&crate::live::link::RequestCancellation>,
) -> N64Result<FrameWaitOutcome> {
    let (lock, condvar) = gate_pair;
    let mut gate = lock.lock().unwrap_or_else(|error| error.into_inner());
    let deadline = Instant::now() + timeout;
    loop {
        // Cancellation reports intent only. Keep any held/armed barrier intact for
        // the owner to establish and verify its native stop before acknowledging.
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(FrameWaitOutcome::Cancelled);
        }
        if gate.blocked || gate.debug_update > after_debug_update {
            break;
        }
        let now = Instant::now();
        if now >= deadline || gate.shutdown {
            gate.arm_next = false;
            gate.trigger = FrameGateTrigger::NextFrame;
            return Err(N64Error::Timeout("exact frame callback barrier"));
        }
        let mut remaining = deadline.saturating_duration_since(now).min(timeout);
        if cancellation.is_some() {
            remaining = remaining.min(Duration::from_millis(25));
        }
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

/// Establish a stop without releasing an already held callback barrier. A native
/// pause flag alone is insufficient: a new debugger callback must acknowledge it.
pub(super) fn park_after_cancellation(
    after_debug_update: u64,
    budget: Duration,
    request_pause: impl FnOnce() -> N64Result<()>,
    debugger_paused: impl FnMut() -> bool,
) -> N64Result<FrameWaitOutcome> {
    park_cancelled_at(
        frame_gate(),
        after_debug_update,
        budget,
        request_pause,
        debugger_paused,
    )
}

fn park_cancelled_at(
    pair: &(Mutex<FrameGate>, Condvar),
    after_debug_update: u64,
    budget: Duration,
    request_pause: impl FnOnce() -> N64Result<()>,
    mut debugger_paused: impl FnMut() -> bool,
) -> N64Result<FrameWaitOutcome> {
    let deadline = Instant::now() + budget;
    let mut request_pause = Some(request_pause);
    loop {
        // Read native state outside the gate mutex; the callback may need that mutex.
        let paused = debugger_paused();
        {
            let mut gate = pair.0.lock().unwrap_or_else(|error| error.into_inner());
            if gate.shutdown {
                return Err(N64Error::BadState(
                    "N64 shut down during cancellation".into(),
                ));
            }
            if gate.blocked {
                return Ok(FrameWaitOutcome::Frame(gate.frame));
            }
            if paused && gate.debug_update > after_debug_update {
                gate.arm_next = false;
                gate.trigger = FrameGateTrigger::NextFrame;
                return Ok(FrameWaitOutcome::DebugUpdate(gate.debug_update));
            }
        }
        if Instant::now() >= deadline {
            return Err(N64Error::Timeout("cancellation native stop"));
        }
        if let Some(request) = request_pause.take() {
            request()?;
        }
        std::thread::sleep(Duration::from_millis(1));
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

pub(super) fn set_frame_resume_hook(hook: unsafe extern "C" fn()) {
    let mut gate = frame_gate()
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    gate.resume_hook = Some(hook);
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
    gate.resume_hook = None;
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
    gate.resume_hook = None;
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
        // Still on the emulation thread. Keeping the gate locked also serializes
        // shutdown/unregistration against this call into the loaded native core.
        if let Some(resume) = gate.resume_hook {
            unsafe { resume() };
        }
    }
}

pub(super) fn validate_observed_frame(
    trigger: FrameGateTrigger,
    observed_before_verified: bool,
    observed_before: u64,
    observed: u64,
) -> N64Result<()> {
    if trigger == FrameGateTrigger::NextFrame
        && observed_before_verified
        && observed != observed_before + 1
    {
        return Err(N64Error::BadState(format!(
            "N64 frame step mismatch: expected {}, observed {observed}",
            observed_before + 1
        )));
    }
    if observed_before_verified && observed <= observed_before {
        return Err(N64Error::BadState(format!(
            "N64 frame boundary did not advance: before {observed_before}, observed {observed}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::live::link::RequestCancellation;

    #[test]
    fn cancelled_stop_requires_callback_proof_and_never_releases_a_held_gate() {
        let held = (
            Mutex::new(FrameGate {
                blocked: true,
                frame: 9,
                ..Default::default()
            }),
            Condvar::new(),
        );
        assert_eq!(
            park_cancelled_at(
                &held,
                4,
                Duration::from_secs(1),
                || panic!("held frame must not request another pause"),
                || false
            )
            .unwrap(),
            FrameWaitOutcome::Frame(9)
        );
        assert!(held.0.lock().unwrap().blocked);

        let pending = (
            Mutex::new(FrameGate {
                arm_next: true,
                debug_update: 4,
                ..Default::default()
            }),
            Condvar::new(),
        );
        assert!(
            park_cancelled_at(&pending, 4, Duration::from_millis(5), || Ok(()), || true).is_err()
        );
        assert!(
            pending.0.lock().unwrap().arm_next,
            "unverified stop must preserve the gate"
        );
        assert_eq!(
            park_cancelled_at(
                &pending,
                4,
                Duration::from_secs(1),
                || {
                    pending.0.lock().unwrap().debug_update = 5;
                    Ok(())
                },
                || true
            )
            .unwrap(),
            FrameWaitOutcome::DebugUpdate(5)
        );
        assert!(!pending.0.lock().unwrap().arm_next);
    }

    #[test]
    fn cancellation_preserves_armed_and_held_native_barriers() {
        for blocked in [false, true] {
            let pair = (
                Mutex::new(FrameGate {
                    arm_next: true,
                    blocked,
                    frame: 37,
                    debug_update: 9,
                    ..Default::default()
                }),
                Condvar::new(),
            );
            let token = RequestCancellation::default();
            token.cancel();
            assert_eq!(
                wait_for_gate(&pair, Duration::from_secs(30), 0, Some(&token)).unwrap(),
                FrameWaitOutcome::Cancelled
            );
            let gate = pair.0.lock().unwrap();
            assert!(gate.arm_next);
            assert_eq!(gate.blocked, blocked);
            assert_eq!((gate.frame, gate.debug_update), (37, 9));
        }
    }

    #[test]
    fn cancellation_wakes_a_slow_frame_wait_without_a_guest_callback() {
        let pair = (
            Mutex::new(FrameGate {
                arm_next: true,
                ..Default::default()
            }),
            Condvar::new(),
        );
        let token = RequestCancellation::default();
        std::thread::scope(|scope| {
            let waiter =
                scope.spawn(|| wait_for_gate(&pair, Duration::from_secs(2), 0, Some(&token)));
            std::thread::sleep(Duration::from_millis(20));
            token.cancel();
            let cancelled_at = Instant::now();
            assert_eq!(waiter.join().unwrap().unwrap(), FrameWaitOutcome::Cancelled);
            assert!(cancelled_at.elapsed() < Duration::from_secs(1));
        });
        assert!(pair.0.lock().unwrap().arm_next);
    }
}
