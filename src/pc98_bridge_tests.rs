use super::*;
use std::collections::VecDeque;
use std::io::Write;

#[derive(Default)]
struct FakeGdb {
    replies: VecDeque<(String, String)>,
    calls: Vec<String>,
    no_reply: Vec<String>,
    nonblocking: VecDeque<String>,
    /// (trigger payload, stop) pairs: when `send` serves the trigger, the stop is enqueued to
    /// `nonblocking`. Models an async stop that arrives *after* a command (e.g. a frame-target
    /// stop that coincides with a BP hit), which a pre-command drain must not see early.
    nonblocking_after: Vec<(String, String)>,
    timeout: Duration,
    timeouts: Vec<Duration>,
    interrupts: usize,
}

impl FakeGdb {
    fn with(replies: &[(&str, &str)]) -> Self {
        Self {
            replies: replies
                .iter()
                .map(|(a, b)| ((*a).into(), (*b).into()))
                .collect(),
            ..Default::default()
        }
    }

    fn from_pairs(replies: Vec<(String, String)>) -> Self {
        Self {
            replies: replies.into(),
            ..Default::default()
        }
    }

    fn with_nonblocking(mut self, replies: &[&str]) -> Self {
        self.nonblocking = replies.iter().map(|reply| (*reply).into()).collect();
        self
    }

    fn enqueue_nonblocking_after(mut self, trigger: &str, stop: &str) -> Self {
        self.nonblocking_after.push((trigger.into(), stop.into()));
        self
    }
}

impl GdbTransport for FakeGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        self.calls.push(payload.into());
        let Some((expected, reply)) = self.replies.pop_front() else {
            return Err(GdbError::Emulator(format!(
                "unexpected fake GDB call: {payload}"
            )));
        };
        assert_eq!(payload, expected);
        if let Some(pos) = self
            .nonblocking_after
            .iter()
            .position(|(trigger, _)| trigger == payload)
        {
            let (_, stop) = self.nonblocking_after.remove(pos);
            self.nonblocking.push_back(stop);
        }
        Ok(reply)
    }

    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()> {
        self.no_reply.push(payload.into());
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        self.interrupts += 1;
        Ok("S05".into())
    }

    fn get_timeout(&self) -> GdbResult<Duration> {
        if self.timeout.is_zero() {
            Ok(Duration::from_secs(5))
        } else {
            Ok(self.timeout)
        }
    }

    fn set_timeout(&mut self, timeout: Duration) -> GdbResult<()> {
        self.timeout = timeout;
        self.timeouts.push(timeout);
        Ok(())
    }

    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        Ok(self.nonblocking.pop_front())
    }
}

fn i386_regs_hex(values: &[(&str, u32)]) -> String {
    let mut out = Vec::new();
    for name in I386_REGS {
        let value = values
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        out.extend_from_slice(&value.to_le_bytes());
    }
    hex::encode(out)
}

struct StateSaveGdb {
    regs_hex: String,
    save_items_dir: Option<PathBuf>,
    reads: usize,
    fail_at_read: Option<usize>,
}

impl StateSaveGdb {
    fn new(regs_hex: String) -> Self {
        Self {
            regs_hex,
            save_items_dir: None,
            reads: 0,
            fail_at_read: None,
        }
    }

    /// Fake a region read that times out on the Nth `m` read, so save_state fails mid-zip.
    fn failing_at_read(regs_hex: String, fail_at_read: usize) -> Self {
        Self {
            regs_hex,
            save_items_dir: None,
            reads: 0,
            fail_at_read: Some(fail_at_read),
        }
    }
}

impl GdbTransport for StateSaveGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        if payload == "qEmucap,stop" {
            return Ok("OK".into());
        }
        if payload == "g" {
            return Ok(self.regs_hex.clone());
        }
        if let Some(hex_path) = payload.strip_prefix("qEmucap,saveitems,") {
            let bytes = hex::decode(hex_path)
                .map_err(|_| GdbError::Emulator("bad saveitems path hex".into()))?;
            let path = PathBuf::from(
                String::from_utf8(bytes)
                    .map_err(|_| GdbError::Emulator("bad saveitems path utf8".into()))?,
            );
            std::fs::create_dir_all(&path)?;
            std::fs::write(path.join("manifest.txt"), "item\n")?;
            self.save_items_dir = Some(path);
            return Ok("OK|1|0".into());
        }
        if let Some(rest) = payload.strip_prefix('m') {
            let Some((_addr, len_hex)) = rest.split_once(',') else {
                return Err(GdbError::Emulator(format!("bad read: {payload}")));
            };
            let len = usize::from_str_radix(len_hex, 16)
                .map_err(|_| GdbError::Emulator(format!("bad read len: {payload}")))?;
            self.reads += 1;
            if self.fail_at_read == Some(self.reads) {
                return Err(GdbError::Emulator("simulated region read timeout".into()));
            }
            return Ok("00".repeat(len));
        }
        Err(GdbError::Emulator(format!("unexpected call: {payload}")))
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

#[derive(Default)]
struct StateLoadGdb {
    regs_hex: String,
    writes: Vec<String>,
    regprobe_specs: Vec<String>,
    load_items_dirs: Vec<PathBuf>,
}

impl StateLoadGdb {
    fn new(regs_hex: String) -> Self {
        Self {
            regs_hex,
            ..Default::default()
        }
    }
}

impl GdbTransport for StateLoadGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        if payload == "qEmucap,stop" {
            return Ok("OK".into());
        }
        if let Some(hex_path) = payload.strip_prefix("qEmucap,loaditems,") {
            let bytes = hex::decode(hex_path)
                .map_err(|_| GdbError::Emulator("bad loaditems path hex".into()))?;
            self.load_items_dirs
                .push(PathBuf::from(String::from_utf8(bytes).map_err(|_| {
                    GdbError::Emulator("bad loaditems path utf8".into())
                })?));
            return Ok("OK|1|0".into());
        }
        if payload.starts_with('M') {
            self.writes.push(payload.into());
            return Ok("OK".into());
        }
        if let Some(hex_regs) = payload.strip_prefix("qEmucap,regload,") {
            let bytes =
                hex::decode(hex_regs).map_err(|_| GdbError::Emulator("bad regload hex".into()))?;
            let regs = String::from_utf8(bytes)
                .map_err(|_| GdbError::Emulator("bad regload utf8".into()))?;
            assert_eq!(regs, self.regs_hex);
            return Ok(format!("OK|{}", self.regs_hex));
        }
        if let Some(hex_spec) = payload.strip_prefix("qEmucap,regprobe,") {
            let bytes =
                hex::decode(hex_spec).map_err(|_| GdbError::Emulator("bad regprobe hex".into()))?;
            let spec = String::from_utf8(bytes)
                .map_err(|_| GdbError::Emulator("bad regprobe utf8".into()))?;
            self.regprobe_specs.push(spec);
            return Ok(format!("HEX:cafe|FRAME:3|REGS:{}", self.regs_hex));
        }
        Err(GdbError::Emulator(format!("unexpected call: {payload}")))
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

