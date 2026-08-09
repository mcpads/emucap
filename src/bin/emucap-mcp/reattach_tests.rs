use super::*;
use emucap::live::link::{Capabilities, EmulatorIdentity};

struct ReattachLink {
    capabilities: Capabilities,
    reattach_error: Option<LinkError>,
}

impl ReattachLink {
    fn ready() -> Self {
        Self {
            capabilities: Capabilities::empty(),
            reattach_error: None,
        }
    }
}

impl EmulatorLink for ReattachLink {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn call(
        &mut self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        assert_eq!(method, "status");
        Ok(serde_json::json!({"connected": true, "state": "frozen"}))
    }

    fn reattach_runtime(&mut self, launch_id: &str) -> Result<serde_json::Value, LinkError> {
        if let Some(error) = self.reattach_error.take() {
            return Err(error);
        }
        self.capabilities.identity = EmulatorIdentity {
            launch_id: Some(launch_id.to_string()),
            ..Default::default()
        };
        Ok(serde_json::json!({
            "launch_id": launch_id,
            "listening_port": 47800,
            "connection": "pending",
        }))
    }
}

#[test]
fn exact_generation_is_reported_only_after_live_identity_matches() {
    let mut link = ReattachLink::ready();
    let args = ReattachArgs {
        launch_id: "launch-returned".into(),
    };

    let result = make_reattach(&mut link, &args);

    assert_eq!(result["reattached"], true);
    assert_eq!(result["launch_id"], "launch-returned");
    assert_eq!(result["status"]["state"], "frozen");
}

#[test]
fn rejected_handoff_is_fail_closed_and_actionable() {
    let mut link = ReattachLink {
        capabilities: Capabilities::empty(),
        reattach_error: Some(LinkError::Busy),
    };
    let args = ReattachArgs {
        launch_id: "launch-occupied".into(),
    };

    let result = make_reattach(&mut link, &args);

    assert_eq!(result["reattached"], false);
    assert_eq!(result["error_kind"], "busy");
    assert!(result["next_action"].as_str().is_some());
}
