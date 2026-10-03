use super::FeatureCapabilities;
use serde_json::json;

#[test]
fn flycast_requires_native_renderer_fence_independent_of_build_label() {
    for features in [
        json!(null),
        json!([]),
        json!(["controlled_start"]),
        json!("native_renderer_fence"),
        json!({"native_renderer_fence": true}),
    ] {
        let hello = json!({
            "adapter": "flycast-native", "build": "future-build+fence-claimed",
            "host_features": features,
        });
        let error = FeatureCapabilities::from_hello(&hello, &[], &[]).unwrap_err();
        assert!(error.to_string().contains("flycast-patch-required"));
    }
    assert!(
        FeatureCapabilities::from_hello(&json!({"adapter":"flycast-native"}), &[], &[],).is_err()
    );
    assert!(FeatureCapabilities::from_hello(
        &json!({"adapter":"flycast-native", "host_features":["native_renderer_fence"]}),
        &[],
        &[],
    )
    .is_ok());
}

#[test]
fn renderer_fence_requirement_is_specific_to_flycast() {
    for hello in [json!({}), json!({"adapter":"other-native"})] {
        assert!(FeatureCapabilities::from_hello(&hello, &[], &[]).is_ok());
    }
}