fn write_test_state(path: &Path, regs_hex: &str) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("tvram.bin", options).unwrap();
    zip.write_all(&[0xAA, 0xBB]).unwrap();
    zip.start_file(SAVE_ITEMS_MANIFEST, options).unwrap();
    zip.write_all(b"item\n").unwrap();
    zip.start_file("state.json", options).unwrap();
    let manifest = json!({
        "format": STATE_FORMAT,
        "system": "pc98",
        "adapter": "mame-pc98-gdb",
        "registers_hex": regs_hex,
        "regions": [{
            "name": "tvram",
            "memory_type": "tvram",
            "base_address": 0xA0000,
            "size": 2,
            "file": "tvram.bin",
        }],
        "save_items": {"items": 1, "skipped": 0, "dir": SAVE_ITEMS_DIR},
        "state_restore": state_restore_info(),
    });
    zip.write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    zip.finish().unwrap();
}

#[test]
fn hello_advertises_only_implemented_rust_methods() {
    let env = GdbBridgeEnv {
        name: Some("pc98".into()),
        session_token: Some("token".into()),
        build: Some("abc123".into()),
        ..Default::default()
    };
    let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), env);
    let response = bridge.handle_request(Request::new(1, "hello", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["adapter"], "mame-pc98-rust-gdb");
    assert_eq!(result["name"], "pc98");
    assert_eq!(result["session_token"], "token");
    assert_eq!(result["build"], "abc123");
    assert_eq!(result["contracts"]["catalog"], crate::contracts::CATALOG_ID);
    assert_eq!(
        result["contracts"]["active_exceptions"],
        json!([
            "pc98.call-stack.best-effort",
            "pc98.input-hold.port-zero-only",
            "pc98.input-pulse.constraints"
        ])
    );
    assert!(result["contracts"].get("authority").is_none());
    let advertised_methods: Vec<String> = result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect();
    let contract_status = crate::contracts::validate_advertisement(
        &crate::contracts::advertisement_from_hello(&result),
        result["adapter"].as_str(),
        result["system"].as_str(),
        &advertised_methods,
    );
    assert_eq!(
        contract_status.state, "validated",
        "{:?}",
        contract_status.errors
    );
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "read_memory"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "find_pattern"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "screenshot"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "run_frames"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "step_instructions"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "disassemble"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "set_breakpoint"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "poll_events"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "watch_register"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "set_trace"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "get_trace"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "call_stack"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "load_state"));
    assert!(result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "probe"));
    assert_eq!(
        result["memory_types"],
        json!(["cpu", "gvram_b", "gvram_g", "gvram_i", "gvram_r", "physical", "ram", "tvram"])
    );
}

#[test]
fn read_and_write_memory_map_regions_to_absolute_gdb_addresses() {
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        ("ma0010,4", "01020304"),
        ("Ma0010,2:aabb", "OK"),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());

    let read = bridge.handle_request(Request::new(
        2,
        "read_memory",
        json!({"memory_type":"tvram","address":"0x10","length":4}),
    ));
    assert_eq!(read.result.unwrap()["hex"], "01020304");

    let write = bridge.handle_request(Request::new(
        3,
        "write_memory",
        json!({"memory_type":"tvram","address":"$10","hex":"aabb"}),
    ));
    assert_eq!(write.result.unwrap()["written"], 2);
}

#[test]
fn read_memory_rejects_access_straddling_region_end() {
    let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(
        4,
        "read_memory",
        json!({"memory_type":"tvram","address":"0x3fff","length":2}),
    ));
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert_eq!(error.kind, "bad_params");
    assert!(error.message.contains("tvram access out of range"));
    assert_eq!(bridge.gdb.calls, vec!["?"], "reject before GDB read");
}

#[test]
fn write_memory_rejects_access_straddling_region_end() {
    let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(
        5,
        "write_memory",
        json!({"memory_type":"tvram","address":"0x3fff","hex":"aabb"}),
    ));
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert_eq!(error.kind, "bad_params");
    assert!(error.message.contains("tvram access out of range"));
    assert_eq!(bridge.gdb.calls, vec!["?"], "reject before GDB write");
}

#[test]
fn memory_access_ending_exactly_at_region_end_is_allowed() {
    let fake = FakeGdb::with(&[("?", "S05"), ("ma3fff,1", "7f"), ("Ma3fff,1:80", "OK")]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());

    let read = bridge.handle_request(Request::new(
        6,
        "read_memory",
        json!({"memory_type":"tvram","address":"0x3fff","length":1}),
    ));
    assert_eq!(read.result.unwrap()["hex"], "7f");

    let write = bridge.handle_request(Request::new(
        7,
        "write_memory",
        json!({"memory_type":"tvram","address":"0x3fff","hex":"80"}),
    ));
    assert_eq!(write.result.unwrap()["written"], 1);
}

