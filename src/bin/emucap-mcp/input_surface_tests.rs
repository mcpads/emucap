use super::*;

fn xbox_axis_status() -> Value {
    serde_json::json!({
        "contracts": {"state":"validated"},
        "capability_revision":"revision-xbox",
        "methods":["set_input"],
        "input_buttons":{"buttons":["a"]},
        "input_axes": {
            "ports":[{
                "port":0,
                "axes":{
                    "left_x":{"minimum":-32768, "neutral":0, "maximum":32767},
                    "right_trigger":{"minimum":0, "neutral":0, "maximum":32767}
                }
            }]
        }
    })
}

#[test]
fn describe_projects_live_axis_capability_next_to_the_set_input_schema() {
    let status = xbox_axis_status();
    let description = describe(&status);
    assert_eq!(description["input_axes"], status["input_axes"]);
    assert!(description["operations"]["set_input"]["arguments_schema"]
        .to_string()
        .contains("axes"));
}

#[test]
fn axis_validation_accepts_only_the_advertised_port_names_and_ranges() {
    let status = xbox_axis_status();
    assert!(validate_controller_axes(
        &status,
        0,
        &BTreeMap::from([("left_x".into(), -32768), ("right_trigger".into(), 32767)])
    )
    .is_ok());

    for (port, axes) in [
        (1, BTreeMap::from([("left_x".into(), 0)])),
        (0, BTreeMap::from([("left_x".into(), 32768)])),
        (0, BTreeMap::from([("throttle".into(), 1)])),
    ] {
        assert!(validate_controller_axes(&status, port, &axes).is_err());
    }
    assert!(validate_controller_axes(&Value::Null, 0, &BTreeMap::new()).is_ok());
}
