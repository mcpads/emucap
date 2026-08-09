use std::collections::VecDeque;

use super::{power_cycle, reset, watch_register, LinkError, ToolOutput};
use crate::live::link::{Capabilities, EmulatorLink, FakeLink};
use serde_json::json;

struct ResetReconnectLink {
    caps: Capabilities,
    status_results: VecDeque<Result<serde_json::Value, LinkError>>,
    prepared: bool,
    calls: Vec<String>,
    operation: &'static str,
    replacement_launch_id: Option<&'static str>,
}

impl EmulatorLink for ResetReconnectLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.calls.push(method.to_string());
        match method {
            method if method == self.operation => Ok(json!({
                self.operation: true,
                "reconnect": true
            })),
            "status" => {
                assert!(self.prepared, "old transport must be discarded first");
                let result = self
                    .status_results
                    .pop_front()
                    .unwrap_or(Err(LinkError::NotConnected));
                if result.as_ref().ok().is_some_and(|status| {
                    status.get("connected").and_then(serde_json::Value::as_bool) == Some(true)
                }) {
                    if let Some(launch_id) = self.replacement_launch_id {
                        self.caps.identity.launch_id = Some(launch_id.into());
                    }
                }
                result
            }
            other => panic!("unexpected method: {other}"),
        }
    }

    fn prepare_reconnect(&mut self) {
        self.prepared = true;
    }
}

#[test]
fn reset_waits_for_replacement_session() {
    let mut link = ResetReconnectLink {
        caps: Capabilities::empty(),
        status_results: VecDeque::from([
            Err(LinkError::NotConnected),
            Ok(json!({"connected": true, "state": "running"})),
        ]),
        prepared: false,
        calls: Vec::new(),
        operation: "reset",
        replacement_launch_id: None,
    };

    let result = reset(&mut link).unwrap();
    assert_eq!(
        result,
        ToolOutput::Json(json!({
            "reset": true,
            "reconnected": true,
            "state": "running"
        }))
    );
    assert_eq!(link.calls, ["reset", "status", "status"]);
}

#[test]
fn power_cycle_waits_for_replacement_session() {
    let mut link = ResetReconnectLink {
        caps: Capabilities::empty(),
        status_results: VecDeque::from([
            Err(LinkError::NotConnected),
            Ok(json!({"connected": true, "state": "running"})),
        ]),
        prepared: false,
        calls: Vec::new(),
        operation: "power_cycle",
        replacement_launch_id: None,
    };

    let result = power_cycle(&mut link).unwrap();
    assert_eq!(
        result,
        ToolOutput::Json(json!({
            "power_cycle": true,
            "reconnected": true,
            "state": "running"
        }))
    );
    assert_eq!(link.calls, ["power_cycle", "status", "status"]);
}

#[test]
fn reconnecting_operation_rejects_a_replacement_launch_generation() {
    let mut caps = Capabilities::empty();
    caps.identity.launch_id = Some("launch-original".into());
    let mut link = ResetReconnectLink {
        caps,
        status_results: VecDeque::from([Ok(json!({
            "connected": true,
            "state": "running"
        }))]),
        prepared: false,
        calls: Vec::new(),
        operation: "power_cycle",
        replacement_launch_id: Some("launch-replacement"),
    };

    let error = power_cycle(&mut link).unwrap_err();
    assert!(matches!(error, LinkError::Emulator { ref kind, .. } if kind == "identity_changed"));
}

#[test]
fn reset_without_reconnect_marker_stays_single_call() {
    let mut link = FakeLink::ok(json!({"reset": true}));
    assert_eq!(
        reset(&mut link).unwrap(),
        ToolOutput::Json(json!({"reset": true}))
    );
    assert_eq!(link.last_method.as_deref(), Some("reset"));
}

#[test]
fn watch_register_rejects_over_budget() {
    // 과대 max_instructions는 매 명령 getState 플러드를 오래 돌려 emu 스레드를 굶긴다 — 실행 전 거부.
    let mut link = FakeLink::ok(json!({ "id": 1 }));
    let r = watch_register(&mut link, "sp", 0, 0xffff, true, Some(u64::MAX));
    assert!(
        matches!(r, Err(LinkError::Emulator { ref kind, .. }) if kind == "bad_params"),
        "과대 max_instructions는 bad_params로 거부해야: {r:?}"
    );
    let mut link2 = FakeLink::ok(json!({ "id": 1 }));
    assert!(
        watch_register(&mut link2, "sp", 0, 0xffff, true, Some(1000)).is_ok(),
        "상한 이내 예산은 통과해야"
    );
}

fn dir_with(path: &std::path::Path, marker: &[u8]) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("marker"), marker).unwrap();
}

#[test]
fn replace_dir_into_absent_dst_is_single_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dump");
    let staging = tmp.path().join(".dump.staging");
    dir_with(&staging, b"new");
    super::replace_dir(&staging, &dst).unwrap();
    assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"new");
    assert!(!staging.exists(), "이동 후 staging은 사라져야");
}

