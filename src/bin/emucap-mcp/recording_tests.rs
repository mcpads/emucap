use rmcp::model::{NumberOrString, ProgressToken};

use super::{next_progress_value, progress_notification};
use emucap::live::link::WorkingProgress;

#[test]
fn progress_projection_is_bounded_generic_and_uses_completed_frames() {
    let progress = WorkingProgress {
        status: "working".into(),
        capture_id: "capture-01test".into(),
        sequence: 3,
        frame: 104,
        frames: Some(2),
        events: 4,
        bytes: 512,
        phase: None,
    };
    let value = next_progress_value(&progress, 120, None).expect("first progress");
    let notification = progress_notification(
        ProgressToken(NumberOrString::Number(7)),
        &progress,
        120,
        value,
    );

    assert_eq!(notification.progress, 2.0);
    assert_eq!(notification.total, Some(120.0));
    let message = notification.message.unwrap();
    assert!(message.contains("capture-01test"));
    assert!(message.contains("2 frames"));
    assert!(message.contains("4 events"));
    assert!(!message.to_ascii_lowercase().contains("snes"));
    assert!(message.len() < 160);
}

#[test]
fn progress_projection_is_strictly_increasing_on_the_mcp_wire() {
    let mut progress = WorkingProgress {
        status: "working".into(),
        capture_id: "capture-01test".into(),
        sequence: 0,
        frame: 100,
        frames: Some(0),
        events: 1,
        bytes: 64,
        phase: None,
    };
    assert_eq!(next_progress_value(&progress, 10, None), Some(0));
    assert_eq!(next_progress_value(&progress, 10, Some(0)), None);

    progress.sequence = 1;
    progress.events = 2;
    assert_eq!(next_progress_value(&progress, 10, Some(0)), None);

    progress.frames = Some(1);
    assert_eq!(next_progress_value(&progress, 10, Some(0)), Some(1));
}
