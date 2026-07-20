use std::collections::VecDeque;

use base64::Engine as _;

use super::*;
use crate::live::protocol::Request;

struct FakePine {
    expected: VecDeque<(Vec<u8>, BridgeResult<Vec<u8>>)>,
    terminal: bool,
}

impl FakePine {
    fn new(items: Vec<(Vec<u8>, BridgeResult<Vec<u8>>)>) -> Self {
        Self {
            expected: items.into(),
            terminal: false,
        }
    }
}

impl PineTransport for FakePine {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>> {
        let (expected, response) = self.expected.pop_front().expect("unexpected PINE request");
        assert_eq!(request, expected);
        response
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

impl Drop for FakePine {
    fn drop(&mut self) {
        assert!(self.expected.is_empty(), "unconsumed PINE expectations");
    }
}

fn bridge(expectations: Vec<(Vec<u8>, BridgeResult<Vec<u8>>)>) -> Pcsx2Bridge<FakePine> {
    let mut all = vec![(
        vec![MSG_EMUCAP_VERSION],
        Ok(REQUIRED_HOST_API.to_le_bytes().to_vec()),
    )];
    all.extend(expectations);
    Pcsx2Bridge::with_identity(
        FakePine::new(all),
        None,
        Some("ps2-test".into()),
        Some("token".into()),
    )
    .unwrap()
}

fn pine_string(value: &str) -> Vec<u8> {
    let mut reply = Vec::new();
    reply.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes());
    reply.extend_from_slice(value.as_bytes());
    reply.push(0);
    reply
}

#[test]
fn reports_terminal_backend_state_from_transport() {
    let pine = FakePine {
        expected: vec![(
            vec![MSG_EMUCAP_VERSION],
            Ok(REQUIRED_HOST_API.to_le_bytes().to_vec()),
        )]
        .into(),
        terminal: true,
    };
    let bridge = Pcsx2Bridge::with_identity(pine, None, None, None).unwrap();
    assert!(bridge.backend_terminal());
}

#[cfg(unix)]
#[test]
fn pine_eof_marks_the_real_transport_terminal() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("pine.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut packet = [0u8; 5];
        stream.read_exact(&mut packet).unwrap();
        assert_eq!(u32::from_le_bytes(packet[..4].try_into().unwrap()), 5);
        assert_eq!(packet[4], MSG_STATUS);
    });

    let mut pine = PineSocket::connect(0, Some(&path), Duration::from_secs(1)).unwrap();
    assert!(pine.transact(&[MSG_STATUS]).is_err());
    assert!(pine.is_terminal());
    server.join().unwrap();
}

#[test]
fn rejects_stock_pine_without_the_host_api() {
    let result = Pcsx2Bridge::with_identity(
        FakePine::new(vec![(
            vec![MSG_EMUCAP_VERSION],
            Err(Pcsx2BridgeError::Emulator("rejected".into())),
        )]),
        None,
        None,
        None,
    );
    let error = match result {
        Ok(_) => panic!("stock PINE was accepted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("rejected"));
}

#[test]
fn hello_reports_the_host_api_three_surface() {
    let mut bridge = bridge(vec![]);
    let response = bridge.handle_request(Request::new(1, "hello", json!({})));
    assert!(response.ok);
    let result = response.result.unwrap();
    assert_eq!(result["system"], "ps2");
    assert_eq!(result["adapter"], "pcsx2-rust-pine");
    assert_eq!(result["name"], "ps2-test");
    assert_eq!(result["session_token"], "token");
    assert_eq!(result["contracts"]["catalog"], crate::contracts::CATALOG_ID);
    let methods = METHODS
        .iter()
        .map(|method| (*method).to_string())
        .collect::<Vec<_>>();
    let advertised = crate::contracts::advertisement_from_hello(&result);
    let contract_status = crate::contracts::validate_advertisement(
        &advertised,
        Some("pcsx2-rust-pine"),
        Some("ps2"),
        &methods,
    );
    assert_eq!(contract_status.state, "validated");
    assert_eq!(
        contract_status.constraints["breakpoint.memory.same_kind_min_separation"],
        16
    );
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "step"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "press_buttons"));
    assert_eq!(result["input_buttons"]["buttons"][6], "cross");
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "set_breakpoint"));
}

