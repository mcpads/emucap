use super::*;
struct Native {
    id: String,
    mode: &'static str,
    cancellation: RequestCancellation,
    commands: Vec<String>,
    timeout: Duration,
    queued: Option<String>,
}
impl Native {
    fn terminal(&self, done: u64) -> String {
        format!("FRAME|{}|interrupted|{done}|120|cancelled", self.id)
    }
}
impl GdbTransport for Native {
    fn send_with_timeout(
        &mut self,
        payload: &str,
        _: Duration,
    ) -> crate::gdb_rsp::GdbResult<String> {
        self.send(payload)
    }
    fn recv_reply_with_timeout(&mut self, _: Duration) -> crate::gdb_rsp::GdbResult<String> {
        self.recv_reply()
    }
    fn send(&mut self, payload: &str) -> crate::gdb_rsp::GdbResult<String> {
        self.commands.push(payload.into());
        if payload == "?" {
            return Ok("S05".into());
        }
        if payload == "qEmucap,features" {
            return Ok(if self.mode == "old" {
                ""
            } else {
                "owned_frames_v1"
            }
            .into());
        }
        let mut fields = payload.splitn(3, ',');
        assert_eq!(fields.next(), Some("qEmucap"));
        let name = fields.next().unwrap();
        let argument = String::from_utf8(hex::decode(fields.next().unwrap()).unwrap()).unwrap();
        Ok(match name {
            "framebegin" => {
                let parts: Vec<_> = argument.split(':').collect();
                assert_eq!(&parts[1..], &["120", "240000"]);
                self.id = parts[0].into();
                self.cancellation.cancel();
                format!("FRAME|{}|running|0|120|none", self.id)
            }
            "framecancel" => {
                assert_eq!(argument, self.id);
                if self.mode == "wrong_id" {
                    "FRAME|wrong|stopping|3|120|cancelled".into()
                } else {
                    format!("FRAME|{}|stopping|3|120|cancelled", self.id)
                }
            }
            "framepoll" => {
                assert_eq!(argument, self.id);
                match self.mode {
                    "backwards" => self.terminal(2),
                    "ack_only" => format!("FRAME|{}|stopping|3|120|cancelled", self.id),
                    _ => {
                        self.queued = Some(self.terminal(3));
                        "T05hwbreak:34120000;idx:1;seq:1;".into()
                    }
                }
            }
            "framefinish" => {
                assert_eq!(argument, self.id);
                self.terminal(if self.mode == "changed_finish" { 2 } else { 3 })
            }
            "frame" => "45".into(),
            other => panic!("unexpected native operation {other}"),
        })
    }
    fn send_no_reply(&mut self, _: &str) -> crate::gdb_rsp::GdbResult<()> {
        panic!("unacknowledged mutation")
    }
    fn interrupt(&mut self) -> crate::gdb_rsp::GdbResult<String> {
        Ok("S05".into())
    }
    fn recv_reply(&mut self) -> crate::gdb_rsp::GdbResult<String> {
        Ok(self.queued.take().expect("queued stop successor"))
    }
    fn get_timeout(&self) -> crate::gdb_rsp::GdbResult<Duration> {
        Ok(self.timeout)
    }
    fn set_timeout(&mut self, timeout: Duration) -> crate::gdb_rsp::GdbResult<()> {
        self.timeout = timeout;
        Ok(())
    }
}
fn bridge(mode: &'static str) -> (Bridge<Native>, RequestCancellation) {
    let cancellation = RequestCancellation::default();
    let native = Native {
        id: String::new(),
        mode,
        cancellation: cancellation.clone(),
        commands: vec![],
        timeout: Duration::from_secs(5),
        queued: None,
    };
    (Bridge::new(native, Default::default()), cancellation)
}
#[test]
fn native_stop_is_verified_before_finish_and_breakpoint_event_is_retained() {
    let (mut bridge, cancel) = bridge("valid");
    let result = bridge.owned_frame_step(120, cancel).unwrap();
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["completed"], 3);
    assert_eq!(result["frame"], 45);
    assert!(bridge.frozen && !bridge.backend_terminal());
    assert_eq!(bridge.events.len(), 1);
    assert!(bridge.events[0]["raw"]
        .as_str()
        .unwrap()
        .contains("hwbreak"));
    assert_eq!(bridge.gdb.timeout, Duration::from_secs(5));
}
#[test]
fn mismatched_progress_or_missing_terminal_retires_owned_frame_control() {
    for mode in ["wrong_id", "backwards", "changed_finish", "ack_only"] {
        let (mut bridge, cancel) = bridge(mode);
        assert!(bridge.owned_frame_step(120, cancel).is_err(), "{mode}");
        assert!(bridge.backend_terminal(), "{mode}");
        assert_eq!(bridge.gdb.timeout, Duration::from_secs(5));
    }
}
#[test]
fn old_host_and_precancel_reject_before_frame_admission() {
    for mode in ["old", "precancel"] {
        let (mut bridge, cancel) = bridge(mode);
        if mode == "precancel" {
            cancel.cancel();
        }
        assert!(bridge.owned_frame_step(120, cancel).is_err());
        assert!(!bridge.backend_terminal());
        assert!(!bridge
            .gdb
            .commands
            .iter()
            .any(|s| s.starts_with("qEmucap,framebegin,")));
    }
}
