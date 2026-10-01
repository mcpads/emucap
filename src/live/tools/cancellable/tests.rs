use super::*;
use crate::live::link::{FeatureCapabilities, ProgressObserver};

struct Scripted {
    caps: Capabilities,
    calls: Vec<String>,
    keys: Vec<Value>,
    cancellation: RequestCancellation,
    cancel_phase: Option<usize>,
    interrupt: bool,
    malformed: bool,
    cancel_on_pause: bool,
    change_generation: bool,
    finished: Vec<bool>,
    port: Option<u16>,
    fail_release: bool,
    remote: Vec<String>,
    fault: Option<&'static str>,
    admitted: bool,
    ports: std::collections::BTreeSet<u64>,
    slow_release: bool,
    late_terminal: bool,
    cancel_before_input: bool,
    closed: bool,
    parent_budgets: Vec<(String, u64)>,
}
impl Scripted {
    fn new() -> Self {
        let mut caps = Capabilities::empty();
        caps.methods = vec![
            "step".into(),
            "status".into(),
            "set_input".into(),
            "pause".into(),
        ];
        caps.identity.launch_id = Some("launch-a".into());
        caps.features.temporal_cancellation = Some(CancellationCapability {
            methods: vec!["step".into()],
            control_service_ms: 25,
            stop_host_ms: 500,
        });
        Self {
            caps,
            calls: vec![],
            keys: vec![],
            cancellation: Default::default(),
            cancel_phase: None,
            interrupt: false,
            malformed: false,
            cancel_on_pause: false,
            change_generation: false,
            finished: vec![],
            port: None,
            fail_release: false,
            remote: vec![],
            fault: None,
            admitted: false,
            ports: Default::default(),
            slow_release: false,
            late_terminal: false,
            cancel_before_input: false,
            closed: false,
            parent_budgets: vec![],
        }
    }
}
impl EmulatorLink for Scripted {
    fn begin_temporal_control(&mut self, _key: &OperationKey) -> Result<(), LinkError> {
        Ok(())
    }
    fn finish_temporal_control(
        &mut self,
        _key: &OperationKey,
        verified: bool,
    ) -> Result<(), LinkError> {
        self.finished.push(verified);
        Ok(())
    }
    fn prepare_reconnect(&mut self) {
        self.closed = true;
    }
    fn endpoint_port(&self) -> Option<u16> {
        self.port
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        if matches!(
            method,
            "begin_temporal_operation" | "finish_temporal_operation"
        ) {
            self.remote.push(method.into());
            assert_eq!(params["parent"], params["_temporal_owner"]);
            if self.fault == Some(method) {
                return Err(LinkError::Protocol("lost parent acknowledgement".into()));
            }
            if method == "begin_temporal_operation" {
                self.admitted = true;
                let mut parent = params["parent"].clone();
                if self.fault == Some("wrong_admission") {
                    parent["operation_id"] = json!("other");
                }
                return Ok(json!({"status":"admitted","parent":parent}));
            }
            self.admitted = false;
            let mut terminal = json!({"status":"completed","parent":params["parent"],
                "cleanup_verified":true,"effects_started":true,"state":"frozen","released_ports":self.ports});
            match self.fault {
                Some("wrong_finish") => terminal["parent"]["owner_id"] = json!("other"),
                Some("missing_port") => terminal["released_ports"] = json!([]),
                Some("extra_port") => terminal["released_ports"] = json!([0, 1]),
                Some("duplicate_port") => terminal["released_ports"] = json!([0, 0]),
                Some("running_finish") => terminal["state"] = json!("running"),
                Some("unverified_finish") => terminal["cleanup_verified"] = json!(false),
                _ => (),
            }
            return Ok(terminal);
        }
        if method == "status" {
            assert!(params.get("_temporal_owner").is_some());
        }
        if params.get("_temporal_owner").is_some() {
            assert!(self.admitted, "effect before producer admission");
        }
        if method == "set_input" {
            self.ports
                .insert(params.get("port").and_then(Value::as_u64).unwrap_or(0));
        }
        self.calls.push(if method == "set_input" {
            if params["buttons"].as_array().unwrap().is_empty() {
                "release"
            } else {
                "press"
            }
            .into()
        } else {
            method.into()
        });
        if method == "set_input"
            && self.fail_release
            && params["buttons"].as_array().unwrap().is_empty()
        {
            return Err(LinkError::Protocol("injected release failure".into()));
        }
        if method == "pause" && self.cancel_on_pause {
            self.cancellation.cancel();
        }
        Ok(json!({"state":"frozen","frame":10}))
    }
    fn call_with_progress(
        &mut self,
        method: &str,
        params: Value,
        _: &mut ProgressObserver<'_>,
        control: &ProgressCallControl,
    ) -> Result<Value, LinkError> {
        if method != "step" {
            self.parent_budgets
                .push((method.into(), control.max_host_ms.unwrap()));
            if method == "set_input"
                && !params["buttons"].as_array().unwrap().is_empty()
                && self.cancel_before_input
            {
                self.cancellation.cancel();
                return Err(LinkError::Cancelled);
            }
            if method == "set_input"
                && params["buttons"].as_array().unwrap().is_empty()
                && self.slow_release
            {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            assert!(control.max_host_ms.unwrap() <= control.temporal_stop_ms.unwrap());
            assert!(control.temporal_deadline.is_some());
            if let Some(cancelled) = self.cancellation.cancelled_at() {
                assert_eq!(
                    control.temporal_deadline,
                    Some(
                        cancelled
                            + std::time::Duration::from_millis(control.temporal_stop_ms.unwrap())
                    )
                );
            }
            assert_eq!(
                control.abort.as_ref().unwrap().params,
                params["_temporal_owner"]
            );
            if method == "finish_temporal_operation" {
                assert!(!control.cancellation.is_cancelled());
            }
            return self.call(method, params);
        }
        assert_eq!(method, "step");
        assert!(self.admitted);
        self.calls.push(method.into());
        assert_eq!(control.abort.as_ref().unwrap().params, params["_control"]);
        assert_eq!(
            params["_control"]["runtime"],
            self.caps.identity.launch_id.as_deref().unwrap()
        );
        assert_eq!(params["_control"]["owner_id"], "owner-a");
        self.keys.push(params["_control"].clone());
        if self.change_generation {
            self.caps.identity.launch_id = Some("launch-b".into());
        }
        let cancelling = self.cancel_phase == Some(self.keys.len());
        if cancelling {
            self.cancellation.cancel();
            if self.late_terminal {
                std::thread::sleep(std::time::Duration::from_millis(75));
            }
        }
        if self.malformed {
            return Ok(json!({"status":"completed","count":999,"state":"running"}));
        }
        let count = if cancelling && self.interrupt {
            1
        } else {
            params["frames"].as_u64().unwrap()
        };
        Ok(
            json!({"status":if cancelling && self.interrupt {"interrupted"} else {"completed"},
            "reason":"cancelled","unit":"frames","count":count,"state":"frozen"}),
        )
    }
}
fn tap(link: &mut Scripted) -> Result<ToolOutput, LinkError> {
    let cancellation = link.cancellation.clone();
    tap_with_cancellation(link, 0, &["a".into()], 6, 120, cancellation, "owner-a")
}

#[test]
fn press_cancellation_releases_input_and_starts_no_later_phase() {
    let mut link = Scripted::new();
    link.cancel_phase = Some(1);
    link.interrupt = true;
    let ToolOutput::Json(result) = tap(&mut link).unwrap() else {
        panic!()
    };
    assert_eq!(result["phase"], "press");
    assert_eq!(result["advances"][0]["count"], 1);
    assert_eq!(link.calls, vec!["pause", "press", "step", "release"]);
    assert_eq!(link.finished, vec![true]);
}

#[test]
fn cancellation_between_phases_keeps_prior_progress_and_skips_remaining_advance() {
    for (phase, label, counts) in [
        (1, "release_edge", vec![6, 0]),
        (2, "after_release", vec![6, 1, 0]),
    ] {
        let mut link = Scripted::new();
        link.cancel_phase = Some(phase);
        let ToolOutput::Json(result) = tap(&mut link).unwrap() else {
            panic!()
        };
        assert_eq!(result["phase"], label);
        assert_eq!(link.keys.len(), phase);
        assert_eq!(
            result["advances"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["count"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            counts
        );
        assert_eq!(link.calls.iter().filter(|s| *s == "release").count(), 1);
        assert_eq!(link.calls.last().unwrap(), "status");
    }
}

#[test]
fn each_phase_has_a_distinct_native_operation_identity() {
    let mut link = Scripted::new();
    let ToolOutput::Json(result) = tap(&mut link).unwrap() else {
        panic!()
    };
    assert_eq!(result["advances"].as_array().unwrap().len(), 3);
    assert_eq!(link.keys.len(), 3);
    for i in 0..3 {
        for j in i + 1..3 {
            assert_ne!(link.keys[i], link.keys[j]);
        }
    }
    assert_eq!(
        link.calls,
        vec!["pause", "press", "step", "release", "step", "step"]
    );
}

#[test]
fn unacquired_input_is_not_released_and_unsupported_calls_do_not_mutate() {
    let mut link = Scripted::new();
    link.cancel_on_pause = true;
    assert!(matches!(tap(&mut link), Err(LinkError::Cancelled)));
    assert!(!link.calls.iter().any(|s| s == "press" || s == "release"));
    let mut link = Scripted::new();
    link.caps.features = FeatureCapabilities::default();
    assert!(tap(&mut link).is_err());
    assert!(link.calls.is_empty());
    let mut link = Scripted::new();
    link.cancellation.cancel();
    assert!(matches!(tap(&mut link), Err(LinkError::Cancelled)));
    assert!(link.calls.is_empty());
}

#[test]
fn invalid_native_terminal_still_releases_input_and_attempts_freeze() {
    let mut link = Scripted::new();
    link.malformed = true;
    assert!(tap(&mut link).is_err());
    assert_eq!(
        link.calls,
        vec!["pause", "press", "step", "release", "pause"]
    );
}

#[test]
fn generation_change_does_not_release_or_pause_the_replacement() {
    let mut link = Scripted::new();
    link.change_generation = true;
    assert!(tap(&mut link).is_err());
    assert_eq!(link.calls, vec!["pause", "press", "step"]);
    assert_eq!(link.finished, vec![false]);
}

#[test]
fn invalid_standalone_step_terminal_attempts_freeze_before_returning_error() {
    let mut link = Scripted::new();
    link.malformed = true;
    let cancellation = link.cancellation.clone();
    assert!(step_with_cancellation(
        &mut link,
        2,
        StepUnit::Frames,
        None,
        cancellation,
        "owner-a"
    )
    .is_err());
    assert_eq!(link.calls, vec!["step", "pause"]);
}

#[test]
fn core_cleanup_controls_the_durable_generation_guard() {
    use crate::live::continuity::{LinkRecord, ObservedLink, TemporalState};
    use crate::live::runtime::{ManifestSpec, RuntimeStore};
    for (fail_release, fault) in [
        (false, None),
        (true, None),
        (false, Some("begin_temporal_operation")),
        (false, Some("wrong_admission")),
        (false, Some("finish_temporal_operation")),
        (false, Some("missing_port")),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new(temp.path().join("sessions"));
        let prepared = store.prepare(47905).unwrap();
        let manifest = prepared.manifest(ManifestSpec {
            adapter: "openmsx".into(),
            system: "msx2".into(),
            content: "fixture.rom".into(),
            emulator_pid: std::process::id(),
            bridge_pid: None,
            backend_endpoint: None,
            build: Some("test".into()),
        });
        prepared.commit(&manifest).unwrap();
        let mut native = Scripted::new();
        native.port = Some(47905);
        native.caps.identity.launch_id = Some(manifest.launch_id.clone());
        native.fail_release = fail_release;
        native.fault = fault;
        native.cancel_phase = Some(1);
        native.interrupt = true;
        let cancellation = native.cancellation.clone();
        let mut link = ObservedLink::with_store(native, store.clone());
        let result =
            tap_with_cancellation(&mut link, 0, &["a".into()], 6, 120, cancellation, "owner-a");
        let record = store
            .read_link_json::<LinkRecord>(47905, &manifest.launch_id)
            .unwrap()
            .unwrap();
        if fail_release || fault.is_some() {
            assert!(result.is_err());
            assert_eq!(
                record.temporal_operation.unwrap().state,
                TemporalState::Unverified
            );
            assert!(
                matches!(link.call("step",json!({"frames":1})),Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_quarantined")
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
            assert!(record.temporal_operation.is_none());
            link.call("pause", json!({})).unwrap();
        }
    }
}

#[test]
fn durable_admission_write_failure_precedes_any_native_input_or_pause() {
    use crate::live::continuity::{LinkRecord, ObservedLink};
    use crate::live::runtime::{ManifestSpec, RuntimeStore};
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let prepared = store.prepare(47907).unwrap();
    let manifest = prepared.manifest(ManifestSpec {
        adapter: "openmsx".into(),
        system: "msx2".into(),
        content: "fixture.rom".into(),
        emulator_pid: std::process::id(),
        bridge_pid: None,
        backend_endpoint: None,
        build: Some("test".into()),
    });
    prepared.commit(&manifest).unwrap();
    // A directory where a private lock file is required makes real persistence fail.
    std::fs::create_dir(
        store
            .generation_dir(47907, &manifest.launch_id)
            .join(".link.lock"),
    )
    .unwrap();
    let mut native = Scripted::new();
    native.port = Some(47907);
    native.caps.identity.launch_id = Some(manifest.launch_id.clone());
    native.cancel_on_pause = true;
    let cancellation = native.cancellation.clone();
    let mut link = ObservedLink::with_store(native, store.clone());
    assert!(tap_with_cancellation(
        &mut link,
        0,
        &["a".into()],
        6,
        120,
        cancellation.clone(),
        "owner-a"
    )
    .is_err());
    assert!(
        !cancellation.is_cancelled(),
        "the first native pause must not be called"
    );
    assert!(store
        .read_link_json::<LinkRecord>(47907, &manifest.launch_id)
        .unwrap()
        .is_none());
}

#[test]
fn producer_admission_failure_quarantines_before_native_effects() {
    for fault in ["begin_temporal_operation", "wrong_admission"] {
        let mut link = Scripted::new();
        link.fault = Some(fault);
        assert!(tap(&mut link).is_err());
        assert!(link.calls.is_empty());
        assert_eq!(link.finished, vec![false]);
        assert_eq!(link.remote, vec!["begin_temporal_operation"]);
    }
}

#[test]
fn producer_terminal_must_bind_exact_parent_stop_and_input_obligations() {
    for fault in [
        "finish_temporal_operation",
        "wrong_finish",
        "missing_port",
        "extra_port",
        "duplicate_port",
        "running_finish",
        "unverified_finish",
    ] {
        let mut link = Scripted::new();
        link.fault = Some(fault);
        assert!(tap(&mut link).is_err(), "{fault}");
        assert_eq!(link.finished, vec![false], "{fault}");
        assert_eq!(
            link.remote,
            vec!["begin_temporal_operation", "finish_temporal_operation"]
        );
    }
}

#[test]
fn producer_finish_checks_the_selected_port_and_step_only_has_no_input_obligation() {
    let mut link = Scripted::new();
    let cancellation = link.cancellation.clone();
    tap_with_cancellation(&mut link, 2, &["a".into()], 6, 0, cancellation, "owner-a").unwrap();
    assert_eq!(link.ports, [2].into_iter().collect());
    assert_eq!(link.finished, vec![true]);
    let mut link = Scripted::new();
    let cancellation = link.cancellation.clone();
    step_with_cancellation(
        &mut link,
        2,
        StepUnit::Frames,
        None,
        cancellation,
        "owner-a",
    )
    .unwrap();
    assert!(link.ports.is_empty());
    assert_eq!(link.finished, vec![true]);
}

#[test]
fn cancellation_cleanup_shares_time_across_release_and_parent_finish() {
    let mut link = Scripted::new();
    link.cancel_phase = Some(1);
    link.interrupt = true;
    link.slow_release = true;
    tap(&mut link).unwrap();
    let releases = link
        .parent_budgets
        .iter()
        .filter(|(method, _)| method == "set_input")
        .collect::<Vec<_>>();
    let finish = link.parent_budgets.last().unwrap();
    assert_eq!(finish.0, "finish_temporal_operation");
    assert!(
        finish.1 < releases.last().unwrap().1,
        "cleanup budget was reset"
    );
    assert_eq!(link.finished, vec![true]);
}

#[test]
fn exhausted_composed_budget_closes_attachment_without_new_cleanup_writes() {
    let mut link = Scripted::new();
    link.caps
        .features
        .temporal_cancellation
        .as_mut()
        .unwrap()
        .stop_host_ms = 50;
    link.cancel_phase = Some(1);
    link.interrupt = true;
    link.late_terminal = true;
    assert!(tap(&mut link).is_err());
    assert!(link.closed);
    assert_eq!(link.calls, vec!["pause", "press", "step"]);
    assert_eq!(link.remote, vec!["begin_temporal_operation"]);
    assert_eq!(link.finished, vec![false]);
}

#[test]
fn input_cancelled_before_dispatch_acquires_no_release_obligation() {
    let mut link = Scripted::new();
    link.cancel_before_input = true;
    assert!(matches!(tap(&mut link), Err(LinkError::Cancelled)));
    assert!(link.ports.is_empty());
    assert!(!link
        .calls
        .iter()
        .any(|method| method == "press" || method == "release"));
    assert_eq!(link.finished, vec![true]);
}