#[test]
fn find_pattern_scans_region_with_match_limit() {
    let mut bridge = Bridge::new(
        FakeGdb::with(&[("?", "S05"), ("m0,8", "aa00aa00aa00aa00")]),
        GdbBridgeEnv::default(),
    );
    let response = bridge.handle_request(Request::new(
        7,
        "find_pattern",
        json!({"memory_type":"ram","start":0,"length":8,"hex":"aa00","max_matches":2}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["matches"], json!([0, 2]));
    assert_eq!(result["count"], 2);
    assert_eq!(result["truncated_matches"], true);
    assert_eq!(result["truncated"], true);
}

#[test]
fn input_methods_send_lua_commands_with_normalized_buttons() {
    let mut bridge = Bridge::new(
        FakeGdb::with(&[
            ("?", "S05"),
            ("qEmucap,setinput,656e7465722c657363", "OK"),
            ("qEmucap,press,333a612c62", "OK"),
            ("qEmucap,frame", "42"),
            ("qEmucap,reset", "OK"),
            ("qEmucap,breakonreset,31", "OK"),
        ]),
        GdbBridgeEnv::default(),
    );

    let set = bridge.handle_request(Request::new(
        8,
        "set_input",
        json!({"buttons":["start","escape"]}),
    ));
    assert_eq!(set.result.unwrap()["buttons"], json!(["enter", "esc"]));

    let press = bridge.handle_request(Request::new(
        9,
        "press_buttons",
        json!({"buttons":["a","b"],"frames":3}),
    ));
    assert_eq!(
        press.result.unwrap(),
        json!({
            "status":"completed",
            "buttons":["a","b"],
            "frames":3,
            "frame":42,
            "state":"running"
        })
    );

    let reset = bridge.handle_request(Request::new(10, "reset", json!({})));
    assert_eq!(reset.result.unwrap()["reset"], "scheduled");

    let br = bridge.handle_request(Request::new(11, "break_on_reset", json!({"enabled":true})));
    assert_eq!(br.result.unwrap()["mode"], "machine_reset_notifier");
}

#[test]
fn breakpoint_methods_set_list_clear_and_enrich_events() {
    let condition = "(pc >= 2000) && ((wpdata & FF) == 42)";
    let set_spec = format!("3|a0010|5|1|{condition}");
    let clear_spec = "wp|7";
    let regs = i386_regs_hex(&[("eip", 0x1234), ("cs", 0)]);
    let fake = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("qEmucap,setpoint,{}", hex::encode(set_spec.as_bytes())),
            "WP:7".into(),
        ),
        (
            format!("qEmucap,clearpoint,{}", hex::encode(clear_spec.as_bytes())),
            "OK".into(),
        ),
        (
            format!(
                "qEmucap,setpoint,{}",
                hex::encode("0|a0000|1|1|".as_bytes())
            ),
            "BP:2".into(),
        ),
        ("qEmucap,pollreset".into(), "NONE".into()),
        ("g".into(), regs),
        ("ma0000,2".into(), "aabb".into()),
    ])
    .with_nonblocking(&["T05hwbreak:00000a00;idx:2;"]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());

    let set = bridge.handle_request(Request::new(
        12,
        "set_breakpoint",
        json!({
            "kind": "read",
            "memory_type": "tvram",
            "start": "0x10",
            "end": "0x14",
            "pc_min": "0x2000",
            "value": "0x42"
        }),
    ));
    assert_eq!(set.result.unwrap()["id"], 1);

    let list = bridge.handle_request(Request::new(13, "list_breakpoints", json!({})));
    assert_eq!(
        list.result.unwrap()["breakpoints"],
        json!([{
            "id": 1,
            "kind": "read",
            "start": 0xA0010,
            "end": 0xA0014,
            "condition": condition,
        }])
    );

    let cleared = bridge.handle_request(Request::new(14, "clear_breakpoint", json!({"id": 1})));
    assert_eq!(cleared.result.unwrap()["cleared"], 1);

    let set_exec = bridge.handle_request(Request::new(
        15,
        "set_breakpoint",
        json!({"kind": "exec", "memory_type": "tvram", "start": 0, "snapshot": ["tvram:0:2"]}),
    ));
    assert_eq!(set_exec.result.unwrap()["id"], 2);
    bridge.frozen = false;

    let events = bridge.handle_request(Request::new(16, "poll_events", json!({})));
    let events = events.result.unwrap();
    assert_eq!(events["dropped"], 0);
    assert_eq!(
        events["events"][0],
        json!({
            "type": "breakpoint_hit",
            "signal": "05",
            "raw": "T05hwbreak:00000a00;idx:2;",
            "kind": "exec",
            "address": 0xA0000,
            "backend_id": 2,
            "id": 2,
            "breakpoint_id": 2,
            "regs": {
                "cpu.eax": 0, "cpu.ecx": 0, "cpu.edx": 0, "cpu.ebx": 0,
                "cpu.esp": 0, "cpu.ebp": 0, "cpu.esi": 0, "cpu.edi": 0,
                "cpu.eip": 0x1234, "cpu.eflags": 0, "cpu.cs": 0, "cpu.ss": 0,
                "cpu.ds": 0, "cpu.es": 0, "cpu.fs": 0, "cpu.gs": 0,
                "cpu.offset_pc": 0x1234, "cpu.pc": 0x1234,
            },
            "snapshot": [{"memory_type": "tvram", "address": 0, "hex": "aabb"}],
        })
    );
}

#[test]
fn set_breakpoint_rejects_out_of_range_region_offset() {
    // tvram is 0x4000; without the bound, region.base + start lands past the region and MAME's
    // setpoint may silently accept an address that can never fire. Reject before arming.
    let fake = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let r = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "exec", "memory_type": "tvram", "start": "0x5000"}),
    ));
    assert!(!r.ok);
    let err = r.error.unwrap();
    assert_eq!(err.kind, "bad_params");
    assert!(
        err.message.contains("out of range") && err.message.contains("tvram"),
        "names the region and the out-of-range condition: {}",
        err.message
    );
    // Nothing was armed on the emulator.
    assert!(!bridge.gdb.calls.iter().any(|c| c.contains("setpoint")));
}

#[test]
fn set_breakpoint_in_range_resolves_to_region_base_plus_offset() {
    // tvram base 0xA0000 + start 0x100 = 0xA0100; the setpoint spec carries the absolute address.
    let set_spec = format!("0|{:x}|1|1|", 0xA0000u64 + 0x100);
    let fake = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("qEmucap,setpoint,{}", hex::encode(set_spec.as_bytes())),
            "BP:9".into(),
        ),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let r = bridge.handle_request(Request::new(
        1,
        "set_breakpoint",
        json!({"kind": "exec", "memory_type": "tvram", "start": "0x100"}),
    ));
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(r.result.unwrap()["id"], 1);
}

#[test]
fn watch_register_sets_regpoint_and_reports_value() {
    let spec = "1|(esp < 1000) || (esp > 2000)";
    let regs = i386_regs_hex(&[("esp", 0x3000), ("eip", 0x2222), ("cs", 0)]);
    let fake = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("qEmucap,setregpoint,{}", hex::encode(spec.as_bytes())),
            "RP:3".into(),
        ),
        ("qEmucap,pollreset".into(), "NONE".into()),
        ("g".into(), regs),
    ])
    .with_nonblocking(&["T05regwatch:00100000;idx:3;"]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());

    let set = bridge.handle_request(Request::new(
        17,
        "watch_register",
        json!({"register": "sp", "min": "0x1000", "max": "0x2000"}),
    ));
    assert_eq!(set.result.unwrap()["id"], 1);
    bridge.frozen = false;

    let events = bridge.handle_request(Request::new(18, "poll_events", json!({})));
    assert_eq!(
        events.result.unwrap()["events"][0],
        json!({
            "type": "register_break",
            "signal": "05",
            "raw": "T05regwatch:00100000;idx:3;",
            "pc": 0x1000,
            "address": 0x1000,
            "backend_id": 3,
            "id": 1,
            "breakpoint_id": 1,
            "register": "sp",
            "min": 0x1000,
            "max": 0x2000,
            "value": 0x3000,
            "regs": {
                "cpu.eax": 0, "cpu.ecx": 0, "cpu.edx": 0, "cpu.ebx": 0,
                "cpu.esp": 0x3000, "cpu.ebp": 0, "cpu.esi": 0, "cpu.edi": 0,
                "cpu.eip": 0x2222, "cpu.eflags": 0, "cpu.cs": 0, "cpu.ss": 0,
                "cpu.ds": 0, "cpu.es": 0, "cpu.fs": 0, "cpu.gs": 0,
                "cpu.offset_pc": 0x2222, "cpu.pc": 0x2222,
            },
        })
    );
}

