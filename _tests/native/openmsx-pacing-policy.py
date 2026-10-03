#!/usr/bin/env python3
"""Execute the shipped Tcl transaction with native-setting fault traces."""
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'src/openmsx_bridge/observation.rs').read_text()
script = source.split('const PACING_TCL: &str = r#"', 1)[1].split('"#;', 1)[0]
setup = r'''
set throttle true
set speed 100
set fastforward false
set fullspeedwhenloading false
set pause true
proc machine {} { return machine1 }
proc machine_info {what} { return 10.5 }
proc debug {what} { return 1 }
namespace eval ::emucap { proc frame_seq {} { return 10 } }
proc check {condition message} {
    if {![uplevel 1 [list expr $condition]]} { error $message }
}
'''
cases = {
    'observation_and_restore_interval': r'''
set observed [split [::emucap::observe_policy] {;}]
check {[lindex $observed 0] eq "machine1|10.5|10|true|1"} "observation boundary missing"
check {[lindex $observed 1] eq "0|true|100|false|false"} "observation policy missing"
set receipt [split [::emucap::apply_policy true 200] {;}]
set revision [lindex [split [lindex $receipt 2] |] 0]
set restored [split [::emucap::restore_policy $revision true 100 false false] {;}]
check {[lindex [split [lindex $restored 0] |] 2] == 100} "restored policy missing"
check {[lindex $restored 1] eq "machine1|10.5|10|true|1"} "restore endpoint missing"
''',
    'silent_apply_clamp': r'''
proc clamp {args} { if {$::speed == 200} { set ::speed 150 } }
trace add variable ::speed write clamp
set rc [catch {::emucap::apply_policy true 200} message]
check {$rc == 1 && [string match {*restored=1:*} $message]} "silent apply clamp escaped native transaction"
check {$speed == 100 && $throttle && !$fastforward && !$fullspeedwhenloading} "clamp rollback incomplete"
''',
    'running_failure_interval': r'''
set pause false
set now 10.5
set frames 10
proc machine_info {what} { return $::now }
proc ::emucap::frame_seq {} { return $::frames }
proc advance_then_fail {args} {
    if {$::speed == 200} {
        set ::now 10.75
        set ::frames 12
        error "injected partial interval"
    }
}
trace add variable ::speed write advance_then_fail
set rc [catch {::emucap::apply_policy true 200} message]
check {$rc == 1 && [string match {*restored=1:*} $message]} "restore failed"
check {[string first {clock_domains=openmsx_emutime_seconds,emucap_frame_seq} $message] >= 0} "missing clock domains"
check {[string first {before=machine1|10.5|10|false|1} $message] >= 0} "missing initial boundary"
check {[string first {after=machine1|10.75|12|false|1} $message] >= 0} "missing final boundary"
check {[string first {previous=0|true|100|false|false} $message] >= 0} "missing previous policy"
check {$speed == 100 && $now == 10.75 && $frames == 12} "policy restoration rewound progress"
''',
    'external_and_conflict': r'''
set before [::emucap::policy]
set speed 250
set external [::emucap::policy]
check {[lindex [split $external |] 2] == 250} "external speed invisible"
check {[lindex [split $external |] 0] > [lindex [split $before |] 0]} "revision did not advance"
set receipt [split [::emucap::apply_policy true 200] {;}]
check {[llength $receipt] == 4} "missing receipt"
set applied [split [lindex $receipt 2] |]
check {[lindex $applied 2] == 200} "application mismatch"
set speed 300
set rc [catch {::emucap::restore_policy [lindex $applied 0] true 250 false false} message]
check {$rc == 1 && [string match {*emucap-policy-conflict*} $message]} "conflict not reported"
check {$speed == 300} "external policy overwritten"
''',
    'verified_restore': r'''
proc fault {args} {
    if {$::speed == 200} { error "injected setter failure" }
    # Native settings may normalize equivalent numeric values.
    set ::speed 100.0
}
trace add variable ::speed write fault
set rc [catch {::emucap::apply_policy true 200} message]
check {$rc == 1 && [string match {*restored=1:*} $message]} "verified restoration not recognized"
check {$speed == 100 && $throttle && !$fastforward && !$fullspeedwhenloading} "previous settings lost"
''',
    'silent_restore_clamp': r'''
proc fault {args} {
    if {$::speed == 200} { error "injected setter failure" }
    set ::speed 150
}
trace add variable ::speed write fault
set rc [catch {::emucap::apply_policy true 200} message]
check {$rc == 1 && $speed == 150} "fault did not exercise partial restoration"
check {[string match {*restored=0:*} $message]} "silent restore clamp falsely certified"
''',
    'restore_error': r'''
proc fault {args} { error "setter unavailable" }
trace add variable ::speed write fault
set rc [catch {::emucap::apply_policy true 200} message]
check {$rc == 1 && [string match {*restored=0:*} $message]} "restore failure falsely certified"
''',
}
# Every effective-policy field is verified inside the owner transaction.
for setting, previous in [('throttle', 'false'),
                                     ('fastforward', 'true'),
                                     ('fullspeedwhenloading', 'true')]:
    cases['silent_apply_' + setting] = r'''
set ::SETTING PREVIOUS
proc clamp_apply {args} { set ::SETTING PREVIOUS }
trace add variable ::SETTING write clamp_apply
set rc [catch {::emucap::apply_policy true 200} message]
check {$rc == 1 && [string match {*restored=1:*} $message]} "boolean apply clamp escaped transaction"
check {$speed == 100 && [set ::SETTING] == PREVIOUS} "boolean clamp restoration lost previous policy"
'''.replace('SETTING', setting).replace('PREVIOUS', previous)

# Exercise each boolean restoration independently after the speed setter fails.
for setting, replacement in [('throttle', 'false'), ('fastforward', 'true'), ('fullspeedwhenloading', 'true')]:
    cases['silent_restore_' + setting] = r'''
set restoring false
proc fail_speed {args} {
    if {$::speed == 200} { set ::restoring true; error "injected setter failure" }
}
proc clamp_restore {name args} {
    if {$::restoring} { set $name REPLACEMENT }
}
trace add variable ::speed write fail_speed
trace add variable ::SETTING write clamp_restore
set rc [catch {::emucap::apply_policy true 200} message]
check {$rc == 1 && [string match {*restored=0:*} $message]} "boolean restore clamp falsely certified"
'''.replace('SETTING', setting).replace('REPLACEMENT', replacement)

tclsh = shutil.which('tclsh')
assert tclsh, 'tclsh is required'
for name, case in cases.items():
    with tempfile.TemporaryDirectory(prefix='emucap-tcl-policy-') as temp:
        path = Path(temp) / 'test.tcl'
        path.write_text(setup + '\n' + script + '\n' + case)
        run = subprocess.run([tclsh, str(path)], capture_output=True, text=True)
        if run.returncode:
            raise AssertionError(f'{name}: {run.stderr}')
        print(f'PASS {name}')
