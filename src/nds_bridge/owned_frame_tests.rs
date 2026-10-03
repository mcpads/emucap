use super::*;
use crate::live::link::RequestCancellation;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Native {
    active: bool,
    stopped: bool,
    complete: bool,
    wrong_clock: bool,
    sibling: bool,
    emitted: bool,
    input_stuck: bool,
    interrupts: usize,
    reject_query_before_ack: bool,
}
struct Fake {
    native: Arc<Mutex<Native>>,
    sibling: bool,
    token: RequestCancellation,
}
impl GdbTransport for Fake {
    fn send(&mut self, p: &str) -> Result<String, GdbError> {
        let n = self.native.lock().unwrap();
        if n.reject_query_before_ack && n.interrupts > 0 && !n.emitted {
            return Err(GdbError::Emulator("native is awaiting stop ACK".into()));
        }
        Ok(match p {
            "?" => "S05".into(),
            "qEmucap,haltstate" => format!(
                "HALT|1|{}|{}",
                if !n.active || n.stopped {
                    "parked"
                } else {
                    "running"
                },
                if n.active {
                    if n.wrong_clock {
                        "9"
                    } else {
                        "2"
                    }
                } else {
                    "0"
                }
            ),
            "qEmucap,framestepstatus" => if n.complete {
                "completed:0,2,2,2"
            } else {
                "interrupted:0,2,4,2"
            }
            .into(),
            "QEmucap,input:0" => "OK".into(),
            "qEmucap,touchstatus" => "0".into(),
            "qEmucap,pacing" => "64,0,1,0".into(),
            "qEmucap,inputstatus" => if n.input_stuck { "-1" } else { "0" }.into(),
            _ => return Err(GdbError::Emulator(format!("unexpected: {p}"))),
        })
    }
    fn send_with_timeout(&mut self, p: &str, _: Duration) -> Result<String, GdbError> {
        self.send(p)
    }
    fn send_no_reply(&mut self, _: &str) -> Result<(), GdbError> {
        unreachable!()
    }
    fn send_no_reply_with_timeout(&mut self, p: &str, _: Duration) -> Result<(), GdbError> {
        assert!(p.starts_with("QEmucap,framestep:"));
        let mut n = self.native.lock().unwrap();
        n.active = true;
        if n.complete || n.sibling {
            n.stopped = true;
        }
        self.token.cancel();
        Ok(())
    }
    fn interrupt(&mut self) -> Result<String, GdbError> {
        unreachable!()
    }
    fn request_interrupt_with_timeout(&mut self, _: Duration) -> Result<(), GdbError> {
        let mut n = self.native.lock().unwrap();
        n.interrupts += 1;
        n.stopped = true;
        Ok(())
    }
    fn recv_nonblocking_with_timeout(&mut self, _: Duration) -> Result<Option<String>, GdbError> {
        let mut n = self.native.lock().unwrap();
        if n.active && n.stopped && !n.emitted && self.sibling == n.sibling {
            n.emitted = true;
            Ok(Some(
                if n.complete || n.sibling {
                    "S05"
                } else {
                    "S02"
                }
                .into(),
            ))
        } else {
            Ok(None)
        }
    }
}
fn fixture(native: Native) -> (NdsBridge<Fake>, Arc<Mutex<Native>>) {
    let n = Arc::new(Mutex::new(native));
    let token = RequestCancellation::default();
    let mut b = NdsBridge::new(
        Fake {
            native: n.clone(),
            sibling: false,
            token: token.clone(),
        },
        Some(Fake {
            native: n.clone(),
            sibling: true,
            token: token.clone(),
        }),
        GdbBridgeEnv::default(),
    );
    b.owned_control = true;
    b.request_cancellation = Some(token);
    (b, n)
}
#[test]
fn cancellation_stops_shared_scheduler_and_reports_exact_partial_progress() {
    let (mut b, n) = fixture(Native::default());
    let r = b.owned_frame_step(4).unwrap();
    assert_eq!(r["reason"], "cancelled");
    assert_eq!(r["count"], 2);
    assert!(b.arm9.frozen && b.arm7.as_ref().unwrap().frozen);
    assert_eq!(n.lock().unwrap().interrupts, 1);
    assert!(!b.backend_terminal());
}
#[test]
fn completion_wins_cancellation_race_without_interrupt() {
    let (mut b, n) = fixture(Native {
        complete: true,
        ..Default::default()
    });
    assert_eq!(b.owned_frame_step(2).unwrap()["status"], "completed");
    assert_eq!(n.lock().unwrap().interrupts, 0);
    assert!(b.arm9.events.is_empty());
}
#[test]
fn sibling_breakpoint_preempts_cancel_and_retains_its_cpu_event() {
    let (mut b, n) = fixture(Native {
        sibling: true,
        ..Default::default()
    });
    let r = b.owned_frame_step(4).unwrap();
    assert_eq!(r["stop_cpu"], "arm7");
    assert_eq!(r["reason"], "breakpoint");
    assert_eq!(b.arm7.as_ref().unwrap().events.len(), 1);
    assert!(b.arm9.events.is_empty());
    assert_eq!(n.lock().unwrap().interrupts, 0);
}
#[test]
fn inconsistent_native_clock_retires_control() {
    let (mut b, _) = fixture(Native {
        wrong_clock: true,
        ..Default::default()
    });
    assert!(b.owned_frame_step(4).is_err());
    assert!(b.backend_terminal());
}
#[test]
fn precancelled_request_does_not_start_native_effects() {
    let (mut b, n) = fixture(Native::default());
    b.request_cancellation.as_ref().unwrap().cancel();
    assert!(matches!(
        b.owned_frame_step(4),
        Err(NdsBridgeError::Cancelled)
    ));
    assert!(!n.lock().unwrap().active);
    assert!(!b.backend_terminal());
}
#[test]
fn halt_proof_requires_current_protocol_and_explicit_state() {
    for bad in [
        "S05",
        "HALT|2|parked|0",
        "HALT|1|unknown|0",
        "HALT|1|parked|z",
        "HALT|1|parked|0|extra",
    ] {
        assert!(parse_halt(bad).is_err());
    }
    assert_eq!(parse_halt("HALT|1|running|a").unwrap(), (false, 10));
}

