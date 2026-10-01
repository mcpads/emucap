use super::{finish_with_cleanup, OperationDeadline};
use std::time::Duration;

fn combine(primary: Option<&'static str>, cleanup: &'static str) -> &'static str {
    match primary {
        Some(_) => "primary+cleanup",
        None => cleanup,
    }
}

#[test]
fn successful_effect_is_not_completed_when_cleanup_fails() {
    assert_eq!(
        finish_with_cleanup(Ok::<_, &'static str>(7), Err("cleanup"), combine),
        Err("cleanup")
    );
}

#[test]
fn dual_failure_keeps_both_failure_classes() {
    assert_eq!(
        finish_with_cleanup::<(), _>(Err("primary"), Err("cleanup"), combine),
        Err("primary+cleanup")
    );
}

#[test]
fn primary_failure_survives_successful_cleanup() {
    assert_eq!(
        finish_with_cleanup::<(), _>(Err("primary"), Ok(()), combine),
        Err("primary")
    );
}

#[test]
fn operation_deadline_expires_and_never_returns_a_zero_socket_timeout() {
    let deadline = OperationDeadline::after(Duration::from_millis(5));
    assert!(deadline
        .remaining_timeout()
        .is_some_and(|value| !value.is_zero()));
    std::thread::sleep(Duration::from_millis(10));
    assert!(deadline.expired());
    assert_eq!(deadline.remaining_timeout(), None);
}

#[test]
fn cancellation_advertisement_pairs_methods_and_validates_stop_bounds() {
    use crate::live::link::FeatureCapabilities;
    use serde_json::json;
    let methods = vec!["step".into(), "cancel_operation".into()];
    let valid = json!({"methods":["step"],"control_service_ms":25,"stop_host_ms":500});
    let hello = json!({"temporal_cancellation_capability":valid});
    assert!(FeatureCapabilities::from_hello(&hello, &methods, &[])
        .unwrap()
        .temporal_cancellation
        .is_some());
    assert!(FeatureCapabilities::from_hello(&json!({}), &methods, &[]).is_err());
    assert!(FeatureCapabilities::from_hello(&hello, &["step".into()], &[]).is_err());
    for invalid in [
        json!({"methods":[],"control_service_ms":25,"stop_host_ms":500}),
        json!({"methods":["step","step"],"control_service_ms":25,"stop_host_ms":500}),
        json!({"methods":["step_instructions"],"control_service_ms":25,"stop_host_ms":500}),
        json!({"methods":["step"],"control_service_ms":0,"stop_host_ms":500}),
        json!({"methods":["step"],"control_service_ms":25,"stop_host_ms":24}),
    ] {
        assert!(FeatureCapabilities::from_hello(
            &json!({"temporal_cancellation_capability":invalid}),
            &methods,
            &[]
        )
        .is_err());
    }
}

#[test]
fn repeated_cancellation_keeps_the_first_host_time_across_clones() {
    let cancellation = crate::live::link::RequestCancellation::default();
    assert_eq!(cancellation.cancelled_at(), None);
    cancellation.cancel();
    let first = cancellation.cancelled_at().unwrap();
    let clone = cancellation.clone();
    clone.cancel();
    assert!(clone.is_cancelled());
    assert_eq!(clone.cancelled_at(), Some(first));
}