#[test]
fn run_frames_sends_lua_command_with_scaled_timeout() {
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        ("qEmucap,runframes,33303030", "OK"),
        ("qEmucap,frame", "42"),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(14, "run_frames", json!({"n": 3000})));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["frames"], 3000);
    assert_eq!(result["frame"], 42);
    assert_eq!(result["state"], "running");
    assert!(bridge
        .gdb
        .timeouts
        .iter()
        .any(|t| *t > Duration::from_secs(5)));
    assert_eq!(bridge.gdb.get_timeout().unwrap(), Duration::from_secs(5));
}

#[test]
fn run_frames_rejects_work_past_backend_deadline_before_mutation() {
    let fake = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let requested = bridge.max_sync_frame_count() + 1;
    let response = bridge.handle_request(Request::new(14, "run_frames", json!({"n": requested})));

    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert_eq!(bridge.gdb.calls, vec!["?"]);
}

#[test]
fn press_buttons_reports_breakpoint_interruption_and_releases_operation() {
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        (
            "qEmucap,press,31303a656e746572",
            "T05hwbreak:01000000;idx:2;",
        ),
        ("qEmucap,frame", "77"),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(
        15,
        "press_buttons",
        json!({"buttons":["start"],"frames":10}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["reason"], "breakpoint");
    assert_eq!(result["raw"], "T05hwbreak:01000000;idx:2;");
    assert_eq!(result["buttons"], json!(["enter"]));
    assert_eq!(result["frames"], 10);
    assert_eq!(result["frame"], 77);
}

#[test]
fn input_rejects_nonzero_port_and_oversized_pulse_before_mutation() {
    for (method, params) in [
        ("set_input", json!({"port": 1, "buttons": ["start"]})),
        (
            "press_buttons",
            json!({"port": 1, "buttons": ["start"], "frames": 1}),
        ),
        (
            "press_buttons",
            json!({
                "buttons": ["start"],
                "frames": MAX_SYNC_TIMED_INPUT_FRAMES + 1
            }),
        ),
    ] {
        let fake = FakeGdb::with(&[("?", "S05")]);
        let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
        let response = bridge.handle_request(Request::new(19, method, params));
        assert!(!response.ok, "{method} must reject before mutation");
        assert_eq!(response.error.unwrap().kind, "bad_params");
        assert_eq!(bridge.gdb.calls, ["?"]);
    }
}

#[test]
fn status_reports_plugin_input_ownership() {
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        ("qEmucap,inputfields", "enter"),
        ("qEmucap,inputstatus", "-1"),
        ("qEmucap,frame", "42"),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(20, "status", json!({})));
    let result = response.result.unwrap();
    let input = &result["input_override"];
    assert_eq!(input["observable"], true);
    assert_eq!(input["engaged"], true);
    assert_eq!(input["mode"], "persistent");
    assert_eq!(
        result["execution_limits"]["frame"]["max_count"],
        bridge.max_sync_frame_count()
    );
    assert_eq!(
        result["execution_limits"]["max_sync_operation_ms"],
        crate::live::temporal::MAX_SYNC_OPERATION_TIME.as_millis() as u64
    );
}

#[test]
fn step_frames_returns_interrupted_on_stop_reply() {
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        ("qEmucap,framestep,3130", "T05hwbreak:01000000;idx:2;"),
        ("qEmucap,frame", "77"),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(15, "step", json!({"frames": 10})));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["reason"], "breakpoint");
    assert_eq!(result["raw"], "T05hwbreak:01000000;idx:2;");
    assert_eq!(result["frame"], 77);
}

#[test]
fn step_frames_drains_immediate_stop_after_ok() {
    // The stop arrives *after* the framestep "OK" (a frame-target that coincides with a BP hit),
    // so drain_immediate_stops must pick it up as the result. Enqueued on the framestep send so
    // the new pre-command drain (which only sees stops buffered *before* the command) can't eat
    // it early.
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        ("qEmucap,framestep,31", "OK"),
        ("qEmucap,frame", "9"),
    ])
    .enqueue_nonblocking_after("qEmucap,framestep,31", "S05");
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(16, "step", json!({"frames": 1})));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["raw"], "S05");
    assert_eq!(result["frame"], 9);
}

#[test]
fn step_instructions_sends_gdb_single_step_count() {
    let fake = FakeGdb::with(&[("?", "S05"), ("s", "S05"), ("s", "S05"), ("s", "S05")]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response =
        bridge.handle_request(Request::new(17, "step_instructions", json!({"count": 3})));
    let result = response.result.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["unit"], "instructions");
    assert_eq!(result["count"], 3);
    assert_eq!(
        bridge
            .gdb
            .calls
            .iter()
            .filter(|call| call.as_str() == "s")
            .count(),
        3
    );
}

#[test]
fn step_instructions_rejects_over_sync_cap_before_mutation() {
    let fake = FakeGdb::with(&[("?", "S05")]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(
        17,
        "step_instructions",
        json!({"count": crate::live::temporal::MAX_SYNC_ADVANCE_COUNT + 1}),
    ));

    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "bad_params");
    assert_eq!(bridge.gdb.calls, vec!["?"]);
}

struct DasmGdb;

impl GdbTransport for DasmGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        let prefix = "qEmucap,dasm,";
        if let Some(hex_spec) = payload.strip_prefix(prefix) {
            let bytes = hex::decode(hex_spec)
                .map_err(|_| GdbError::Emulator("bad dasm spec hex".into()))?;
            let spec = String::from_utf8(bytes)
                .map_err(|_| GdbError::Emulator("bad dasm spec utf8".into()))?;
            let mut parts = spec.split('|');
            let path = parts
                .next()
                .ok_or_else(|| GdbError::Emulator("missing dasm path".into()))?;
            let address = parts
                .next()
                .ok_or_else(|| GdbError::Emulator("missing dasm address".into()))?;
            let len = parts
                .next()
                .ok_or_else(|| GdbError::Emulator("missing dasm length".into()))?;
            assert_eq!(address, "1000");
            assert_eq!(len, "20");
            std::fs::write(
                path,
                "00001000: b8 34 12 mov ax,1234\n00001003: cd 18 int 18\n",
            )?;
            return Ok("OK".into());
        }
        Err(GdbError::Emulator(format!("unexpected call: {payload}")))
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

