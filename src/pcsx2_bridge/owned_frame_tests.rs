use super::*;
use crate::live::link::RequestCancellation;
use std::collections::VecDeque;

fn receipt(id: u64, phase: u32, count: u32, requested: u32, reason: u32) -> Vec<u8> {
    let start = if phase == 0 { 0 } else { 100 };
    let mut r = Vec::new();
    for n in [id, 2, start, start + u64::from(count)] {
        r.extend_from_slice(&n.to_le_bytes());
    }
    for n in [requested, count, phase, reason] {
        r.extend_from_slice(&n.to_le_bytes());
    }
    r
}
struct Pine {
    replies: VecDeque<(u8, Vec<u8>)>,
    token: Option<RequestCancellation>,
    budgets: Vec<Duration>,
    delay: Duration,
}
impl PineTransport for Pine {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>> {
        if request == [MSG_EMUCAP_VERSION] {
            return Ok(REQUIRED_HOST_API.to_le_bytes().to_vec());
        }
        let (opcode, reply) = self.replies.pop_front().expect("unexpected native request");
        assert_eq!(request[0], opcode);
        Ok(reply)
    }
    fn transact_with_timeout(&mut self, request: &[u8], budget: Duration) -> BridgeResult<Vec<u8>> {
        self.budgets.push(budget);
        let (opcode, reply) = self.replies.pop_front().expect("unexpected native request");
        assert_eq!(request[0], opcode);
        if opcode == BEGIN {
            if let Some(token) = &self.token {
                token.cancel();
            }
        }
        std::thread::sleep(self.delay);
        Ok(reply)
    }
}
fn bridge(replies: Vec<(u8, Vec<u8>)>) -> Pcsx2Bridge<Pine> {
    let mut b = Pcsx2Bridge::with_identity(
        Pine {
            replies: replies.into(),
            token: None,
            budgets: vec![],
            delay: Duration::ZERO,
        },
        None,
        None,
        None,
    )
    .unwrap();
    b.owned_control = true;
    b
}
#[test]
fn cancellation_preserves_partial_progress_and_waits_for_finish() {
    let token = RequestCancellation::default();
    let mut b = bridge(vec![
        (BEGIN, receipt(1, 1, 0, 120, 0)),
        (CANCEL, receipt(1, 2, 1, 120, 1)),
        (POLL, receipt(1, 3, 1, 120, 1)),
        (FINISH, receipt(1, 4, 1, 120, 1)),
        (POLL, receipt(1, 5, 1, 120, 1)),
    ]);
    b.pine.token = Some(token.clone());
    b.request_cancellation = Some(token.clone());
    let value = b.owned_frame_step(120).unwrap();
    assert_eq!(value["count"], 1);
    assert_eq!(value["advanced"], 1);
    assert_eq!(value["reason"], "cancelled");
    assert_eq!(
        b.native_stop_deadline,
        token.cancelled_at().map(|t| t + STOP_BUDGET)
    );
    assert!(b.pine.replies.is_empty() && !b.backend_terminal());
    assert!(b.pine.budgets.iter().all(|n| *n <= FRAME_EXCHANGE));
}
#[test]
fn completion_wins_cancellation_and_does_not_drain_breakpoint_events() {
    let token = RequestCancellation::default();
    let mut b = bridge(vec![
        (BEGIN, receipt(1, 3, 2, 2, 0)),
        (FINISH, receipt(1, 5, 2, 2, 0)),
    ]);
    b.pine.token = Some(token.clone());
    b.request_cancellation = Some(token);
    assert_eq!(b.owned_frame_step(2).unwrap()["status"], "completed");
    assert!(b.pine.replies.is_empty());
    let mut b = bridge(vec![
        (BEGIN, receipt(1, 3, 0, 120, 3)),
        (FINISH, receipt(1, 5, 0, 120, 3)),
    ]);
    assert_eq!(
        b.owned_frame_step(120).unwrap()["reason"],
        "external_pause_or_debugger_stop"
    );
    assert!(b.pine.replies.is_empty());
}
#[test]
fn stale_identity_counter_and_terminal_rewrites_retire_without_finishing_foreign_work() {
    for wrong in [
        receipt(2, 3, 0, 120, 1),
        {
            let mut r = receipt(1, 3, 0, 120, 1);
            r[8..16].copy_from_slice(&3u64.to_le_bytes());
            r
        },
        receipt(1, 3, 121, 120, 1),
    ] {
        let mut b = bridge(vec![(BEGIN, receipt(1, 1, 0, 120, 0)), (POLL, wrong)]);
        assert!(b.owned_frame_step(120).is_err() && b.backend_terminal());
        assert!(b.pine.replies.is_empty());
    }
    let mut b = bridge(vec![
        (BEGIN, receipt(1, 3, 0, 120, 1)),
        (FINISH, receipt(1, 5, 1, 120, 1)),
    ]);
    assert!(b.owned_frame_step(120).is_err() && b.backend_terminal());
}
#[test]
fn no_entry_cancellation_dispatches_nothing_and_keeps_its_cleanup_origin() {
    let token = RequestCancellation::default();
    token.cancel();
    let mut b = bridge(vec![]);
    b.request_cancellation = Some(token.clone());
    assert!(matches!(
        b.owned_frame_step(120),
        Err(Pcsx2BridgeError::Cancelled)
    ));
    assert!(!b.backend_terminal());
    assert_eq!(
        b.native_stop_deadline,
        token.cancelled_at().map(|t| t + STOP_BUDGET)
    );
}
#[test]
fn pause_proves_unwind_without_a_frame_begin() {
    let mut b = bridge(vec![
        (PAUSE, receipt(1, 0, 0, 0, 0)),
        (POLL, receipt(1, 3, 0, 0, 0)),
        (FINISH, receipt(1, 5, 0, 0, 0)),
    ]);
    b.verify_owned_pause(Instant::now() + STOP_BUDGET).unwrap();
    assert!(b.pine.replies.is_empty() && !b.backend_terminal());
}
#[test]
fn expired_cleanup_and_late_receipt_cannot_renew_control() {
    let mut b = bridge(vec![]);
    assert!(
        b.verify_owned_pause(Instant::now() - Duration::from_millis(1))
            .is_err()
            && b.backend_terminal()
    );
    let mut b = bridge(vec![(BEGIN, receipt(1, 3, 2, 2, 0))]);
    b.pine.delay = FRAME_EXCHANGE + Duration::from_millis(10);
    assert!(b.owned_frame_step(2).is_err() && b.backend_terminal());
    assert!(b.pine.replies.is_empty());
}
#[test]
fn unsupported_extension_is_not_advertised() {
    let mut b = bridge(vec![(PROTOCOL, vec![0; 16])]);
    b.owned_control = false;
    assert!(b.enable_owned_control().is_err() && !b.owned_control);
}