#[test]
fn parent_release_requires_native_readback_even_after_ack() {
    use crate::live::control_session::{Attachment, EventKind, SessionEvent};
    use crate::live::reconnect::cancellation::OperationKey;
    for stuck in [false, true] {
        let (mut b, _) = fixture(Native {
            input_stuck: stuck,
            ..Default::default()
        });
        b.env.launch_id = Some("runtime".into());
        let attachment = Attachment {
            broker_instance: "broker".into(),
            registration: 1,
            session: 1,
        };
        b.apply_control_session(SessionEvent {
            runtime: "runtime".into(),
            attachment: attachment.clone(),
            kind: EventKind::Attach,
        })
        .unwrap();
        let key = OperationKey {
            runtime: "runtime".into(),
            owner_id: "owner".into(),
            operation_id: "parent".into(),
        };
        b.begin_temporal_operation(&attachment, key.clone())
            .unwrap();
        b.producer_ownership
            .as_mut()
            .unwrap()
            .record_input_attempt(&attachment, &key, 0)
            .unwrap();
        let result = b.finish_temporal_operation(&attachment, &key);
        assert_eq!(result.is_err(), stuck);
        assert_eq!(b.backend_terminal(), stuck);
        if !stuck {
            let terminal = result.unwrap();
            assert_eq!(terminal["released_ports"], json!([0]));
            assert_eq!(
                b.finish_temporal_operation(&attachment, &key).unwrap(),
                terminal
            );
        }
    }
}

#[test]
fn owned_status_advertises_cancellation_without_a_methods_array() {
    use crate::live::control_session::{Attachment, EventKind, SessionEvent};
    use crate::live::reconnect::owned::OwnedHandler;
    let (mut b, _) = fixture(Native::default());
    b.env.launch_id = Some("runtime".into());
    let a = Attachment {
        broker_instance: "broker".into(),
        registration: 1,
        session: 1,
    };
    b.apply_control_session(SessionEvent {
        runtime: "runtime".into(),
        attachment: a.clone(),
        kind: EventKind::Attach,
    })
    .unwrap();
    let reply = b.request(&a, Request::new(1, "status", json!({})), Default::default());
    assert!(reply.response.ok);
    assert_eq!(
        reply.response.result.unwrap()["temporal_cancellation_capability"]["methods"],
        json!(["step"])
    );
}

#[test]
fn owned_pause_acknowledges_either_cpu_stop_before_querying_halt() {
    for sibling in [false, true] {
        let (mut bridge, native) = fixture(Native {
            active: true,
            sibling,
            reject_query_before_ack: true,
            ..Default::default()
        });
        bridge.set_scheduler_frozen(false);
        bridge.verify_owned_halt().unwrap();
        let native = native.lock().unwrap();
        assert_eq!(native.interrupts, 1);
        assert!(native.emitted && bridge.primary_frozen());
        assert!(!bridge.backend_terminal());
        if sibling {
            assert_eq!(bridge.arm7.as_ref().unwrap().events.len(), 1);
        }
    }
}
