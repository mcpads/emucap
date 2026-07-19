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

#include "Common/CommonTypes.h"

struct GCPadStatus;

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

// Called by the PowerPC breakpoint handler after it has confirmed a real hit.
// The adapter filters this against breakpoints registered through emucap.
void NotifyBreakpointHit(u32 address);
}  // namespace EmuCap
