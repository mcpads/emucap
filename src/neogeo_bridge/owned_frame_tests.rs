use super::*;
struct Native {
    id: String,
    finished: bool,
    queued: Option<String>,
    bad_delta: bool,
    cancel: RequestCancellation,
    breakpoint: bool,
    fail_snapshot: bool,
    poisoned: bool,
}
impl Native {
    fn terminal(&self) -> String {
        format!(
            "FRAME|{}|interrupted|1|120|{}",
            self.id,
            if self.breakpoint {
                "native_stop"
            } else {
                "cancelled"
            }
        )
    }
}
impl GdbTransport for Native {
    fn is_terminal(&self) -> bool {
        self.poisoned
    }
    fn send(&mut self, _: &str) -> crate::gdb_rsp::GdbResult<String> {
        panic!("unbounded I/O")
    }
    fn send_no_reply(&mut self, _: &str) -> crate::gdb_rsp::GdbResult<()> {
        panic!("unbounded mutation")
    }
    fn interrupt(&mut self) -> crate::gdb_rsp::GdbResult<String> {
        panic!("unbounded interrupt")
    }
    fn recv_nonblocking_with_timeout(
        &mut self,
        _: Duration,
    ) -> crate::gdb_rsp::GdbResult<Option<String>> {
        Ok(None)
    }
    fn recv_reply_with_timeout(&mut self, _: Duration) -> crate::gdb_rsp::GdbResult<String> {
        Ok(self.queued.take().unwrap())
    }
    fn send_with_timeout(
        &mut self,
        payload: &str,
        _: Duration,
    ) -> crate::gdb_rsp::GdbResult<String> {
        if self.poisoned {
            return Err(GdbError::Poisoned);
        }
        if payload.starts_with("m") {
            assert!(self.finished, "snapshot read before owned finish");
            if self.fail_snapshot {
                self.poisoned = true;
                return Err(GdbError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "snapshot response lost",
                )));
            }
            return Ok("ab".into());
        }
        let mut parts = payload.splitn(3, ',');
        assert_eq!(parts.next(), Some("qEmucap"));
        let op = parts.next().unwrap();
        let arg = String::from_utf8(hex::decode(parts.next().unwrap()).unwrap()).unwrap();
        Ok(match op {
            "frame" => if self.id.is_empty() {
                "7"
            } else if self.bad_delta {
                "9"
            } else {
                "8"
            }
            .into(),
            "framebegin" => {
                self.id = arg.split(':').next().unwrap().into();
                if !self.breakpoint {
                    self.cancel.cancel();
                }
                format!("FRAME|{}|running|0|120|none", self.id)
            }
            "framecancel" => self.terminal(),
            "framepoll" => {
                self.queued = Some(self.terminal());
                "T05hwbreak:04001000;idx:1;seq:1;".into()
            }
            "framefinish" => {
                self.finished = true;
                self.terminal()
            }
            "setpoint" => {
                assert!(self.finished, "rearm before owned finish");
                "BP:2".into()
            }
            other => panic!("unexpected operation {other}"),
        })
    }
}
fn fixture(breakpoint: bool, bad_delta: bool) -> (NeoGeoBridge<Native>, RequestCancellation) {
    let cancel = RequestCancellation::default();
    let native = Native {
        id: String::new(),
        finished: false,
        queued: None,
        bad_delta,
        cancel: cancel.clone(),
        breakpoint,
        fail_snapshot: false,
        poisoned: false,
    };
    let mut bridge = NeoGeoBridge::new(native, Default::default(), "neogeo_aes").unwrap();
    bridge.owned_control = true;
    bridge.frozen = true;
    bridge.breakpoints.insert(
        1,
        NeoGeoBreakpoint {
            kind: "exec".into(),
            start: 0x100004,
            end: 0x100004,
            absolute_start: 0x100004,
            backend_kind: "bp".into(),
            backend_id: Some(1),
            snapshots: vec![NeoGeoSnapshot {
                memory_type: "ram".into(),
                address: 0,
                length: 1,
            }],
            arm_state: NeoGeoArmState::Armed,
        },
    );
    (bridge, cancel)
}
#[test]
fn owned_breakpoint_keeps_event_snapshot_and_rearm_after_native_finish() {
    let (mut bridge, cancel) = fixture(true, false);
    let result = bridge.owned_frame_step(120, cancel).unwrap();
    assert_eq!(result["reason"], "breakpoint");
    assert_eq!(result["completed"], 1);
    assert_eq!(result["frame_counter_delta"], 1);
    assert_eq!(result["event"]["snapshot"][0]["hex"], "ab");
    assert_eq!(result["event"]["rearmed"], true);
    assert_eq!(bridge.breakpoints[&1].backend_id, Some(2));
    assert_eq!(bridge.events.len(), 1);
    assert!(!bridge.backend_terminal());
}
#[test]
fn owned_cancel_verifies_clock_delta_and_rejects_inconsistent_native_progress() {
    for bad in [false, true] {
        let (mut bridge, cancel) = fixture(false, bad);
        let result = bridge.owned_frame_step(120, cancel);
        assert_eq!(result.is_err(), bad);
        assert_eq!(bridge.backend_terminal(), bad);
        if !bad {
            assert_eq!(result.unwrap()["reason"], "cancelled");
        }
    }
}
#[test]
fn lost_snapshot_transport_cannot_report_healthy_owned_completion() {
    let (mut bridge, cancel) = fixture(true, false);
    bridge.gdb.fail_snapshot = true;
    assert!(bridge.owned_frame_step(120, cancel).is_err());
    assert!(bridge.backend_terminal());
    assert!(bridge.gdb.finished);
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["rearmed"], false);
    assert!(bridge.events[0]["snapshot_error"].is_string());
}
#[test]
fn deferred_unknown_breakpoint_retires_control_during_later_observation() {
    let (mut bridge, _) = fixture(true, false);
    bridge
        .owned_stops
        .push("T05hwbreak:04001000;idx:999;seq:1;".into());
    assert!(bridge.drain_breakpoint_packets().is_err());
    assert!(bridge.backend_terminal());
    assert!(!bridge.gdb.is_terminal());
}
