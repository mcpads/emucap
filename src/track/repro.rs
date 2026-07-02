use crate::track::model::{Intervention, ReproStatus};

/// v1-a 도출 규칙: load_state 개입이 있으면 lineage가 끊겨 savestate_only,
/// 그 외에는 replayable_with_interventions. clean은 v1-b(입력 포착·replay)에서만.
pub fn derive_status(interventions: &[Intervention]) -> ReproStatus {
    if interventions.iter().any(|i| i.op == "load_state") {
        ReproStatus::SavestateOnly
    } else {
        ReproStatus::ReplayableWithInterventions
    }
}
