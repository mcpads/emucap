#!/usr/bin/env python3
"""Run the bridge's actual Tcl frame-target procedures with a controlled event queue."""
from pathlib import Path
import re
import subprocess

root = Path(__file__).resolve().parents[3]
source = (root / "src/openmsx_bridge/frame.rs").read_text()
match = re.search(r'const FRAME_TCL: &str = r#"(.*?)"#;', source, re.S)
assert match, "frame target Tcl source missing"
fixture = r'''
set queued {}
set resumes 0
set pause on
rename after real_after
proc after {kind delay command} { lappend ::queued $command }
proc debug {action args} {
    if {$action eq "cont"} { incr ::resumes }
}
proc check {condition message} {
    if {![uplevel 1 [list expr $condition]]} { error $message }
}
'''
cases = r'''
::emucap::next_frame 3
set stale [lindex $queued end]
::emucap::cancel_frame
uplevel #0 $stale
check {$pause eq "on" && $resumes == 0 && $::emucap::frame_target eq {}} "cancelled start resumed"

::emucap::next_frame 2
set current [lindex $queued end]
uplevel #0 $stale
check {$resumes == 0} "stale start affected replacement"
uplevel #0 $current
check {$resumes == 1 && $pause eq "off" && $::emucap::frame_target == 2} "current start failed"
::emucap::frame_tick
check {$pause eq "off"} "target stopped early"
::emucap::frame_tick
check {$pause eq "on" && $::emucap::frame_target eq {}} "target failed to stop"

uplevel #0 $current
check {$pause eq "on" && $resumes == 1} "completed start resumed again"

::emucap::next_frame 100
set pending [lindex $queued end]
::emucap::cancel_frame
::emucap::cancel_frame
uplevel #0 $pending
::emucap::frame_tick
check {$pause eq "on" && $resumes == 1 && $::emucap::frame_target eq {}} "duplicate cancel allowed resume"
puts "frame callback lifetime checks passed"
'''
# Tcl's interactive stdin mode may print an error and still exit zero. Explicitly exit on failure.
script = fixture + match.group(1) + "\nif {[catch {\n" + cases + "\n} message]} {puts stderr $message; exit 1}\n"
subprocess.run(["tclsh"], input=script, text=True, check=True)
