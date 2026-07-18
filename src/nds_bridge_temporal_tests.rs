use super::*;
use crate::pc98_bridge::BridgeError as GdbError;
use std::time::{Duration, Instant};

struct DelayedStepGdb {
    timeout: Duration,
    step_delay: Duration,
    issued_steps: u64,
    timeout_history: Vec<Duration>,
}

impl DelayedStepGdb {
    fn new(step_delay: Duration) -> Self {
        Self {
            timeout: Duration::from_secs(1),
            step_delay,
            issued_steps: 0,
            timeout_history: Vec::new(),
        }
    }

    fn delayed_step(&mut self) -> Result<String, GdbError> {
        self.issued_steps += 1;
        let wait = self.step_delay.min(self.timeout);
        std::thread::sleep(wait);
        if self.timeout < self.step_delay {
            return Err(GdbError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "delayed GDB step exceeded its clipped timeout",
            )));
        }
        Ok("S05".into())
    }
}

impl GdbTransport for DelayedStepGdb {
    fn send(&mut self, payload: &str) -> Result<String, GdbError> {
        match payload {
            "?" => Ok("S05".into()),
            "s" => self.delayed_step(),
            other => Err(GdbError::Emulator(format!(
                "unexpected delayed GDB call: {other}"
            ))),
        }
    }

    fn send_no_reply(&mut self, _payload: &str) -> Result<(), GdbError> {
        Ok(())
    }

    fn interrupt(&mut self) -> Result<String, GdbError> {
        Ok("S02".into())
    }

    fn get_timeout(&self) -> Result<Duration, GdbError> {
        Ok(self.timeout)
    }

    fn set_timeout(&mut self, timeout: Duration) -> Result<(), GdbError> {
        self.timeout = timeout;
        self.timeout_history.push(timeout);
        Ok(())
    }
}

#[test]
fn delayed_backend_cannot_turn_a_partial_nds_step_into_completion() {
    let mut bridge = NdsBridge::new(
        DelayedStepGdb::new(Duration::from_millis(70)),
        None,
        BridgeEnv::default(),
    );
    let started = Instant::now();
    let error = bridge
        .step_cpu_with_budget(&json!({}), 3, Duration::from_millis(120))
        .unwrap_err();

    assert!(started.elapsed() < Duration::from_millis(250));
    assert!(error.to_string().contains("after 1 acknowledged of 3"));
    assert_eq!(bridge.arm9.gdb.issued_steps, 2);
    assert!(bridge.arm9.frozen);
    assert_eq!(bridge.arm9.gdb.timeout, Duration::from_secs(1));
    assert!(bridge
        .arm9
        .gdb
        .timeout_history
        .iter()
        .any(|timeout| *timeout < Duration::from_millis(70)));
}
