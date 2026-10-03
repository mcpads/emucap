use std::sync::{Arc, Mutex};

use emucap::live::link::{Capabilities, EmulatorIdentity, FeatureCapabilities};
use emucap::live::memory_batch::MemoryBatchCapability;
use emucap::live::pacing::ExecutionSpeedCapability;
use serde_json::{json, Value};

use super::*;
use crate::args::{Num, ReadMemoryArgs};

struct ScriptedLink {
    caps: Capabilities,
    reply: Value,
    calls: Vec<(String, Value)>,
}

impl ScriptedLink {
    fn new(features: FeatureCapabilities, reply: Value) -> Self {
        let identity = EmulatorIdentity {
            launch_id: Some("launch-1".into()),
            ..EmulatorIdentity::default()
        };
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["read_memory_batch".into(), "execution_speed".into()],
                memory_types: vec!["ram".into()],
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: emucap::contracts::ContractAdvertisement::Unreported,
                recording: None,
                features,
                identity,
            },
            reply,
            calls: vec![],
        }
    }
}

impl EmulatorLink for ScriptedLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.calls.push((method.into(), params));
        Ok(self.reply.clone())
    }
}

fn features() -> FeatureCapabilities {
    FeatureCapabilities {
        temporal_cancellation: None,
        memory_batch: Some(
            MemoryBatchCapability::from_hello(
                &json!({"max_ranges":2, "max_range_bytes":4, "max_total_bytes":8,
                    "consistency":"frozen_boundary", "halt_kinds":["debugger_break"],
                    "windows":[{"memory_type":"ram","address":0,"length":256}]}),
                &["ram".to_string()],
            )
            .unwrap(),
        ),
        execution_speed: Some(
            ExecutionSpeedCapability::from_hello(&json!({"modes":["limited","unlimited"],
                "percent":{"values":[50,100,200,400]}, "states":["running","frozen"],
                "scope":"host_pacing", "source":"native", "control_service_ms":50}))
            .unwrap(),
        ),
    }
}

fn policy(mode: &str, percent: Value) -> Value {
    json!({"mode":mode, "percent":percent, "source":"native", "policy_revision":"7",
        "host_constraints":[]})
}

fn error_code(result: &CallToolResult) -> Option<String> {
    (result.is_error == Some(true)).then(|| {
        result.structured_content.as_ref().unwrap()["error"]["code"]
            .as_str()
            .unwrap()
            .to_string()
    })
}

fn batch_args(ranges: &[(u64, u64)]) -> ReadMemoryBatchArgs {
    ReadMemoryBatchArgs {
        ranges: ranges
            .iter()
            .map(|(address, length)| ReadMemoryArgs {
                memory_type: "ram".into(),
                address: Num(*address),
                length: Num(*length),
            })
            .collect(),
    }
}

fn speed(mode: Option<ExecutionSpeedMode>, percent: Option<f64>) -> ExecutionSpeedArgs {
    ExecutionSpeedArgs { mode, percent }
}

#[test]
fn batch_admission_failure_never_reaches_the_adapter() {
    for ranges in [&[(0, 4), (4, 4), (8, 1)][..], &[(0, 5)], &[(255, 2)], &[]] {
        let mut link = ScriptedLink::new(features(), json!({}));
        let result = read_memory_batch(&mut link, batch_args(ranges));
        assert_eq!(
            error_code(&result).as_deref(),
            Some("invalid_request"),
            "{ranges:?}"
        );
        assert!(link.calls.is_empty());
    }
    let mut link = ScriptedLink::new(FeatureCapabilities::default(), json!({}));
    let result = read_memory_batch(&mut link, batch_args(&[(0, 1)]));
    assert_eq!(error_code(&result).as_deref(), Some("unsupported"));
    assert!(link.calls.is_empty());
}

#[test]
fn batch_reply_is_published_only_when_it_answers_the_request() {
    let good = json!({"state":"frozen", "consistency":"frozen_boundary",
        "boundary":{"runtime_generation":"launch-1","stop_epoch":"1","memory_mapping_epoch":"1",
            "clocks":[]},
        "total_bytes":3, "reads":[
            {"index":0,"memory_type":"ram","address":16,"length":2,"hex":"0102"},
            {"index":1,"memory_type":"ram","address":0,"length":1,"hex":"ff"}]});
    let mut link = ScriptedLink::new(features(), good.clone());
    let result = read_memory_batch(&mut link, batch_args(&[(16, 2), (0, 1)]));
    assert_eq!(error_code(&result), None);
    assert_eq!(link.calls.len(), 1);
    assert_eq!(
        link.calls[0].1["ranges"][1],
        json!({"memory_type":"ram","address":0,"length":1})
    );

    let mut stale = good;
    stale["boundary"]["runtime_generation"] = json!("launch-0");
    let mut link = ScriptedLink::new(features(), stale);
    let result = read_memory_batch(&mut link, batch_args(&[(16, 2), (0, 1)]));
    assert_eq!(error_code(&result).as_deref(), Some("protocol_error"));
    assert!(!format!("{:?}", result.content).contains("0102"));
}