#[test]
fn disassemble_uses_lua_dasm_and_parses_instruction_rows() {
    let mut bridge = Bridge::new(DasmGdb, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(
        19,
        "disassemble",
        json!({"address":"0x1000","count":2}),
    ));
    let result = response.result.unwrap();
    assert_eq!(
        result["instructions"],
        json!([
            {"addr": 0x1000, "text": "mov ax,1234", "bytes": "b83412"},
            {"addr": 0x1003, "text": "int 18", "bytes": "cd18"},
        ])
    );
}

#[derive(Default)]
struct TraceGdb {
    path: Option<PathBuf>,
    flushes: usize,
    stops: usize,
}

impl TraceGdb {
    fn write_trace(&self) -> GdbResult<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        std::fs::write(
            path,
            concat!(
                "00001000: e8 00 00 call 2000\n",
                "00002000: 90 nop\n",
                "00002001: c3 ret\n",
                "00001003: e8 00 00 call 3000\n",
            ),
        )?;
        Ok(())
    }
}

impl GdbTransport for TraceGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        let prefix = "qEmucap,tracestart,";
        if let Some(hex_path) = payload.strip_prefix(prefix) {
            let bytes = hex::decode(hex_path)
                .map_err(|_| GdbError::Emulator("bad trace path hex".into()))?;
            let path = String::from_utf8(bytes)
                .map_err(|_| GdbError::Emulator("bad trace path utf8".into()))?;
            self.path = Some(PathBuf::from(path));
            self.write_trace()?;
            return Ok("OK".into());
        }
        if payload == "qEmucap,traceflush" {
            self.flushes += 1;
            self.write_trace()?;
            return Ok("OK".into());
        }
        if payload == "qEmucap,tracestop" {
            self.stops += 1;
            return Ok("OK".into());
        }
        Err(GdbError::Emulator(format!("unexpected call: {payload}")))
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

#[test]
fn trace_methods_manage_lua_trace_file_and_parse_rows() {
    let mut bridge = Bridge::new(TraceGdb::default(), GdbBridgeEnv::default());
    let started = bridge.handle_request(Request::new(20, "set_trace", json!({"enabled": true})));
    let started = started.result.unwrap();
    assert_eq!(started["tracing"], true);
    assert!(started["path"]
        .as_str()
        .unwrap()
        .contains("emucap_pc98_trace_"));

    let trace = bridge.handle_request(Request::new(21, "get_trace", json!({"count": 2})));
    let trace = trace.result.unwrap();
    assert_eq!(trace["tracing"], true);
    assert_eq!(trace["total"], 4);
    assert_eq!(
        trace["trace"],
        json!([
            {"pc": 0x2001, "text": "ret", "raw": "00002001: c3 ret", "bytes": "c3"},
            {"pc": 0x1003, "text": "call 3000", "raw": "00001003: e8 00 00 call 3000", "bytes": "e80000"},
        ])
    );

    let stack = bridge.handle_request(Request::new(22, "call_stack", json!({})));
    let stack = stack.result.unwrap();
    assert_eq!(stack["call_stack"], json!([0x1003]));
    assert_eq!(stack["depth"], 1);
    assert_eq!(
        stack["frames"],
        json!([{"pc": 0x1003, "text": "call 3000"}])
    );

    let stopped = bridge.handle_request(Request::new(23, "set_trace", json!({"enabled": false})));
    assert_eq!(stopped.result.unwrap()["tracing"], false);
    assert_eq!(bridge.gdb.stops, 1);
}

#[test]
fn status_drains_nonblocking_stop_when_running() {
    let fake = FakeGdb::with(&[
        ("?", ""),
        ("qEmucap,inputfields", "enter,esc,space,a,b"),
        ("qEmucap,inputstatus", "0"),
        ("qEmucap,frame", "12"),
    ])
    .with_nonblocking(&["S05"]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    bridge.frozen = false;
    let response = bridge.handle_request(Request::new(18, "status", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["state"], "frozen");
    assert_eq!(
        result["input_buttons"]["available"],
        json!(["enter", "esc", "space", "a", "b"])
    );
}

#[test]
fn dump_memory_writes_regions_under_requested_directory() {
    let mut replies = vec![("?".to_string(), "S05".to_string())];
    for name in DUMP_REGION_NAMES {
        let region = memory_region(name).unwrap();
        let mut offset = 0usize;
        while offset < region.size as usize {
            let chunk = MAX_READ_CHUNK.min(region.size as usize - offset);
            replies.push((
                format!("m{:x},{:x}", region.base as usize + offset, chunk),
                "00".repeat(chunk),
            ));
            offset += chunk;
        }
    }
    let mut bridge = Bridge::new(FakeGdb::from_pairs(replies), GdbBridgeEnv::default());
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dump");
    let response = bridge.handle_request(Request::new(
        12,
        "dump_memory",
        json!({"path": out.to_str().unwrap()}),
    ));
    assert_eq!(response.result.unwrap()["regions"], DUMP_REGION_NAMES.len());
    let regions: Value =
        serde_json::from_slice(&std::fs::read(out.join("regions.json")).unwrap()).unwrap();
    assert_eq!(regions.as_array().unwrap().len(), DUMP_REGION_NAMES.len());
    assert_eq!(
        std::fs::metadata(out.join("ram.bin")).unwrap().len(),
        memory_region("ram").unwrap().size as u64
    );
    assert_eq!(
        std::fs::metadata(out.join("tvram.bin")).unwrap().len(),
        memory_region("tvram").unwrap().size as u64
    );
}

#[test]
fn save_state_writes_python_compatible_zip_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("state.zip");
    let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
    let mut bridge = Bridge::new(StateSaveGdb::new(regs.clone()), GdbBridgeEnv::default());

    let response = bridge.handle_request(Request::new(
        24,
        "save_state",
        json!({"path": out.display().to_string()}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["format"], STATE_FORMAT);
    assert_eq!(
        result["save_items"],
        json!({"items": 1, "skipped": 0, "dir": SAVE_ITEMS_DIR})
    );
    assert!(result["bytes"].as_u64().unwrap() > 0);
    assert!(bridge.gdb.reads > 0);

    let file = File::open(&out).unwrap();
    let mut zip = ZipArchive::new(file).unwrap();
    assert!(zip.by_name(SAVE_ITEMS_MANIFEST).is_ok());
    let manifest = read_state_manifest(&mut zip).unwrap();
    assert_eq!(manifest["format"], STATE_FORMAT);
    assert_eq!(manifest["registers_hex"], regs);
    assert_eq!(
        manifest["regions"].as_array().unwrap().len(),
        DUMP_REGION_NAMES.len()
    );
}

#[test]
fn save_state_preserves_prior_save_on_mid_save_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("state.zip");
    // A pre-existing valid savestate with a distinct byte fingerprint. A mid-save failure must
    // leave it byte-for-byte intact — never truncated by an in-place File::create.
    let prior = b"PRIOR-VALID-SAVESTATE-BYTES".to_vec();
    std::fs::write(&out, &prior).unwrap();

    let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
    // Fail on the first region read (timeout mid-zip), after the staging file is created.
    let mut bridge = Bridge::new(
        StateSaveGdb::failing_at_read(regs, 1),
        GdbBridgeEnv::default(),
    );
    let response = bridge.handle_request(Request::new(
        60,
        "save_state",
        json!({"path": out.display().to_string()}),
    ));
    assert!(
        !response.ok,
        "a mid-save read failure must be reported as an error"
    );

    // The prior savestate survives byte-for-byte (not truncated, not overwritten).
    assert_eq!(
        std::fs::read(&out).unwrap(),
        prior,
        "the pre-existing savestate must survive a mid-save failure"
    );

    // The staging .partial temp is cleaned up, not left behind.
    let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".partial"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the staging .partial temp must be removed: {leftovers:?}"
    );
}

#[test]
fn run_frames_drains_pre_command_stale_stop() {
    // A buffered stop left after a prior resume() that hit a pause_on_hit BP sits ahead of the
    // frames command. Without a pre-command drain, drain_immediate_stops mis-consumes it as the
    // frames result → spurious interrupted+frozen. Draining it first routes it to the event queue
    // and the frames run completes normally (frozen stays false).
    let gdb = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("qEmucap,runframes,{}", hex::encode("3")),
            "OK".into(),
        ),
        ("qEmucap,frame".into(), "42".into()),
    ])
    .with_nonblocking(&["T05hwbreak:00100000;idx:2"]);
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(50, "run_frames", json!({"n": 3})));
    let result = response.result.unwrap();
    assert_eq!(
        result["status"], "completed",
        "the buffered stop must not be mis-consumed as the frames result"
    );
    assert_eq!(result["frames"], 3);
    assert_eq!(result["state"], "running");
    assert!(
        !bridge.frozen,
        "frozen must stay false after a completed run"
    );
    // The buffered stop was drained to the event queue, not returned as the frames result.
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00100000;idx:2");
}

