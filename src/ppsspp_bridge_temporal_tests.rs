use super::*;
use std::time::{Duration, Instant};

struct DelayedStepWs {
    step_delay: Duration,
    issued_steps: u64,
    step_timeouts: Vec<Duration>,
    halted: bool,
}

impl DelayedStepWs {
    fn new(step_delay: Duration) -> Self {
        Self {
            step_delay,
            issued_steps: 0,
            step_timeouts: Vec::new(),
            halted: true,
        }
    }
}

impl WsTransport for DelayedStepWs {
    fn call(&mut self, event: &str, _params: Value) -> BridgeResult<Value> {
        Err(BridgeError::Emulator(format!(
            "unexpected unbounded WS call: {event}"
        )))
    }

    fn call_and_wait_for(
        &mut self,
        event: &str,
        _params: Value,
        _expect_event: &str,
    ) -> BridgeResult<Value> {
        Err(BridgeError::Emulator(format!(
            "unexpected unbounded WS step call: {event}"
        )))
    }

    fn call_and_wait_for_with_timeout(
        &mut self,
        event: &str,
        _params: Value,
        expect_event: &str,
        timeout: Duration,
    ) -> BridgeResult<Value> {
        assert_eq!(event, "cpu.stepInto");
        assert_eq!(expect_event, "cpu.stepping");
        self.issued_steps += 1;
        self.step_timeouts.push(timeout);
        let wait = self.step_delay.min(timeout);
        std::thread::sleep(wait);
        if timeout < self.step_delay {
            return Err(BridgeError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "delayed WS step exceeded its clipped timeout",
            )));
        }
        Ok(json!({"event": "cpu.stepping"}))
    }

    fn call_with_timeout(
        &mut self,
        event: &str,
        _params: Value,
        _timeout: Duration,
    ) -> BridgeResult<Value> {
        match event {
            "cpu.status" => Ok(json!({"event": "cpu.status", "stepping": self.halted})),
            "cpu.getAllRegs" => Ok(json!({
                "event": "cpu.getAllRegs",
                "categories": [{
                    "id": 0,
                    "registerNames": ["pc"],
                    "uintValues": [0x0880_4004u64],
                }],
            })),
            other => Err(BridgeError::Emulator(format!(
                "unexpected bounded WS call: {other}"
            ))),
        }
    }

    fn call_ticketed(&mut self, event: &str, _params: Value, _ticket: &str) -> BridgeResult<Value> {
        Err(BridgeError::Emulator(format!(
            "unexpected ticketed WS call: {event}"
        )))
    }

    fn drain_events(&mut self) -> Vec<Value> {
        Vec::new()
    }
}

#[test]
fn delayed_backend_cannot_turn_a_partial_ppsspp_step_into_completion() {
    let mut bridge = PpssppBridge::new(DelayedStepWs::new(Duration::from_millis(70)));
    let started = Instant::now();
    let error = bridge
        .step_instructions_with_budget(&json!({"count": 3}), Duration::from_millis(120))
        .unwrap_err();

    assert!(started.elapsed() < Duration::from_millis(250));
    assert!(error.to_string().contains("after 1 acknowledged of 3"));
    assert_eq!(bridge.ws.issued_steps, 2);
    assert!(bridge.ws.halted);
    assert_eq!(bridge.ws.step_timeouts.len(), 2);
    assert!(bridge.ws.step_timeouts[1] < Duration::from_millis(70));
}
