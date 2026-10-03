use super::*;
use std::time::Duration;

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
}

impl GdbTransport for DelayedStepGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        match payload {
            "?" => Ok("S05".into()),
            "s" => {
                self.issued_steps += 1;
                let wait = if self.issued_steps == 1 {
                    Duration::ZERO
                } else {
                    self.timeout
                };
                std::thread::sleep(wait);
                if self.issued_steps > 1 && self.timeout < self.step_delay {
                    return Err(GdbError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "delayed GDB step exceeded its clipped timeout",
                    )));
                }
                Ok("S05".into())
            }
            other => Err(GdbError::Emulator(format!(
                "unexpected delayed GDB call: {other}"
            ))),
        }
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }

    fn get_timeout(&self) -> GdbResult<Duration> {
        Ok(self.timeout)
    }

    fn set_timeout(&mut self, timeout: Duration) -> GdbResult<()> {
        self.timeout = timeout;
        self.timeout_history.push(timeout);
        Ok(())
    }
}

#[test]
fn delayed_backend_cannot_turn_a_partial_pc98_step_into_completion() {
    let mut bridge = Bridge::new(
        DelayedStepGdb::new(Duration::from_secs(2)),
        GdbBridgeEnv::default(),
    );
    let error = bridge
        .step_instruction_count_with_budget(3, Duration::from_secs(1))
        .unwrap_err();

    assert!(error.to_string().contains("after 1 acknowledged of 3"));
    assert_eq!(bridge.gdb.issued_steps, 2);
    assert!(bridge.frozen);
    assert_eq!(bridge.gdb.timeout, Duration::from_secs(1));
    assert!(bridge
        .gdb
        .timeout_history
        .iter()
        .any(|timeout| *timeout < Duration::from_secs(2)));
}