#[test]
fn step_framestep_drains_pre_command_stale_stop() {
    let gdb = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("qEmucap,framestep,{}", hex::encode("2")),
            "OK".into(),
        ),
        ("qEmucap,frame".into(), "7".into()),
    ])
    .with_nonblocking(&["T05hwbreak:00200000;idx:3"]);
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(
        51,
        "step",
        json!({"frames": 2, "unit": "frames"}),
    ));
    let result = response.result.unwrap();
    assert_eq!(
        result["status"], "completed",
        "the buffered stop must not be mis-consumed as the framestep result"
    );
    assert_eq!(result["frames"], 2);
    // The buffered stop was drained to the event queue, not returned as the framestep result.
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00200000;idx:3");
}

#[test]
fn pause_preserves_real_bp_hit_buffered_before_interrupt() {
    // A pause_on_hit BP fired and its stop is buffered just before pause() injects an interrupt.
    // Pause must drain that real hit to the event queue before issuing its own interrupt.
    let regs = i386_regs_hex(&[("eip", 0x1234), ("cs", 0)]);
    let gdb = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        ("qEmucap,pollreset".into(), "NONE".into()),
        ("g".into(), regs),
    ])
    .with_nonblocking(&["T05hwbreak:00100000;idx:2"]);
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    bridge.frozen = false; // core running, so pause() actually injects an interrupt
    bridge.pause().unwrap();
    bridge.resume().unwrap();
    let response = bridge.handle_request(Request::new(80, "poll_events", json!({})));
    let events = response.result.unwrap()["events"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        events.len(),
        1,
        "the real BP hit must surface and the echo must not: {events:?}"
    );
    assert_eq!(events[0]["raw"], "T05hwbreak:00100000;idx:2");
    assert_eq!(events[0]["type"], "breakpoint_hit");
}

#[test]
fn load_state_restores_save_items_memory_and_registers() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state.zip");
    let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
    write_test_state(&state, &regs);
    let mut bridge = Bridge::new(StateLoadGdb::new(regs), GdbBridgeEnv::default());

    let response = bridge.handle_request(Request::new(
        25,
        "load_state",
        json!({"path": state.display().to_string()}),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["format"], STATE_FORMAT);
    assert_eq!(result["regions"], 1);
    assert_eq!(result["save_items_restored"], 1);
    assert_eq!(result["restore_strategy"], "lua_register_load_hold");
    assert_eq!(result["post_restore_instruction_exact"], true);
    assert_eq!(bridge.gdb.writes, vec!["Ma0000,2:aabb"]);
    assert_eq!(bridge.gdb.load_items_dirs.len(), 1);
}

#[test]
fn probe_restores_state_and_uses_lua_register_probe() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state.zip");
    let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234)]);
    write_test_state(&state, &regs);
    let mut bridge = Bridge::new(StateLoadGdb::new(regs.clone()), GdbBridgeEnv::default());

    let response = bridge.handle_request(Request::new(
        26,
        "probe",
        json!({
            "state": state.display().to_string(),
            "frame": 3,
            "memory_type": "tvram",
            "address": 0,
            "length": 2
        }),
    ));
    let result = response.result.unwrap();
    assert_eq!(result["hex"], "cafe");
    assert_eq!(result["frame"], 3);
    assert_eq!(result["save_items_restored"], 1);
    assert_eq!(bridge.gdb.writes, vec!["Ma0000,2:aabb"]);
    assert_eq!(bridge.gdb.regprobe_specs, vec![format!("{regs}|3|a0000|2")]);
}

#[test]
fn get_state_decodes_i386_register_packet_and_segmented_pc() {
    let regs = i386_regs_hex(&[("eip", 0x8000), ("cs", 0x1234), ("esp", 0xAA55)]);
    let mut bridge = Bridge::new(
        FakeGdb::with(&[("?", "S05"), ("g", &regs)]),
        GdbBridgeEnv::default(),
    );
    let response = bridge.handle_request(Request::new(4, "get_state", json!({})));
    let state = &response.result.unwrap()["state"];
    assert_eq!(state["cpu.eip"], 0x8000);
    assert_eq!(state["cpu.esp"], 0xAA55);
    assert_eq!(state["cpu.pc"], 0x12340 + 0x8000);
}