#[test]
fn read_memory_routes_ee_offset_and_fails_cross_boundary() {
    let mut request = vec![MSG_EMUCAP_READ_BYTES];
    request.extend_from_slice(&0x100u32.to_le_bytes());
    request.extend_from_slice(&3u32.to_le_bytes());
    let mut bridge = bridge(vec![(request, Ok(vec![0xaa, 0xbb, 0xcc]))]);
    let response = bridge.handle_request(Request::new(
        2,
        "read_memory",
        json!({"memory_type":"ee", "address":"0x100", "length":3}),
    ));
    assert_eq!(response.result.unwrap()["hex"], "aabbcc");

    let response = bridge.handle_request(Request::new(
        3,
        "read_memory",
        json!({"memory_type":"ee", "address":"0x1ffffff", "length":2}),
    ));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
}

#[test]
fn write_memory_is_one_terminal_bulk_command() {
    let mut request = vec![MSG_EMUCAP_WRITE_BYTES];
    request.extend_from_slice(&0x200u32.to_le_bytes());
    request.extend_from_slice(&3u32.to_le_bytes());
    request.extend_from_slice(&[1, 2, 3]);
    let mut bridge = bridge(vec![(request, Ok(3u32.to_le_bytes().to_vec()))]);
    let response = bridge.handle_request(Request::new(
        4,
        "write_memory",
        json!({"memory_type":"ee", "address":0x200, "hex":"010203"}),
    ));
    assert_eq!(response.result.unwrap()["written"], 3);
}

#[test]
fn frame_step_accepts_only_bounded_frame_units() {
    let mut request = vec![MSG_EMUCAP_FRAME_ADVANCE];
    request.extend_from_slice(&2u32.to_le_bytes());
    let mut bridge = bridge(vec![
        (vec![MSG_STATUS], Ok(1u32.to_le_bytes().to_vec())),
        (request, Ok(vec![])),
    ]);
    let response =
        bridge.handle_request(Request::new(5, "step", json!({"count":2, "unit":"frames"})));
    assert_eq!(response.result.unwrap()["state"], "frozen");

    let response = bridge.handle_request(Request::new(
        6,
        "step",
        json!({"count":1, "unit":"instructions"}),
    ));
    assert_eq!(response.error.unwrap().kind, "unsupported");
}

#[test]
fn frozen_only_operations_fail_before_mutating_the_emulator() {
    let mut bridge = bridge(vec![
        (vec![MSG_STATUS], Ok(0u32.to_le_bytes().to_vec())),
        (vec![MSG_STATUS], Ok(0u32.to_le_bytes().to_vec())),
    ]);
    let step = bridge.handle_request(Request::new(
        12,
        "step",
        json!({"count":1, "unit":"frames"}),
    ));
    assert_eq!(step.error.unwrap().kind, "bad_state");

    let save = bridge.handle_request(Request::new(
        13,
        "save_state",
        json!({"path":"/tmp/ps2-running.p2s"}),
    ));
    assert_eq!(save.error.unwrap().kind, "bad_state");
}