#[test]
fn parent_release_requires_input_readback_and_retains_terminal() {
    use crate::live::control_session::{Attachment, EventKind, SessionEvent};
    use crate::live::reconnect::cancellation::OperationKey;
    for stuck in [false, true] {
        let mut input = vec![0; 16];
        input[0] = u8::from(stuck);
        let mut b = bridge(vec![
            (PAUSE, receipt(1, 3, 0, 0, 0)),
            (FINISH, receipt(1, 5, 0, 0, 0)),
            (MSG_EMUCAP_SET_INPUT, vec![]),
            (MSG_EMUCAP_INPUT_STATUS, input),
        ]);
        b.launch_id = Some("runtime".into());
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
        let deadline = Instant::now() + STOP_BUDGET;
        b.native_stop_deadline = Some(deadline);
        b.begin_temporal_operation(&attachment, key.clone())
            .unwrap();
        assert_eq!(b.native_stop_deadline, Some(deadline));
        let result = b.finish_temporal_operation(&attachment, &key);
        assert_eq!(result.is_err(), stuck);
        assert_eq!(b.backend_terminal(), stuck);
        if stuck {
            assert!(b.finish_temporal_operation(&attachment, &key).is_err());
        } else {
            let terminal = result.unwrap();
            assert_eq!(terminal["released_ports"], json!([0]));
            assert_eq!(terminal["cleanup_verified"], true);
            assert_eq!(
                b.finish_temporal_operation(&attachment, &key).unwrap(),
                terminal
            );
        }
        assert!(b.pine.replies.is_empty());
    }
}

#[test]
fn unscoped_commands_keep_normal_transport_budget_and_cleanup_can_run_after_cancel() {
    let mut b = bridge(vec![(MSG_EMUCAP_SET_INPUT, vec![])]);
    b.command(MSG_EMUCAP_SET_INPUT, &0u32.to_le_bytes())
        .unwrap();
    assert!(b.pine.budgets.is_empty());
    let token = RequestCancellation::default();
    token.cancel();
    b.request_cancellation = Some(token.clone());
    b.pine.replies.push_back((MSG_EMUCAP_SET_INPUT, vec![]));
    b.command(MSG_EMUCAP_SET_INPUT, &0u32.to_le_bytes())
        .unwrap();
    assert_eq!(b.pine.budgets.len(), 1);
    assert_eq!(
        b.native_stop_deadline,
        token.cancelled_at().map(|t| t + STOP_BUDGET)
    );
    assert!(!b.backend_terminal());
}
