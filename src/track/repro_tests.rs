use super::model::*;
use super::repro::derive_status;

fn iv(op: &str) -> Intervention {
    Intervention {
        id: "x".into(),
        seq: 0,
        at_frame: None,
        at_event: None,
        frozen_context: false,
        op: op.into(),
        args: serde_json::Value::Null,
        created_at: "t".into(),
    }
}

#[test]
fn load_state_intervention_means_savestate_only() {
    let s = derive_status(&[iv("write_memory"), iv("load_state")]);
    assert_eq!(s, ReproStatus::SavestateOnly);
}

#[test]
fn non_load_interventions_are_replayable() {
    let s = derive_status(&[iv("write_memory"), iv("reset")]);
    assert_eq!(s, ReproStatus::ReplayableWithInterventions);
}

#[test]
fn empty_interventions_default_replayable_in_v1a() {
    // v1-a는 입력 movie를 포착하지 못하므로 clean으로 도출하지 않는다.
    let s = derive_status(&[]);
    assert_eq!(s, ReproStatus::ReplayableWithInterventions);
}