#[test]
fn parses_ee_register_snapshot() {
    let mut state = Vec::new();
    state.extend_from_slice(&0x0010_0000u32.to_le_bytes());
    for register in 0u64..32 {
        state.extend_from_slice(&register.to_le_bytes());
    }
    state.extend_from_slice(&0x1111u64.to_le_bytes());
    state.extend_from_slice(&0x2222u64.to_le_bytes());
    let mut bridge = bridge(vec![(vec![MSG_EMUCAP_EE_STATE], Ok(state))]);
    let response = bridge.handle_request(Request::new(7, "get_state", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["cpu"], "ee");
    assert_eq!(result["state"]["cpu.pc"], 0x0010_0000u64);
    assert_eq!(result["state"]["cpu.sp"], 29u64);
    assert_eq!(result["state"]["cpu.hi"], 0x1111u64);
}

#[test]
fn parses_disassembly_rows_and_little_endian_bytes() {
    let mut request = vec![MSG_EMUCAP_DISASSEMBLE];
    request.extend_from_slice(&0x1000u32.to_le_bytes());
    request.extend_from_slice(&1u32.to_le_bytes());
    let text = b"addiu sp, sp, -0x20";
    let mut reply = Vec::new();
    reply.extend_from_slice(&1u32.to_le_bytes());
    reply.extend_from_slice(&0x1000u32.to_le_bytes());
    reply.extend_from_slice(&0x27bdffe0u32.to_le_bytes());
    reply.extend_from_slice(&(text.len() as u32).to_le_bytes());
    reply.extend_from_slice(text);
    let mut bridge = bridge(vec![(request, Ok(reply))]);
    let response = bridge.handle_request(Request::new(
        8,
        "disassemble",
        json!({"address":"0x1000", "count":1}),
    ));
    let row = &response.result.unwrap()["instructions"][0];
    assert_eq!(row["bytes"], "e0ffbd27");
    assert_eq!(row["text"], "addiu sp, sp, -0x20");
}

#[test]
fn savestate_paths_must_be_absolute_and_are_length_prefixed() {
    let path = "/tmp/ps2-state.p2s";
    let mut request = vec![MSG_EMUCAP_SAVE_STATE];
    request.extend_from_slice(&(path.len() as u32).to_le_bytes());
    request.extend_from_slice(path.as_bytes());
    let mut bridge = bridge(vec![
        (vec![MSG_STATUS], Ok(1u32.to_le_bytes().to_vec())),
        (request, Ok(vec![])),
    ]);
    let response = bridge.handle_request(Request::new(9, "save_state", json!({"path":path})));
    assert_eq!(response.result.unwrap()["state"], "frozen");

    let response = bridge.handle_request(Request::new(
        10,
        "load_state",
        json!({"path":"relative.p2s"}),
    ));
    assert_eq!(response.error.unwrap().kind, "bad_params");
}

#[test]
fn get_rom_info_never_hashes_large_media_on_the_request_path() {
    let expectations = vec![
        (vec![MSG_TITLE], Ok(pine_string("Test Game"))),
        (vec![MSG_ID], Ok(pine_string("SLPM-00000"))),
        (vec![MSG_UUID], Ok(pine_string("12345678"))),
        (vec![MSG_GAME_VERSION], Ok(pine_string("1.00"))),
    ];
    let mut bridge = bridge(expectations);
    bridge.content = Some(PathBuf::from("/large/game.iso"));
    bridge.content_sha1 = ContentSha1::Pending(std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(200));
        Ok("abc".into())
    }));

    let started = std::time::Instant::now();
    let response = bridge.handle_request(Request::new(11, "get_rom_info", json!({})));
    assert!(response.ok);
    let result = response.result.unwrap();
    assert_eq!(result["hash_status"], "pending");
    assert!(result["sha1"].is_null());
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[test]
fn set_input_maps_all_buttons_and_empty_releases_native_input() {
    let mask = (1u32 << 6) | (1u32 << 9);
    let mut set = vec![MSG_EMUCAP_SET_INPUT];
    set.extend_from_slice(&mask.to_le_bytes());
    let mut release = vec![MSG_EMUCAP_SET_INPUT];
    release.extend_from_slice(&0u32.to_le_bytes());
    let mut bridge = bridge(vec![(set, Ok(vec![])), (release, Ok(vec![]))]);

    let held = bridge.handle_request(Request::new(
        20,
        "set_input",
        json!({"port":0, "buttons":["cross", "start", "cross"]}),
    ));
    assert_eq!(held.result.unwrap()["override_engaged"], true);

    let released = bridge.handle_request(Request::new(21, "set_input", json!({"buttons":[]})));
    let result = released.result.unwrap();
    assert_eq!(result["override_engaged"], false);
    assert_eq!(result["mode"], "native");
}

