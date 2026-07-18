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