#[test]
fn get_rom_info_hashes_content_path() {
    let tmp = tempfile::tempdir().unwrap();
    let disk = tmp.path().join("game.hdi");
    std::fs::write(&disk, b"pc98").unwrap();
    let env = GdbBridgeEnv {
        content: Some(disk.clone()),
        ..Default::default()
    };
    let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), env);
    let response = bridge.handle_request(Request::new(5, "get_rom_info", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["name"], "game.hdi");
    assert_eq!(result["size"], 4);
    assert_eq!(result["media_type"], "hdi");
    assert_eq!(result["sha1"], sha1_file(&disk).unwrap());
}

#[test]
fn unknown_method_uses_protocol_unknown_method_kind() {
    let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(6, "not_a_method", json!({})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "unknown_method");
}

struct SnapshotGdb;

impl GdbTransport for SnapshotGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        if payload == "qEmucap,frame" {
            return Ok("42".into());
        }
        let prefix = "qEmucap,snapshot,";
        if let Some(hex_path) = payload.strip_prefix(prefix) {
            let bytes = hex::decode(hex_path)
                .map_err(|_| GdbError::Emulator("bad snapshot path hex".into()))?;
            let path = String::from_utf8(bytes)
                .map_err(|_| GdbError::Emulator("bad snapshot path utf8".into()))?;
            std::fs::write(path, b"\x89PNG\r\n\x1a\nfake")?;
            return Ok("OK".into());
        }
        Err(GdbError::Emulator(format!("unexpected call: {payload}")))
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

#[test]
fn screenshot_returns_png_base64_from_lua_snapshot() {
    let mut bridge = Bridge::new(SnapshotGdb, GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(13, "screenshot", json!({})));
    let result = response.result.unwrap();
    assert_eq!(
        result["png_base64"],
        base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfake")
    );
    assert_eq!(
        result["sha256"],
        format!("{:x}", Sha256::digest(b"\x89PNG\r\n\x1a\nfake"))
    );
    assert_eq!(result["byte_len"], 12);
    assert_eq!(result["state"], "frozen");
    assert_eq!(result["frame_before"], 42);
    assert_eq!(result["frame_after"], 42);
    assert_eq!(result["frame_stable"], true);
    assert_eq!(result["freshness"], "unverified");
    assert_eq!(result["frame_binding"], "unverified");
}

// P1: stale async stop이 데이터 명령의 응답 자리에 오배달돼도 send_cmd가 이벤트 큐로
// 걷어내고 진짜 응답을 이어 읽어 off-by-one 디싱크를 막는지 검증한다.
#[derive(Default)]
struct StaleStopGdb {
    stale: Option<String>,
    reply: String,
}

