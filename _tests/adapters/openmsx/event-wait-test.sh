#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
source_root="${1:?usage: event-wait-test.sh <patched-openmsx-source>}"
task_tmp="$(mktemp -d)"
trap 'rm -rf "$task_tmp"' EXIT
cp "$source_root/src/events/EventDistributor.cc" "$source_root/src/events/EventDistributor.hh" "$task_tmp/"
cp "$here/event-wait/"* "$task_tmp/"
for header in RTScheduler Interpreter InputEventGenerator; do
  printf '#pragma once\n' > "$task_tmp/$header.hh"
done
"${CXX:-c++}" -std=c++23 -O2 -pthread -I"$task_tmp" "$task_tmp/EventDistributor.cc" "$task_tmp/main.cc" -o "$task_tmp/event-wait"
"$task_tmp/event-wait"
