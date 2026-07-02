//! 에뮬레이터 연결 broker 데몬.
//!
//! 두 포트를 원자적으로 바인드(둘 다 못 잡으면 즉시 종료 — bind-as-lock 선출)하고
//! serve를 구동한다. SO_REUSEPORT는 사용하지 않는다(이중 바인드를 허용하면
//! bind-as-lock 선출이 깨짐).
use std::net::TcpListener;

use emucap::live::broker;

fn main() {
    let emu_port: u16 = std::env::var("EMUCAP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(47800);
    let sess_port: u16 = std::env::var("EMUCAP_BROKER_SESSION_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(emu_port + 100);

    // 원자적: 에뮬레이터 포트 먼저 바인드.
    let emu = match TcpListener::bind(("127.0.0.1", emu_port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("broker: 에뮬레이터 포트 {emu_port} 바인드 실패({e}) — 이미 broker가 실행 중일 수 있음. 종료");
            std::process::exit(0);
        }
    };

    // 세션 포트 바인드. 실패하면 에뮬레이터 포트를 drop(해제)하고 종료.
    let sess = match TcpListener::bind(("127.0.0.1", sess_port)) {
        Ok(l) => l,
        Err(e) => {
            // emu는 여기서 drop → OS가 포트를 즉시 해제
            drop(emu);
            eprintln!("broker: 세션 포트 {sess_port} 바인드 실패({e}) — 종료");
            std::process::exit(0);
        }
    };

    // 세션 stale 임계(이 시간만큼 활동 없으면 hang으로 보고 신규 attach가 steal). 기본 15초.
    let stale_ms: u64 = std::env::var("EMUCAP_BROKER_STALE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15000);

    eprintln!("broker: 구동 emu={emu_port} sess={sess_port} stale={stale_ms}ms");
    broker::serve(emu, sess, std::time::Duration::from_millis(stale_ms));
}
