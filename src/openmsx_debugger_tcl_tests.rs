use std::io::Write;
use std::process::{Command, Stdio};

use super::*;

fn produce_hit(spec_command: &str, kind: &str) -> String {
    // Only the emulator primitives are synthetic. Serialization, queue and drain
    // are the exact script installed by the bridge.
    let script = format!(
        r#"
proc reg {{name}} {{
    if {{$name eq "PC"}} {{return 16384}}
    return 42
}}
proc debug {{command args}} {{
    if {{$command eq "break"}} {{return}}
    if {{$command ne "read_block"}} {{error "unexpected debug command"}}
    lassign $args space address length
    set bytes {{}}
    for {{set i 0}} {{$i < $length}} {{incr i}} {{
        append bytes [binary format c [expr {{($address + $i) & 255}}]]
    }}
    return $bytes
}}
if {{[catch {{
{producer}
{spec_command}
set ::wp_last_address 16384
set ::wp_last_value 90
::emucap::hit 1 {kind}
puts [::emucap::drain]
puts [::emucap::drain]
if {{$::pause ne "on"}} {{error "callback did not pause"}}
}} message]}} {{puts stderr $message; exit 1}}
"#,
        producer = super::super::breakpoints::DEBUGGER_TCL,
    );
    let mut child = Command::new(std::env::var("TCLSH").unwrap_or_else(|_| "tclsh".into()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("install Tcl 8.6+ or set TCLSH for this integration test");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert_eq!(hex::decode(lines[1]).unwrap(), b"0\n");
    lines[0].into()
}

#[test]
#[ignore = "requires Tcl 8.6+; run explicitly with --ignored"]
fn tcl_producer_round_trips_snapshot_ranges_and_rejects_corruption() {
    for kind in ["exec", "read", "write"] {
        for count in [0, 1, 2, 16] {
            let (mut bridge, commands, _temp) = fixture(false);
            bridge.launch_id = Some("launch-tcl-test".into());
            let ranges = (0..count)
                .map(|i| {
                    format!(
                        "{}:{}:{}",
                        ["memory", "ram", "vram"][i % 3],
                        8192 + i * 16,
                        i + 1
                    )
                })
                .collect::<Vec<_>>();
            result(bridge.handle_request(Request::new(
                1,
                "set_breakpoint",
                json!({
                    "kind":kind, "start":16384, "memory_type":"memory", "snapshot":ranges,
                }),
            )));
            let command = commands
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.starts_with("::emucap::set_spec "))
                .unwrap()
                .clone();
            let encoded = produce_hit(&command, if kind == "exec" { "x" } else { &kind[..1] });
            bridge.control.debugger_drain = encoded.clone();
            let events = result(bridge.handle_request(Request::new(2, "poll_events", json!({}))));
            assert_eq!(events["dropped"], 0);
            assert_eq!(events["events"].as_array().unwrap().len(), 1);
            let event = &events["events"][0];
            assert_eq!(event["kind"], kind);
            assert_eq!(event["breakpoint_id"], 1);
            assert_eq!(event["hit_seq"], 1);
            assert_eq!(event["launch_id"], "launch-tcl-test");
            assert_eq!(event["address"], 16384);
            assert_eq!(event["pc"], 16384);
            assert_eq!(event["regs"]["PC"], 16384);
            for register in [
                "AF", "BC", "DE", "HL", "AF2", "BC2", "DE2", "HL2", "IX", "IY", "SP", "I", "R",
                "IM", "IFF",
            ] {
                assert_eq!(event["regs"][register], 42);
            }
            assert_eq!(event.get("value"), (kind == "write").then_some(&json!(90)));
            assert_eq!(event["snapshot"].as_array().unwrap().len(), count);
            for i in 0..count {
                let bytes = (0..=i)
                    .map(|j| ((i * 16 + j) & 255) as u8)
                    .collect::<Vec<_>>();
                assert_eq!(
                    event["snapshot"][i],
                    json!({
                        "memory_type":(["memory", "ram", "vram"][i % 3]),
                        "address":8192+i*16, "length":i+1, "hex":hex::encode(bytes),
                    })
                );
            }
            assert!(!bridge.backend_terminal());
            if count == 2 {
                // Parse malformed real producer output with an otherwise fresh hit sequence.
                bridge.last_hit_seq = 0;
                let text = String::from_utf8(hex::decode(encoded).unwrap()).unwrap();
                for corrupt in [
                    text.replace(";", " "),
                    text.replace("m,8192,1,00", "m,8193,1,00"),
                    text.replace("m,8192,1,00", "m,8192,1,0000"),
                    text.replace("m,8192,1,00", "m,8192,1,zz"),
                ] {
                    bridge.control.debugger_drain = hex::encode(corrupt);
                    assert!(bridge.drain_debug_events().is_err());
                    assert!(bridge.backend_terminal());
                }
            }
        }
    }
}
