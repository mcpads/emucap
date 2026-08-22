use crate::args::LaunchPlanArgs;

pub(super) fn unresolved_system_action(
    args: &LaunchPlanArgs,
    inference: &serde_json::Value,
) -> serde_json::Value {
    let question = inference
        .get("required_user_input")
        .cloned()
        .unwrap_or_else(|| serde_json::json!("Provide the missing launch input."));
    match args.content_path.as_deref() {
        Some(content_path) => serde_json::json!({
            "kind": "resolve_input",
            "required_input": ["system"],
            "question_if_missing": question,
            "then_call": {
                "tool": "launch_plan",
                "arguments": {"content_path": content_path},
                "arguments_from": ["system"]
            }
        }),
        None => serde_json::json!({
            "kind": "resolve_input",
            "required_input": ["content_path"],
            "question_if_missing": question,
            "then_call": {
                "tool": "launch_plan",
                "arguments_from": ["content_path", "system?"]
            }
        }),
    }
}

pub(super) fn missing_content_action(system: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "resolve_input",
        "required_input": ["content_path"],
        "question_if_missing": "Which ROM, disc, or disk path should be used?",
        "then_call": {
            "tool": "launch_plan",
            "arguments": {"system": system},
            "arguments_from": ["content_path"]
        }
    })
}

pub(super) fn ready_launch_action() -> serde_json::Value {
    serde_json::json!({
        "kind": "call_tool",
        "tool": "status",
        "arguments": {},
        "review_plan_fields_before_launch": [
            "/preconditions/build_required",
            "/preconditions/bios_required"
        ],
        "require": {
            "path": "/task_entry/accepts_new_content",
            "equals": true
        },
        "then_call": {
            "tool": "launch",
            "arguments_from": "/preferred_launcher/args"
        },
        "after_call": {
            "tool": "status",
            "arguments": {},
            "verify": [
                "/connected",
                "/emulator_identity/system",
                "/contracts/state",
                "/continuity/runtime_binding"
            ]
        }
    })
}

pub(super) fn blocked_launch_action(
    content_path: &str,
    system: &str,
    blockers: &[String],
    approval: Option<&emucap::content_identity::IndirectMediaApproval>,
) -> serde_json::Value {
    let mut arguments = serde_json::json!({
        "content_path": content_path,
        "system": system
    });
    if let Some(approval) = approval {
        arguments["indirect_media_approval"] =
            serde_json::to_value(approval).expect("indirect media approval is serializable");
    }
    serde_json::json!({
        "kind": "resolve_preconditions",
        "blockers": blockers,
        "then_call": {
            "tool": "launch_plan",
            "arguments": arguments
        }
    })
}

pub(super) fn review_indirect_media_action(
    content_path: &str,
    system: &str,
    approval: &emucap::content_identity::IndirectMediaApproval,
    members: &[emucap::content_identity::IndirectMediaMember],
    newly_declared: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "kind": "review_input",
        "review": {
            "scope": "indirect_media_members",
            "members": members,
            "newly_declared": newly_declared,
            "instruction": "Review every relative member name. Continue only when each file belongs to the selected media."
        },
        "then_call": {
            "tool": "launch_plan",
            "arguments": {
                "content_path": content_path,
                "system": system,
                "indirect_media_approval": approval
            }
        }
    })
}

pub(crate) fn apply_task_entry_transition(
    plan: &mut serde_json::Value,
    args: &LaunchPlanArgs,
    bootstrap: &serde_json::Value,
) {
    let state = bootstrap.pointer("/entry/state").and_then(|v| v.as_str());
    if state == Some("ready_for_content") {
        return;
    }
    let Some(obj) = plan.as_object_mut() else {
        return;
    };

    let local_preconditions_ready = obj
        .get("ready_to_launch")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut replan_arguments = serde_json::Map::new();
    if let Some(content_path) = args.content_path.as_deref() {
        replan_arguments.insert(
            "content_path".into(),
            serde_json::Value::String(content_path.into()),
        );
    }
    if let Some(system) = args.system.as_deref() {
        replan_arguments.insert("system".into(), serde_json::Value::String(system.into()));
    }
    if let Some(approval) = args.indirect_media_approval.as_ref() {
        replan_arguments.insert(
            "indirect_media_approval".into(),
            serde_json::to_value(approval).expect("indirect media approval is serializable"),
        );
    }
    let task_entry_action = bootstrap
        .pointer("/entry/primary_action")
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "kind": "call_tool",
                "tool": "status",
                "arguments": {}
            })
        });

    obj.insert(
        "local_preconditions_ready".into(),
        serde_json::json!(local_preconditions_ready),
    );
    obj.insert("ready_to_launch".into(), serde_json::json!(false));
    obj.insert(
        "transition".into(),
        serde_json::json!({
            "state": state,
            "reason": bootstrap.pointer("/entry/reason"),
            "resume_when": {
                "path": "/task_entry/accepts_new_content",
                "equals": true
            },
            "then_call": {
                "tool": "launch_plan",
                "arguments": replan_arguments
            }
        }),
    );
    obj.insert("next_action".into(), task_entry_action);
}