#[test]
fn press_buttons_is_one_host_owned_window_and_checks_terminal_cleanup() {
    let mask = (1u32 << 0) | (1u32 << 6);
    let mut press = vec![MSG_EMUCAP_PRESS_BUTTONS];
    press.extend_from_slice(&mask.to_le_bytes());
    press.extend_from_slice(&4u32.to_le_bytes());
    let mut terminal = Vec::new();
    terminal.extend_from_slice(&0u32.to_le_bytes());
    terminal.extend_from_slice(&4u32.to_le_bytes());
    terminal.extend_from_slice(&1u32.to_le_bytes());
    let clear_status = vec![0u8; 16];
    let mut bridge = bridge(vec![
        (press, Ok(terminal)),
        (vec![MSG_EMUCAP_INPUT_STATUS], Ok(clear_status)),
    ]);

    let response = bridge.handle_request(Request::new(
        22,
        "press_buttons",
        json!({"buttons":["up", "cross"], "frames":4}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["state"], "frozen");
    assert_eq!(result["frames_elapsed"], 4);
    assert_eq!(result["override_engaged"], false);
}

#[test]
fn input_rejects_bad_port_frame_range_and_unknown_button_before_mutation() {
    let mut bridge = bridge(vec![]);
    for params in [
        json!({"port":1, "buttons":["cross"]}),
        json!({"buttons":["cross"], "frames":241}),
        json!({"buttons":["fire"], "frames":1}),
    ] {
        let response = bridge.handle_request(Request::new(23, "press_buttons", params));
        assert_eq!(response.error.unwrap().kind, "bad_params");
    }
}

#[test]
fn status_reports_emulator_owned_persistent_input() {
    let mut input = Vec::new();
    input.extend_from_slice(&1u32.to_le_bytes());
    input.extend_from_slice(&(1u32 << 6).to_le_bytes());
    input.extend_from_slice(&0u32.to_le_bytes());
    input.extend_from_slice(&0u32.to_le_bytes());
    let mut bridge = bridge(vec![
        (vec![MSG_STATUS], Ok(0u32.to_le_bytes().to_vec())),
        (vec![MSG_VERSION], Ok(pine_string("v2.6.3"))),
        (vec![MSG_EMUCAP_INPUT_STATUS], Ok(input)),
    ]);
    let response = bridge.handle_request(Request::new(24, "status", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["input_override"]["authority"], "emulator");
    assert_eq!(result["input_override"]["mode"], "persistent");
    assert_eq!(result["input_override"]["buttons"][0], "cross");
}

#[test]
fn find_pattern_freezes_one_scan_and_restores_running_state_on_failure() {
    let mut read = vec![MSG_EMUCAP_READ_BYTES];
    read.extend_from_slice(&0x10u32.to_le_bytes());
    read.extend_from_slice(&6u32.to_le_bytes());
    let mut bridge = bridge(vec![
        (vec![MSG_STATUS], Ok(0u32.to_le_bytes().to_vec())),
        (vec![MSG_EMUCAP_PAUSE], Ok(vec![])),
        (
            read,
            Err(Pcsx2BridgeError::Emulator("short host read".into())),
        ),
        (vec![MSG_EMUCAP_RESUME], Ok(vec![])),
    ]);
    let response = bridge.handle_request(Request::new(
        25,
        "find_pattern",
        json!({"memory_type":"ee", "start":"0x10", "length":6, "hex":"aabb"}),
    ));
    assert_eq!(response.error.unwrap().kind, "emulator_error");
}

#[test]
fn find_pattern_reports_region_relative_addresses_and_match_truncation() {
    let mut read = vec![MSG_EMUCAP_READ_BYTES];
    read.extend_from_slice(&0x10u32.to_le_bytes());
    read.extend_from_slice(&6u32.to_le_bytes());
    let mut bridge = bridge(vec![
        (vec![MSG_STATUS], Ok(1u32.to_le_bytes().to_vec())),
        (read, Ok(vec![0xaa, 0xbb, 0, 0xaa, 0xbb, 0])),
    ]);
    let response = bridge.handle_request(Request::new(
        26,
        "find_pattern",
        json!({
            "memory_type":"ee",
            "start":"0x10",
            "length":6,
            "hex":"aabb",
            "max_matches":1
        }),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["matches"], json!([0x10]));
    assert_eq!(result["truncated_matches"], true);
}

struct ScreenshotPine {
    first: bool,
}

impl PineTransport for ScreenshotPine {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>> {
        if self.first {
            self.first = false;
            assert_eq!(request, [MSG_EMUCAP_VERSION]);
            return Ok(REQUIRED_HOST_API.to_le_bytes().to_vec());
        }
        assert_eq!(request.first(), Some(&MSG_EMUCAP_SCREENSHOT));
        let length = u32::from_le_bytes(request[1..5].try_into().unwrap()) as usize;
        let path = std::str::from_utf8(&request[5..5 + length]).unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        std::fs::write(path, png).unwrap();
        let mut reply = Vec::new();
        reply.extend_from_slice(&1u32.to_le_bytes());
        reply.extend_from_slice(&1u32.to_le_bytes());
        reply.extend_from_slice(&99u64.to_le_bytes());
        reply.extend_from_slice(&99u64.to_le_bytes());
        Ok(reply)
    }
}

#[test]
fn screenshot_returns_current_frame_png_with_provenance() {
    let mut bridge = Pcsx2Bridge::with_identity(
        ScreenshotPine { first: true },
        None,
        Some("ps2-test".into()),
        None,
    )
    .unwrap();
    bridge.launch_id = Some("launch-test".into());
    let response = bridge.handle_request(Request::new(27, "screenshot", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["format"], "png");
    assert_eq!(result["width"], 1);
    assert_eq!(result["frame_before"], 99);
    assert_eq!(result["freshness"], "current");
    assert_eq!(result["generation"], "launch-test");
    assert!(result["png_base64"].as_str().unwrap().starts_with("iVBOR"));
}

struct DumpPine {
    initialized: bool,
    reads: usize,
}

impl PineTransport for DumpPine {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>> {
        match request[0] {
            MSG_EMUCAP_VERSION if !self.initialized => {
                self.initialized = true;
                Ok(REQUIRED_HOST_API.to_le_bytes().to_vec())
            }
            MSG_STATUS => Ok(1u32.to_le_bytes().to_vec()),
            MSG_EMUCAP_READ_BYTES => {
                let length = u32::from_le_bytes(request[5..9].try_into().unwrap()) as usize;
                self.reads += 1;
                Ok(vec![self.reads as u8; length])
            }
            opcode => panic!("unexpected opcode {opcode:#x}"),
        }
    }
}

#[test]
fn dump_memory_streams_complete_ee_region_and_writes_manifest_last() {
    let temp = tempfile::tempdir().unwrap();
    let mut bridge = Pcsx2Bridge::with_identity(
        DumpPine {
            initialized: false,
            reads: 0,
        },
        None,
        None,
        None,
    )
    .unwrap();
    let response =
        bridge.handle_request(Request::new(28, "dump_memory", json!({"path":temp.path()})));
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(
        std::fs::metadata(temp.path().join("ee.bin")).unwrap().len(),
        PCSX2_EE_RAM_SIZE
    );
    let regions: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("regions.json")).unwrap()).unwrap();
    assert_eq!(regions[0]["memory_type"], "ee");
    assert_eq!(regions[0]["size"], PCSX2_EE_RAM_SIZE);
    assert!(!temp.path().join(".ee.bin.partial").exists());
}

#[test]
fn breakpoint_crud_uses_exact_native_ranges_and_rejects_lossy_options() {
    let mut set = vec![MSG_EMUCAP_SET_BREAKPOINT];
    set.extend_from_slice(&2u32.to_le_bytes());
    set.extend_from_slice(&0x100u32.to_le_bytes());
    set.extend_from_slice(&0x103u32.to_le_bytes());
    let mut clear = vec![MSG_EMUCAP_CLEAR_BREAKPOINT];
    clear.extend_from_slice(&2u32.to_le_bytes());
    clear.extend_from_slice(&0x100u32.to_le_bytes());
    clear.extend_from_slice(&0x103u32.to_le_bytes());
    let mut bridge = bridge(vec![(set, Ok(vec![])), (clear, Ok(vec![]))]);

    let armed = bridge.handle_request(Request::new(
        30,
        "set_breakpoint",
        json!({
            "kind":"write",
            "memory_type":"ee",
            "start":"0x100",
            "end":"0x103",
            "pause_on_hit":true,
        }),
    ));
    assert!(armed.ok, "{:?}", armed.error);
    assert_eq!(armed.result.unwrap()["id"], 1);
    let listed = bridge.handle_request(Request::new(31, "list_breakpoints", json!({})));
    assert_eq!(listed.result.unwrap()["breakpoints"][0]["end"], 0x103);
    let cleared = bridge.handle_request(Request::new(32, "clear_breakpoint", json!({"id":1})));
    assert_eq!(cleared.result.unwrap()["cleared"], 1);

    for params in [
        json!({"kind":"exec", "memory_type":"ee", "start":"0x1000", "pause_on_hit":false}),
        json!({"kind":"exec", "memory_type":"ee", "start":"0x1000", "snapshot":["ee:0:4"]}),
        json!({"kind":"write", "memory_type":"ee", "start":0, "value":1}),
    ] {
        let rejected = bridge.handle_request(Request::new(33, "set_breakpoint", params));
        assert_eq!(rejected.error.unwrap().kind, "bad_params");
    }
}

#[test]
fn exec_breakpoint_requires_ee_and_rejects_a_canonical_alias_duplicate() {
    let mut set = vec![MSG_EMUCAP_SET_BREAKPOINT];
    set.extend_from_slice(&0u32.to_le_bytes());
    set.extend_from_slice(&0x8000_1000u32.to_le_bytes());
    set.extend_from_slice(&0x8000_1000u32.to_le_bytes());
    let mut clear = vec![MSG_EMUCAP_CLEAR_BREAKPOINT];
    clear.extend_from_slice(&0u32.to_le_bytes());
    clear.extend_from_slice(&0x8000_1000u32.to_le_bytes());
    clear.extend_from_slice(&0x8000_1000u32.to_le_bytes());
    let mut bridge = bridge(vec![(set, Ok(vec![])), (clear, Ok(vec![]))]);

    let missing_type = bridge.handle_request(Request::new(
        34,
        "set_breakpoint",
        json!({"kind":"exec", "start":"0x80001000"}),
    ));
    assert_eq!(missing_type.error.unwrap().kind, "bad_params");

    let armed = bridge.handle_request(Request::new(
        35,
        "set_breakpoint",
        json!({"kind":"exec", "memory_type":"ee", "start":"0x80001000"}),
    ));
    assert!(armed.ok, "{:?}", armed.error);
    assert_eq!(armed.result.unwrap()["memory_type"], "ee");

    let alias = bridge.handle_request(Request::new(
        36,
        "set_breakpoint",
        json!({"kind":"exec", "memory_type":"ee", "start":"0x00001000"}),
    ));
    assert_eq!(alias.error.unwrap().kind, "bad_params");

    let cleared = bridge.handle_request(Request::new(37, "clear_breakpoint", json!({"id":1})));
    assert_eq!(cleared.result.unwrap()["cleared"], 1);
}

#[test]
fn access_breakpoints_reject_ambiguous_same_kind_ranges_before_mutation() {
    fn set_command(kind: u32, start: u32, end: u32) -> Vec<u8> {
        let mut command = vec![MSG_EMUCAP_SET_BREAKPOINT];
        command.extend_from_slice(&kind.to_le_bytes());
        command.extend_from_slice(&start.to_le_bytes());
        command.extend_from_slice(&end.to_le_bytes());
        command
    }

    let mut bridge = bridge(vec![
        (set_command(1, 0x100, 0x1ff), Ok(vec![])),
        (set_command(2, 0x180, 0x18f), Ok(vec![])),
        (set_command(1, 0x20f, 0x21e), Ok(vec![])),
    ]);
    let armed = bridge.handle_request(Request::new(
        38,
        "set_breakpoint",
        json!({"kind":"read", "memory_type":"ee", "start":0x100, "end":0x1ff}),
    ));
    assert_eq!(armed.result.unwrap()["id"], 1);

    for (start, end) in [
        (0x100, 0x1ff),
        (0x120, 0x13f),
        (0x080, 0x220),
        (0x080, 0x100),
        (0x1ff, 0x220),
        (0x200, 0x20e),
    ] {
        let rejected = bridge.handle_request(Request::new(
            39,
            "set_breakpoint",
            json!({"kind":"read", "memory_type":"ee", "start":start, "end":end}),
        ));
        let error = rejected.error.unwrap();
        assert_eq!(error.kind, "bad_params");
        assert!(error.message.contains("id 1"));
        assert!(error.message.contains("same EE memory access"));
    }

    let unchanged = bridge.handle_request(Request::new(40, "list_breakpoints", json!({})));
    let listed = unchanged.result.unwrap();
    assert_eq!(listed["breakpoints"].as_array().unwrap().len(), 1);
    assert_eq!(listed["breakpoints"][0]["id"], 1);

    let other_kind = bridge.handle_request(Request::new(
        41,
        "set_breakpoint",
        json!({"kind":"write", "memory_type":"ee", "start":0x180, "end":0x18f}),
    ));
    assert_eq!(other_kind.result.unwrap()["id"], 2);

    let separated = bridge.handle_request(Request::new(
        42,
        "set_breakpoint",
        json!({"kind":"read", "memory_type":"ee", "start":0x20f, "end":0x21e}),
    ));
    assert_eq!(separated.result.unwrap()["id"], 3);
}

#[test]
fn poll_events_returns_exact_access_and_frozen_register_snapshot() {
    let mut set = vec![MSG_EMUCAP_SET_BREAKPOINT];
    set.extend_from_slice(&1u32.to_le_bytes());
    set.extend_from_slice(&0x200u32.to_le_bytes());
    set.extend_from_slice(&0x20fu32.to_le_bytes());
    let mut events = Vec::new();
    events.extend_from_slice(&1u32.to_le_bytes());
    events.extend_from_slice(&3u32.to_le_bytes());
    events.extend_from_slice(&1u32.to_le_bytes());
    events.extend_from_slice(&0x1000u32.to_le_bytes());
    events.extend_from_slice(&0x208u32.to_le_bytes());
    events.extend_from_slice(&4u32.to_le_bytes());
    for register in 0u64..34 {
        events.extend_from_slice(&register.to_le_bytes());
    }
    let mut bridge = bridge(vec![
        (set, Ok(vec![])),
        (vec![MSG_EMUCAP_POLL_EVENTS], Ok(events)),
    ]);
    bridge.handle_request(Request::new(
        34,
        "set_breakpoint",
        json!({"kind":"read", "memory_type":"ee", "start":0x200, "end":0x20f}),
    ));

    let polled = bridge.handle_request(Request::new(35, "poll_events", json!({})));
    let result = polled.result.unwrap();
    assert_eq!(result["dropped"], 3);
    assert_eq!(result["events"][0]["type"], "breakpoint_hit");
    assert_eq!(result["events"][0]["breakpoint_id"], 1);
    assert_eq!(result["events"][0]["pc"], 0x1000);
    assert_eq!(result["events"][0]["address"], 0x208);
    assert_eq!(result["events"][0]["regs"]["cpu.sp"], 29);
    assert_eq!(result["events"][0]["regs"]["cpu.lo"], 33);
}

#[test]
fn call_stack_is_frozen_only_and_preserves_outer_to_inner_frames() {
    let mut stack = Vec::new();
    stack.extend_from_slice(&1u32.to_le_bytes());
    stack.extend_from_slice(&2u32.to_le_bytes());
    for values in [
        [0x1000u32, 0x0ff0, 0x2000, 0x20],
        [0x1100u32, 0x10f0, 0x1fe0, u32::MAX],
    ] {
        for value in values {
            stack.extend_from_slice(&value.to_le_bytes());
        }
    }
    let mut frozen_bridge = bridge(vec![
        (vec![MSG_STATUS], Ok(1u32.to_le_bytes().to_vec())),
        (vec![MSG_EMUCAP_CALL_STACK], Ok(stack)),
    ]);
    let response = frozen_bridge.handle_request(Request::new(36, "call_stack", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["valid"], true);
    assert_eq!(result["depth"], 2);
    assert_eq!(result["call_stack"][0]["pc"], 0x1000);
    assert_eq!(result["call_stack"][1]["stack_size"], -1);

    let mut running = bridge(vec![(vec![MSG_STATUS], Ok(0u32.to_le_bytes().to_vec()))]);
    let rejected = running.handle_request(Request::new(37, "call_stack", json!({})));
    assert_eq!(rejected.error.unwrap().kind, "bad_state");
}

#[test]
fn reset_is_one_terminal_native_command_and_leaves_ps2_frozen() {
    let mut bridge = bridge(vec![(
        vec![MSG_EMUCAP_RESET],
        Ok(0xbfc0_0000u32.to_le_bytes().to_vec()),
    )]);
    let response = bridge.handle_request(Request::new(38, "reset", json!({})));
    assert_eq!(
        response.result.unwrap(),
        json!({
            "status":"completed",
            "state":"frozen",
            "post_reset_pc":0xbfc0_0000u32,
        })
    );
}
