// Native emucap adapter hooks for Dolphin (GameCube and Wii).
// The embedded service exposes the supported Dolphin control surface without a GDB relay.
//
// The Control MCP accepts the adapter connection on its current listening port. This service is
// the TCP client and answers NDJSON {"v":1,"id","method","params"} requests with
// {"id","ok","result|error"}.
//
// When EMUCAP_PORT is set, Core::Init calls Start() to create the service thread. The hello
// response carries EMUCAP_SESSION_TOKEN, EMUCAP_NAME, and EMUCAP_CONTENT when present.
#pragma once

#include <cstddef>

#include "Common/CommonTypes.h"

struct GCPadStatus;

namespace WiimoteEmu
{
struct DesiredWiimoteState;
}

namespace Core
{
class System;
}

namespace EmuCap
{
// Start the adapter thread once when EMUCAP_PORT is set; otherwise do nothing.
void Start(Core::System& system);

// Stop and join the adapter thread during core shutdown.
void Stop();

// Replace a polled GCPad status when set_input owns that controller. Leave native input unchanged
// when no override is engaged.
void ApplyInputOverride(int pad_num, GCPadStatus* status);

// Replace only an emulated Wii Remote's core-button field when set_input owns it. Motion, IR, and
// extension state remain owned by Dolphin.
void ApplyWiimoteInputOverride(int wiimote_num, WiimoteEmu::DesiredWiimoteState* state);

// Called by the PowerPC breakpoint handler after it has confirmed a real adapter-owned hit.
void NotifyBreakpointHit(Core::System& system, u32 address, u64 breakpoint_id);

// Called by Dolphin's native memory-check path before a matching write commits or after a read has
// fetched its value, but before the PowerPC instruction completes.
void NotifyMemoryBreakpointHit(Core::System& system, u64 value, u32 address, bool write,
                               size_t size, u32 pc, u64 breakpoint_id);

// Called after Dolphin has presented a non-duplicate frame and returned the CPU to stepping mode.
void NotifyFrameStepComplete();

// Called by the adapter-owned reset-button release event on the CPU thread. The token prevents a
// user, movie, or other native reset tap from completing an emucap request.
void NotifyResetTapComplete(Core::System& system, u64 token);
}  // namespace EmuCap
