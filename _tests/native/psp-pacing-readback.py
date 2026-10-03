#!/usr/bin/env python3
"""Exercise the maintained PSP native pacing transaction and result identity."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True, help='patched SteppingSubscriber.cpp')
args = parser.parse_args()
source = args.source.read_text()
start = source.index('void WebSocketSteppingState::Pacing(')
end = source.index('\n}', start) + 2
code = r'''
#include "EmucapPacing.h"
#include <cassert>
#include <cstdio>
#include <map>
#include <string>
struct JsonWriter {
    std::map<std::string, uint32_t> values;
    std::map<std::string, std::string> strings;
    std::string prefix;
    void writeUint(const char *key, uint32_t value) { values[prefix + key] = value; }
    void writeBool(const char *key, bool value) { values[prefix + key] = value; }
    void writeString(const char *key, const char *value) { strings[prefix + key] = value; }
    void pushDict(const char *key) { prefix = std::string(key) + "."; }
    void pop() { prefix.clear(); }
};
struct DebuggerRequest {
    JsonWriter writer;
    std::map<std::string,uint32_t> args;
    bool failed = false;
    bool HasParam(const char *key) { return args.count(key); }
    bool ParamBool(const char *key, bool *value) { *value = args.at(key) != 0; return true; }
    bool ParamU32(const char *key, uint32_t *value) { *value = args.at(key); return true; }
    void Fail(const char *) { failed = true; }
    JsonWriter &Respond() { return writer; }
};
struct WebSocketSteppingState { void Pacing(DebuggerRequest &); };
static EmucapPacingOwner owner;
static int applies = 0;
EmucapPacingSnapshot __DisplayGetEmucapPacing() { return owner.Read(); }
EmucapPacingApply __DisplayApplyEmucapSpeed(int percent) { ++applies; return owner.ApplySpeed(percent); }
int __DisplayGetNumVblanks() { return 9; }
''' + source[start:end] + r'''
int main() {
    WebSocketSteppingState handler;
    auto query = [&] { DebuggerRequest r; handler.Pacing(r); return r.writer.values; };
    auto initial = query();
    assert(initial["transaction_version"] == 1 && initial["percent"] == 100);
    assert(query()["revision"] == initial["revision"] && applies == 0);
    owner.SetPercent(250);
    owner.SetFastForward(true);
    owner.ChangeLimit(FPSLimit::NORMAL, FPSLimit::CUSTOM1);
    auto previous = owner.Read();
    assert(query()["revision"] == previous.revision && query()["fast_forward"]);
    uint32_t networkState = 0;
    owner.PublishApctlState(networkState, 4, true);
    for (bool unlimited : {false, true}) {
        DebuggerRequest r; r.args[unlimited ? "unlimited" : "percent"] = unlimited ? 1 : 400;
        auto before = owner.Read(); handler.Pacing(r);
        assert(!r.failed && r.writer.strings["outcome"] == "rejected");
        assert(owner.Read().revision == before.revision && owner.Read().percent == before.percent);
        assert(r.writer.values["previous.revision"] == r.writer.values["revision"]);
        assert(r.writer.values["network_forced"] && r.writer.values["previous.network_forced"]);
    }
    owner.PublishApctlState(networkState, 0, false);
    previous = owner.Read();
    DebuggerRequest set; set.args["percent"] = 400; handler.Pacing(set);
    assert(!set.failed && set.writer.strings["outcome"] == "completed");
    assert(set.writer.values["previous.percent"] == 250 && set.writer.values["previous.fast_forward"]);
    assert(set.writer.values["previous.fps_limit"] == 1 && set.writer.values["previous.revision"] == previous.revision);
    assert(set.writer.values["percent"] == 400 && !set.writer.values["fast_forward"] && !set.writer.values["fps_limit"]);
    assert(set.writer.values["revision"] == previous.revision + 1);
    assert(set.writer.strings["clock_domain"] == "psp_vblank" && set.writer.values["begin_vblank"] == 9);
    for (uint32_t invalid : {0u, 10001u}) {
        int before = applies; DebuggerRequest r; r.args["percent"] = invalid; handler.Pacing(r);
        assert(r.failed && applies == before);
    }
    DebuggerRequest unlimited; unlimited.args["unlimited"] = 1; handler.Pacing(unlimited);
    assert(!unlimited.failed && unlimited.writer.values["percent"] == 0);
    assert(unlimited.writer.values["previous.percent"] == 400);
    puts("PSP native transaction: version, previous identity, override clearing, network rejection and admission passed");
}
'''
with tempfile.TemporaryDirectory(prefix='emucap-psp-transaction-') as directory:
    path = Path(directory)
    (path / 'EmucapPacing.h').write_text((args.source.parents[2] / 'EmucapPacing.h').read_text())
    (path / 'probe.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++17', '-pthread', '-fsanitize=address,undefined',
                    str(path / 'probe.cpp'), '-o', str(path / 'probe')], check=True)
    subprocess.run([str(path / 'probe')], check=True)