#[test]
fn pacing_reports_busy_instead_of_queueing_behind_another_operation() {
    let shared: SharedLink = Arc::new(Mutex::new(ScriptedLink::new(features(), json!({}))));
    let guard = shared.lock().unwrap();
    let result = execution_speed(
        &shared,
        speed(Some(ExecutionSpeedMode::Limited), Some(50.0)),
    );
    assert_eq!(error_code(&result).as_deref(), Some("busy"));
    drop(guard);
}

#[test]
fn pacing_requests_are_normalized_and_admitted_before_forwarding() {
    let concrete = Arc::new(Mutex::new(ScriptedLink::new(features(), json!({}))));
    let shared: SharedLink = concrete.clone();
    for args in [
        speed(Some(ExecutionSpeedMode::Limited), Some(300.0)),
        speed(Some(ExecutionSpeedMode::Limited), Some(400.0001)),
        speed(Some(ExecutionSpeedMode::Limited), None),
        speed(Some(ExecutionSpeedMode::Unlimited), Some(100.0)),
        speed(None, Some(50.0)),
        speed(Some(ExecutionSpeedMode::Limited), Some(f64::NAN)),
    ] {
        let result = execution_speed(&shared, args);
        assert_eq!(error_code(&result).as_deref(), Some("invalid_request"));
    }
    assert!(concrete.lock().unwrap().calls.is_empty());
}

#[test]
fn pacing_change_must_be_confirmed_by_native_readback() {
    let change = |confirmed: Value| {
        json!({"status":"completed", "state":"frozen", "previous":policy("limited", json!(100)),
            "execution_speed":confirmed})
    };
    let cases = [
        (change(policy("limited", json!(200))), None),
        (
            change(policy("custom", Value::Null)),
            Some("protocol_error"),
        ),
        (
            change(policy("limited", json!(100))),
            Some("protocol_error"),
        ),
    ];
    for (reply, expected) in cases {
        let concrete = Arc::new(Mutex::new(ScriptedLink::new(features(), reply)));
        let shared: SharedLink = concrete.clone();
        let result = execution_speed(
            &shared,
            speed(Some(ExecutionSpeedMode::Limited), Some(200.0)),
        );
        assert_eq!(error_code(&result).as_deref(), expected);
        let calls = &concrete.lock().unwrap().calls;
        assert_eq!(calls[0].1, json!({"mode":"limited","percent":200}));
    }

    let concrete = Arc::new(Mutex::new(ScriptedLink::new(
        features(),
        policy("unlimited", Value::Null),
    )));
    let shared: SharedLink = concrete.clone();
    assert_eq!(
        error_code(&execution_speed(&shared, speed(None, None))),
        None
    );
    assert_eq!(concrete.lock().unwrap().calls[0].1, json!({}));
}

#[test]
fn routed_speed_rejects_busy_before_status_or_capability_queries() {
    let shared: SharedLink = Arc::new(Mutex::new(ScriptedLink::new(features(), json!({}))));
    let server = crate::Emucap::new(shared.clone());
    let guard = shared.lock().unwrap();
    let result = crate::debug_surface::execute_speed(
        &server,
        crate::args::RoutedOperationArgs {
            operation: "execution_speed".into(),
            arguments: Some(
                serde_json::from_value(json!({"mode":"limited","percent":50})).unwrap(),
            ),
            known_capability_revision: Some("revision".into()),
        },
    );
    assert_eq!(error_code(&result).as_deref(), Some("busy"));
    drop(guard);
}

#[test]
fn routed_speed_rejects_stale_capability_before_native_mutation() {
    for revision in [None, Some("previous-generation-revision")] {
        let concrete = Arc::new(Mutex::new(ScriptedLink::new(
            features(),
            json!({"connected":true,"state":"frozen"}),
        )));
        let shared: SharedLink = concrete.clone();
        let server = crate::Emucap::new(shared);
        let result = crate::debug_surface::execute_speed(
            &server,
            crate::args::RoutedOperationArgs {
                operation: "execution_speed".into(),
                arguments: Some(
                    serde_json::from_value(json!({"mode":"limited","percent":50})).unwrap(),
                ),
                known_capability_revision: revision.map(str::to_owned),
            },
        );
        assert_eq!(error_code(&result).as_deref(), Some("bad_state"));
        let link = concrete.lock().unwrap();
        assert!(
            link.calls.iter().all(|(method, _)| method == "status"),
            "stale capability reached native control: {:?}",
            link.calls
        );
    }
}
