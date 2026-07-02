// 빌드 시점의 emucap git hash를 바이너리에 임베드한다 — status.server_build로 노출해, 실행 중인 MCP가
// 어느 커밋에서 빌드됐는지 확인 가능하게(재빌드 안 하면 옛 hash 그대로라 stale 감지). launch-time env가
// 아니라 build-time 임베드여야 옳다: 소스 재빌드 없이 재실행하면 바이너리는 옛 것이므로 옛 hash가 맞다.
use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    // dirty: 커밋 안 된 변경이 있으면 -dirty 접미(재빌드 근거 있음을 표시). best-effort.
    let dirty = Command::new("git")
        .args(["diff", "--quiet", "--ignore-submodules", "HEAD"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    let build = if dirty { format!("{hash}-dirty") } else { hash };
    // OUT_DIR에 hash 파일을 쓰고 소스에서 include_str!로 읽는다 — env!(cargo:rustc-env)는 build-script env
    // 변경을 소스 재컴파일에 반영 못 하는 함정이 있어 갱신된 hash가 바이너리에 안 실렸다. include_str!은 cargo가
    // 파일 의존성을 추적하므로 hash 파일이 바뀌면 그 소스가 확실히 재컴파일된다.
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    std::fs::write(format!("{out_dir}/emucap_build_hash"), &build).expect("write build hash");
    // 커밋·체크아웃·reset마다 갱신해야 hash가 최신이 된다. `.git/HEAD`는 symref라 커밋 시 내용이 안 바뀌어
    // 트리거가 안 된다 — `.git/logs/HEAD`(HEAD 이동마다 append)를 쓴다. `.git/index`는 스테이징(=-dirty 근거)
    // 변화를 잡는다. (working-tree만 바뀌면 cargo가 패키지 파일 변화로 build.rs를 재실행하니 -dirty가 갱신된다.)
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
