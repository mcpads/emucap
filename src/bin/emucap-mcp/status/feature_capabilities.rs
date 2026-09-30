/// Publish the hello-validated static batch/pacing domains and verify the dynamic pacing policy.
/// An invalid live policy is withheld rather than presented as the current setting.
pub(super) fn enrich_feature_capabilities(
    value: &mut serde_json::Value,
    features: &emucap::live::link::FeatureCapabilities,
) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("memory_batch_capability");
    object.remove("execution_speed_capability");
    if let Some(batch) = &features.memory_batch {
        object.insert("memory_batch_capability".into(), serde_json::json!(batch));
    }
    let Some(speed) = &features.execution_speed else {
        object.remove("execution_speed");
        return;
    };
    object.insert(
        "execution_speed_capability".into(),
        serde_json::json!(speed),
    );
    if let Some(policy) = object.get("execution_speed") {
        if let Err(error) = speed.verify_policy(policy) {
            object.remove("execution_speed");
            object.insert("execution_speed_error".into(), serde_json::json!(error));
        }
    }
}