impl GdbTransport for StaleStopGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        Ok(self.stale.take().unwrap_or_else(|| self.reply.clone()))
    }

    fn recv_reply(&mut self) -> GdbResult<String> {
        Ok(self.reply.clone())
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

#[test]
fn send_cmd_demuxes_stale_async_stop_ahead_of_data_reply() {
    let gdb = StaleStopGdb {
        stale: Some("T05".into()),
        reply: "OK".into(),
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let resp = bridge
        .send_cmd("qEmucap,setinput,656e746572")
        .expect("send_cmd returns the real reply");
    assert_eq!(resp, "OK");
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["type"], "stop");
}

// F1: m 읽기(read_abs_hex → read_memory/dump_memory/save_state/find_pattern/probe/call_stack)가
// send_cmd demux를 경유해, 응답 앞에 낀 stale async stop을 이벤트 큐로 걷어내고 진짜 hex
// 응답을 반환하는지 검증한다. raw send면 stop이 hex 자리에 오배달돼 디코드 실패 + off-by-one.
struct StaleStopReadGdb {
    stale: Option<String>,
    hex: String,
    reads: Vec<String>,
}

impl GdbTransport for StaleStopReadGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        self.reads.push(payload.into());
        Ok(self.stale.take().unwrap_or_else(|| self.hex.clone()))
    }

    fn recv_reply(&mut self) -> GdbResult<String> {
        Ok(self.hex.clone())
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

#[test]
fn read_abs_hex_demuxes_stale_stop_ahead_of_memory_reply() {
    let gdb = StaleStopReadGdb {
        stale: Some("T05hwbreak:00100000;idx:1".into()),
        hex: "deadbeef".into(),
        reads: Vec::new(),
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let hex = bridge
        .read_abs_hex(0x1234, 4)
        .expect("read_abs_hex returns the real hex reply");
    assert_eq!(hex, "deadbeef");
    // stale stop이 hex 응답 자리에 오배달되지 않고 이벤트 큐로 걷혔다.
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00100000;idx:1");
    // 데이터 명령이 실제 m 읽기였는지(demux는 m 자체는 건드리지 않는다).
    assert!(
        bridge.gdb.reads.iter().any(|c| c.starts_with('m')),
        "issued an m read: {:?}",
        bridge.gdb.reads
    );
}

#[test]
fn send_cmd_drains_stale_ok_ahead_of_register_read() {
    // 트레이싱 중 runframes가 frame-target에 도달한 순간 BP도 히트하면, frame notifier의 완료 "OK"와
    // note_breakpoint의 BP stop이 하나의 runframes에 이중 응답한다. 브리지가 그중 하나를 소비하면 나머지
    // stale "OK"가 다음 데이터 명령(g=레지스터)의 응답 자리에 오배달돼 off-by-one desync된다
    // (get_state가 raw_register_bytes로 깨지고 이후 traceflush가 register 패킷을 받음). send_cmd는 데이터
    // 읽기 앞의 stale "OK"를 걷어내고 진짜 hex를 재읽기해야 한다.
    let gdb = StaleStopReadGdb {
        stale: Some("OK".into()),
        hex: "00ff0000160000008080".into(), // i386 레지스터 hex(축약)
        reads: Vec::new(),
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let resp = bridge.send_cmd_data("g").expect("g returns register hex");
    assert_eq!(
        resp, "00ff0000160000008080",
        "g는 stale OK가 아니라 레지스터 hex를 받아야(desync 없음)"
    );
}

#[test]
fn send_cmd_data_drains_stale_ok_ahead_of_frame_read() {
    // run_frames/step은 frames_op 직후 qEmucap,frame(current_frame)을 필수로 부른다. 이 경로도
    // send_cmd_data를 타야 stale bare "OK"가 frame 응답 자리에 오배달(→ 이후 g가 프레임 숫자를 레지스터로
    // 오소비)되는 것을 막는다. g/m 하드코딩이 아닌 명령-의도 기반이라 frame도 커버된다.
    let gdb = StaleStopReadGdb {
        stale: Some("OK".into()),
        hex: "2028".into(), // qEmucap,frame의 10진 프레임 번호
        reads: Vec::new(),
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let resp = bridge
        .send_cmd_data("qEmucap,frame")
        .expect("frame returns the decimal frame number");
    assert_eq!(
        resp, "2028",
        "frame은 stale OK가 아니라 프레임 번호를 받아야"
    );
}

#[test]
fn send_cmd_data_keeps_ok_pipe_data_reply() {
    // 회귀 가드: saveitems/loaditems/regload은 성공 시 "OK|<data>"를 반환한다 — bare "OK"가 아니므로
    // send_cmd_data가 이를 stale로 오인해 드레인하면 안 된다(그러면 hang). "OK|..."는 그대로 반환.
    let gdb = StaleStopReadGdb {
        stale: None,
        hex: "OK|3|0".into(),
        reads: Vec::new(),
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let resp = bridge
        .send_cmd_data("qEmucap,saveitems,2f74")
        .expect("saveitems returns OK|data");
    assert_eq!(resp, "OK|3|0", "\"OK|...\"는 유효 데이터라 드레인 금지");
}

// F3: s(instruction step)는 응답 자체가 stop이라 send_cmd demux가 스킵된다. 스텝 직전에
// 버퍼의 stale async stop을 걷어내(note_stop) s의 응답 자리 오배달을 막고, 진짜 스텝 완료
// stop을 응답으로 받아(re-read) instruction step이 유지되는지 검증한다.
struct StepStaleGdb {
    buffered: VecDeque<String>,
    step_reply: String,
    steps: usize,
}

impl GdbTransport for StepStaleGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        if payload == "s" {
            self.steps += 1;
            return Ok(self.step_reply.clone());
        }
        Err(GdbError::Emulator(format!("unexpected call: {payload}")))
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }

    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        Ok(self.buffered.pop_front())
    }
}

#[test]
fn step_instruction_drains_pre_command_stale_stop() {
    let gdb = StepStaleGdb {
        buffered: VecDeque::from(vec!["T05hwbreak:00100000;idx:2".to_string()]),
        step_reply: "S05".into(),
        steps: 0,
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    let response =
        bridge.handle_request(Request::new(40, "step_instructions", json!({"count": 1})));
    assert!(response.ok, "instruction step still completes");
    let result = response.result.unwrap();
    assert_eq!(result["unit"], "instructions");
    assert_eq!(result["count"], 1);
    // stale stop이 s의 응답 자리에 오배달되지 않고 이벤트 큐로 걷혔다.
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["raw"], "T05hwbreak:00100000;idx:2");
    // 스텝은 실제로 한 번 실행됐다(stale를 스텝 완료로 오인하지 않음).
    assert_eq!(bridge.gdb.steps, 1);
}

// P2: 머신 ioport에 없는 버튼을 눌렀을 때, 어느 버튼이 없고 무엇이 가능한지 이름을 붙여
// 반환하는지 검증한다(맨몸 E08 패스스루 금지).
#[test]
fn set_input_names_unavailable_button_and_lists_machine_fields() {
    let fake = FakeGdb::from_pairs(vec![
        ("?".into(), "S05".into()),
        (
            format!("qEmucap,setinput,{}", hex::encode("help")),
            "E08:help".into(),
        ),
        ("qEmucap,inputfields".into(), "a,b,enter,esc,space".into()),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    let response =
        bridge.handle_request(Request::new(30, "set_input", json!({"buttons": ["help"]})));
    assert!(!response.ok);
    let msg = response.error.unwrap().message;
    assert!(msg.contains("help"), "names the unavailable button: {msg}");
    assert!(
        msg.contains("enter") && msg.contains("space"),
        "lists the machine-registered fields: {msg}"
    );
}

// P3: 트레이스 없이 정지 상태의 BP(EBP) 체인을 걸어 호출 스택을 복원하고, 어느 방법을
// 썼는지 method 필드로 알리는지 검증한다.
struct CallStackFpGdb {
    regs_hex: String,
    mem: BTreeMap<u64, u64>,
}

impl GdbTransport for CallStackFpGdb {
    fn send(&mut self, payload: &str) -> GdbResult<String> {
        if payload == "?" {
            return Ok("S05".into());
        }
        if payload == "g" {
            return Ok(self.regs_hex.clone());
        }
        if let Some(rest) = payload.strip_prefix('m') {
            let (addr_hex, len_hex) = rest
                .split_once(',')
                .ok_or_else(|| GdbError::Emulator(format!("bad read: {payload}")))?;
            let addr = u64::from_str_radix(addr_hex, 16)
                .map_err(|_| GdbError::Emulator(format!("bad addr: {payload}")))?;
            let len = usize::from_str_radix(len_hex, 16)
                .map_err(|_| GdbError::Emulator(format!("bad len: {payload}")))?;
            let value = self.mem.get(&addr).copied().unwrap_or(0);
            return Ok(hex::encode(&value.to_le_bytes()[..len]));
        }
        Err(GdbError::Emulator(format!("unexpected call: {payload}")))
    }

    fn send_no_reply(&mut self, _payload: &str) -> GdbResult<()> {
        Ok(())
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        Ok("S05".into())
    }
}

#[test]
fn call_stack_walks_frame_pointer_chain_without_trace() {
    // eip를 16비트를 넘겨 protected32(포인터 4바이트, 평면 SS)로 판정되게 한다.
    let regs = i386_regs_hex(&[("eip", 0x0010_0000), ("ebp", 0x1000), ("esp", 0x0FF0)]);
    let mem = BTreeMap::from([
        (0x1000u64, 0x1100u64), // saved_bp
        (0x1004u64, 0xAAAAu64), // ret addr, frame 1
        (0x1100u64, 0x0000u64), // saved_bp = 0 → 체인 종료
        (0x1104u64, 0xBBBBu64), // ret addr, frame 2
    ]);
    let mut bridge = Bridge::new(
        CallStackFpGdb {
            regs_hex: regs,
            mem,
        },
        GdbBridgeEnv::default(),
    );
    let response = bridge.handle_request(Request::new(31, "call_stack", json!({})));
    let result = response.result.unwrap();
    assert_eq!(result["method"], "frame_pointer");
    assert_eq!(result["call_stack"], json!([0xAAAA, 0xBBBB]));
    assert_eq!(result["depth"], 2);
}

#[test]
fn gdb_backend_and_stream_errors_keep_distinct_protocol_kinds() {
    let mut bridge = Bridge::new(FakeGdb::with(&[("?", "S05")]), GdbBridgeEnv::default());
    let response = bridge.handle_request(Request::new(32, "get_state", json!({})));
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().kind, "emulator_error");

    let stream_error = BridgeError::Gdb(GdbError::Io(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "fake reset",
    )));
    assert_eq!(error_kind(&stream_error), "bridge_error");
}