#[test]
fn replace_dir_publishes_new_dump_over_existing() {
    // end-to-end(교환 또는 폴백): 기존 덤프 위에 새 스테이징 배치 → dst엔 신본, staging 제거.
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dump");
    dir_with(&dst, b"old");
    let staging = tmp.path().join(".dump.staging");
    dir_with(&staging, b"new");
    super::replace_dir(&staging, &dst).unwrap();
    assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"new");
    assert!(!staging.exists(), "스왑/이동 후 staging은 제거되어야");
    // 백업 잔재(.dump.dump-old.*)가 남지 않아야.
    let leftovers = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains("dump-old"));
    assert!(!leftovers, "성공 시 백업/구덤프 잔재가 없어야");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unsupported_exchange_errnos_fall_back_not_hard_fail() {
    // 교환 프리미티브를 파일시스템/커널이 거부하는 errno 계열은 2-rename 폴백으로 강등해야 한다.
    // macOS 아암이 ENOTSUP만 폴백하던 회귀: EINVAL을 내는 파일시스템이면 덤프 publish가 하드-실패했다.
    use super::is_unsupported_exchange_errno as f;
    for e in [libc::ENOSYS, libc::EINVAL, libc::ENOTSUP] {
        assert!(f(Some(e)), "미지원 errno {e}는 폴백해야");
    }
    assert!(!f(Some(libc::ENOENT)), "경로 소멸(ENOENT)은 진짜 실패");
    assert!(!f(None), "errno 없음은 진짜 실패");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn try_exchange_swaps_contents_atomically() {
    // 이 플랫폼의 원자 교환 프리미티브가 두 디렉토리 내용을 한 번에 맞바꾼다(교환 경로 검증).
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    dir_with(&a, b"A");
    dir_with(&b, b"B");
    assert!(
        super::try_exchange(&a, &b).unwrap(),
        "지원 플랫폼(linux/macos)은 교환에 성공해야"
    );
    assert_eq!(std::fs::read(a.join("marker")).unwrap(), b"B");
    assert_eq!(std::fs::read(b.join("marker")).unwrap(), b"A");
}

#[test]
fn replace_dir_refuses_file_destination() {
    // 요청 경로에 사용자의 일반 파일이 있으면 원자 스왑/폴백이 그 파일을 숨은 이름으로 밀어내
    // (요청 경로에서 사라지게) 하면 안 된다 — 거부하고 파일을 바이트 그대로 둔다.
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dump");
    std::fs::write(&dst, b"user-file").unwrap();
    let staging = tmp.path().join(".dump.staging");
    dir_with(&staging, b"new");
    let err = super::replace_dir(&staging, &dst).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&dst).unwrap(),
        b"user-file",
        "거부 시 사용자 파일은 그대로여야"
    );
    assert!(staging.exists(), "거부 시 staging은 밀려나지 않아야");
}

#[cfg(unix)]
#[test]
fn replace_dir_refuses_symlink_destination() {
    // dst가 심링크면(파일이든 디렉토리든) 거부한다 — 스왑/폴백이 링크를 교체하거나 대상을 밀어내지
    // 않게. copy_dir_replace와 같은 가드.
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    dir_with(&real, b"real");
    let dst = tmp.path().join("dump");
    std::os::unix::fs::symlink(&real, &dst).unwrap();
    let staging = tmp.path().join(".dump.staging");
    dir_with(&staging, b"new");
    let err = super::replace_dir(&staging, &dst).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        std::fs::symlink_metadata(&dst)
            .unwrap()
            .file_type()
            .is_symlink(),
        "심링크는 보존되어야"
    );
    assert_eq!(
        std::fs::read(real.join("marker")).unwrap(),
        b"real",
        "심링크 대상 디렉토리는 보존되어야"
    );
}

#[test]
fn dump_memory_refuses_file_destination() {
    // 요청 경로가 일반 파일이면 브리지 덤프·스테이징 전에 거부하고 파일을 보존한다(fail-fast) —
    // 어댑터를 호출하지 않는다.
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dump");
    std::fs::write(&dst, b"user-file").unwrap();
    let mut link = FakeLink::ok(json!({}));
    let err = super::dump_memory(&mut link, dst.to_str().unwrap()).unwrap_err();
    assert!(
        matches!(err, LinkError::Protocol(_)),
        "가드는 Protocol 에러"
    );
    assert_eq!(
        std::fs::read(&dst).unwrap(),
        b"user-file",
        "거부 시 사용자 파일은 그대로여야"
    );
    assert!(
        link.last_method.is_none(),
        "가드가 브리지 dump_memory 호출 전에 거부해야"
    );
}

#[test]
fn replace_dir_fallback_swaps_over_existing() {
    // 폴백(2-rename) 경로를 직접 검증 — 교환 미지원 플랫폼/파일시스템의 동작.
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dump");
    dir_with(&dst, b"old");
    let staging = tmp.path().join(".dump.staging");
    dir_with(&staging, b"new");
    super::replace_dir_fallback(&staging, &dst).unwrap();
    assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"new");
    assert!(!staging.exists(), "폴백 성공 후 staging 제거");
    let leftovers = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains("dump-old"));
    assert!(!leftovers, "폴백 성공 시 백업이 제거되어야");
}
