use super::*;

impl<T: WsTransport> PpssppBridge<T> {
    /// Native output readback preserves an existing CPU/debugger halt. Running capture
    /// still uses the native GPU boundary; the bridge never resumes a halted guest.
    pub(super) fn screenshot(&mut self) -> BridgeResult<Value> {
        let result =
            self.ws
                .call_with_timeout("emucap.screenshot", json!({}), SCREENSHOT_READ_TIMEOUT)?;
        let uri = result.get("uri").and_then(Value::as_str).ok_or_else(|| {
            BridgeError::Emulator("emucap.screenshot: reply had no uri field".into())
        })?;
        let b64 = uri.strip_prefix("data:image/png;base64,").ok_or_else(|| {
            let head: String = uri.chars().take(32).collect();
            BridgeError::Emulator(format!(
                "emucap.screenshot: unexpected uri prefix: {head:?}"
            ))
        })?;
        let width = result
            .get("width")
            .and_then(Value::as_u64)
            .unwrap_or(PSP_SCREEN_WIDTH);
        let height = result
            .get("height")
            .and_then(Value::as_u64)
            .unwrap_or(PSP_SCREEN_HEIGHT);
        Ok(json!({
            "png_base64": b64,
            "format": "png",
            "width": width,
            "height": height,
        }))
    }

    /// `input.buttons.send {buttons: {<psp_name>: bool}}` — a full replacement of the held button
    /// set (every emucap button name is written explicitly true/false), matching the NDS bridge's
    /// full-mask `set_input` semantics: an empty `buttons` list releases everything currently held
    /// (the `tools.rs` combo/tap helpers rely on that to clean up after a press). PPSSPP's own
    /// `input.buttons.send` is itself a *partial* update — unlisted keys are left alone — so
    /// writing every uniform button explicitly is what turns it into a full "set", not a merge.
    /// Both `__CtrlUpdateButtons` and `req.Respond()` are synchronous (no frame wait), so this
    /// works regardless of whether the CPU is running or halted.
    pub(super) fn set_input(&mut self, params: &Value) -> BridgeResult<Value> {
        require_input_port_zero(params)?;
        let requested = button_list(params.get("buttons"))?;
        let mut buttons_obj = serde_json::Map::new();
        for name in PSP_INPUT_BUTTONS {
            let psp_name = psp_button_name(name).expect("PSP_INPUT_BUTTONS names all map");
            buttons_obj.insert(psp_name.into(), json!(requested.iter().any(|r| r == name)));
        }
        self.ws.call(
            "input.buttons.send",
            json!({ "buttons": Value::Object(buttons_obj) }),
        )?;
        self.held_buttons = Some(requested.clone());
        Ok(json!({ "buttons": requested }))
    }

    /// `input.buttons.press {button, duration}` for one requested button. PPSSPP has no multi-button
    /// timed-press command (`ButtonsPress` takes exactly one `button`), so accepting a combo would
    /// silently serialize it and violate the common same-frame-window contract. Multi-button lists
    /// are rejected before any WS mutation until a fork-owned combo command exists. PPSSPP acks the
    /// press asynchronously under the *same* event name once `duration` frames have elapsed and
    /// the button auto-releases (`WebSocketInputState::Broadcast`), so this rides `call_ticketed`
    /// (the ack name matches the request name): each press carries a unique `ticket` PPSSPP echoes
    /// on its release ack, so a late ack from a *different* press can't be misattributed to this one.
    /// That auto-release never fires while the core is halted (frames only advance while running).
    /// A halted core is rejected up front (mirroring the NDS bridge's frozen-timed-input guard), but
    /// a breakpoint can still halt the CPU *mid-press* — after the pre-check passes — stranding the
    /// ack; on that timeout this releases every input (so the button doesn't stay held) and returns
    /// a clear error, and the stale ack is ignored by ticket correlation. `frames` is capped at
    /// `MAX_PRESS_FRAMES` for the same reason `read_memory` caps `length`: an uncapped hold blocks
    /// the call past the bridge's own WS read timeout.
    pub(super) fn press_buttons(&mut self, params: &Value) -> BridgeResult<Value> {
        require_input_port_zero(params)?;
        let requested = button_list(params.get("buttons"))?;
        if requested.is_empty() {
            return Err(BridgeError::BadParams(
                "press_buttons requires at least one button".into(),
            ));
        }
        if requested.len() > 1 {
            return Err(BridgeError::BadParams(
                "PPSSPP press_buttons currently supports exactly one button: stock PPSSPP has no atomic timed-combo command, and sequential presses would violate the simultaneous frame-window contract. Use set_input for an explicit persistent combo and set_input([]) to release it, or send single-button pulses."
                    .into(),
            ));
        }
        let frames = optional_num(params, "frames")?.unwrap_or(1).max(1);
        if frames > MAX_PRESS_FRAMES {
            return Err(BridgeError::BadParams(format!(
                "press_buttons frames {frames} exceeds the {MAX_PRESS_FRAMES} cap (~4s at 60fps) \
                 — a longer hold risks PPSSPP's ack arriving after the bridge's own 8s WS read \
                 timeout, which then misattributes the late reply to an unrelated request; hold \
                 longer with repeated press_buttons calls or set_input instead."
            )));
        }
        if self.cpu_is_stepping()? {
            return Err(BridgeError::BadParams(
                "press_buttons needs a running emulator — while the CPU is halted for the \
                 debugger, frames never advance, so PPSSPP's timed press never auto-releases \
                 (resume first, or use set_input to hold instead)."
                    .into(),
            ));
        }
        let name = &requested[0];
        let psp_name = psp_button_name(name).expect("validated by button_list");
        let ticket = self.mint_ticket();
        // Ticket-correlated so a stale ack from a *previous* press (one whose release was
        // stranded when a breakpoint halted the CPU mid-press, then fired late on resume) can't
        // satisfy this call — it carries the earlier ticket and is queued/ignored.
        if let Err(err) = self.ws.call_ticketed(
            "input.buttons.press",
            json!({ "button": psp_name, "duration": frames }),
            &ticket,
        ) {
            // The press ack only fires after `duration` frames elapse. If a breakpoint
            // halts the CPU before then, frames stop, the auto-release never runs, and this
            // read times out with the button still held. Release everything best-effort so
            // the button doesn't stay stuck, then surface a clear error. The late ack (this
            // ticket) is ignored by every later ticketed read.
            let _ = self.release_all_inputs();
            if is_timeout_error(&err) {
                return Err(BridgeError::Emulator(format!(
                    "press_buttons({name}) timed out waiting for the timed release — the \
                     CPU likely halted (breakpoint) mid-press so frames stopped advancing. \
                     Inputs were released; resume and retry, or hold with set_input instead."
                )));
            }
            return Err(err);
        }
        Ok(json!({ "buttons": requested, "frames": frames }))
    }

    /// Mint a unique ticket string for a correlated request (see `press_buttons`).
    pub(super) fn mint_ticket(&mut self) -> String {
        let n = self.next_ticket;
        self.next_ticket += 1;
        format!("emucap-{n}")
    }

    /// Release every held button — `input.buttons.send` with all buttons explicitly false. Used to
    /// recover after a timed press is interrupted mid-flight (the button was pressed but its timed
    /// release never ran). Synchronous on the PPSSPP side, so it works even while the CPU is halted.
    pub(super) fn release_all_inputs(&mut self) -> BridgeResult<Value> {
        let mut buttons_obj = serde_json::Map::new();
        for name in PSP_INPUT_BUTTONS {
            let psp_name = psp_button_name(name).expect("PSP_INPUT_BUTTONS names all map");
            buttons_obj.insert(psp_name.into(), json!(false));
        }
        self.ws.call(
            "input.buttons.send",
            json!({ "buttons": Value::Object(buttons_obj) }),
        )?;
        self.held_buttons = Some(Vec::new());
        Ok(json!({ "released": true }))
    }

    /// Write a PPSSPP savestate to `path` via the emucap fork's `savestate.save` (stock PPSSPP
    /// exposes no WebSocket savestate command — `SaveState::Save`/`Load` are async and normally
    /// only serviced while the EmuThread is stepping; the fork's handler breaks the CPU into
    /// stepping if running, waits for the save to complete, then restores the prior run state).
    pub(super) fn save_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = required_str(params, "path")?.to_string();
        // Dedicated read budget above the fork's 15s save wait (see SAVESTATE_READ_TIMEOUT) — the
        // default 8s would time out mid-save and desync the channel.
        self.ws.call_with_timeout(
            "savestate.save",
            json!({ "path": path.clone() }),
            SAVESTATE_READ_TIMEOUT,
        )?;
        Ok(json!({ "path": path, "status": "completed" }))
    }

    /// Restore a PPSSPP savestate from `path` via the emucap fork's `savestate.load`.
    pub(super) fn load_state(&mut self, params: &Value) -> BridgeResult<Value> {
        let path = required_str(params, "path")?.to_string();
        // Same dedicated budget as save_state — the fork's load handler shares the 15s wait.
        let result = self.ws.call_with_timeout(
            "savestate.load",
            json!({ "path": path.clone() }),
            SAVESTATE_READ_TIMEOUT,
        )?;
        if result.get("state").and_then(Value::as_str) != Some("frozen") {
            return Err(BridgeError::BadState(
                "native savestate.load did not confirm a frozen machine".into(),
            ));
        }
        Ok(json!({ "path": path, "status": "completed", "state": "frozen" }))
    }

    /// Power-cycle via `game.reset`. The emucap fork's headless build performs a *real* reboot on
    /// its emu-thread run loop (`PSP_Shutdown` + re-init from the same content) — stock headless
    /// dropped `REQUEST_GAME_RESET` on the floor (`System_PostUIMessage` was an empty stub), so
    /// `game.reset` was a silent no-op. The fork's WS handler now blocks its ack until the reboot
    /// actually completes, so this call gets a synchronous "rebooted" acknowledgement; it just needs
    /// a read budget above that reboot wait (a commercial title reloads modules for a few seconds),
    /// the same way `save_state` outlasts the fork's save wait. Bridge-side breakpoints are left
    /// tracked as-is: PPSSPP's breakpoints live in a WS-side global registry (`g_breakpoints`) that
    /// survives the reset, so `self.bps` stays in sync (the next run-loop `g_breakpoints.Frame()`
    /// re-arms them against the freshly booted code). Mirroring the initial launch, the headless
    /// reboot leaves the CPU halted at the fresh boot entry (state `frozen`) so a caller can re-arm
    /// breakpoints before running — resume to run it. `post_reset_pc` reports that boot-entry pc as
    /// verifiable evidence the machine really rebooted (a no-op would leave the pc progressing deep
    /// in-game).
    ///
    /// Only the headless fork blocks the ack — a display:true GUI session does not (its
    /// `game.reset` posts a UI message and returns immediately, and the async reboot keeps the core
    /// running rather than halting). So the ack alone does not prove the reboot completed: this
    /// confirms the halted state (`wait_for_reset_halt`) before claiming `completed`. When the core
    /// is halted the reboot is confirmed and `post_reset_pc` is the boot entry; when it is still
    /// running the reboot is in flight, so `reset` reports `status:"rebooting"` and withholds a
    /// `post_reset_pc` — the live pc there is a stale in-game value, not reset evidence. `status` is
    /// the single source of truth for whether the core halted (`completed`) or is still rebooting.
    pub(super) fn reset(&mut self, _params: &Value) -> BridgeResult<Value> {
        self.ws
            .call_with_timeout("game.reset", json!({}), RESET_READ_TIMEOUT)?;
        if !self.wait_for_reset_halt()? {
            // The core never halted: the reboot is running asynchronously (display:true GUI). Report
            // that instead of a false "completed" with the still-in-game pc — get_state /
            // poll_events track the reboot as it settles.
            return Ok(json!({ "status": "rebooting" }));
        }
        let mut result = json!({ "status": "completed" });
        // Halted at the fresh boot entry — surface that pc as verifiable reset evidence.
        if let Ok(state) = self.fetch_cpu_state() {
            if let Some(pc) = state.get("cpu.pc").and_then(Value::as_u64) {
                result["post_reset_pc"] = json!(pc);
            }
        }
        Ok(result)
    }

    /// Poll `cpu.status.stepping` after a `game.reset` ack to confirm the reboot left the CPU halted
    /// at the fresh boot entry. Returns `true` on the first poll that reads halted (the headless
    /// path, which acks already-halted, returns immediately with no sleep); returns `false` if the
    /// core is still running after `RESET_HALT_POLLS` tries (a display:true GUI session, whose async
    /// reboot keeps the core running). See `reset`.
    pub(super) fn wait_for_reset_halt(&mut self) -> BridgeResult<bool> {
        for attempt in 0..RESET_HALT_POLLS {
            if self.cpu_is_stepping()? {
                return Ok(true);
            }
            if attempt + 1 < RESET_HALT_POLLS {
                std::thread::sleep(RESET_HALT_POLL_INTERVAL);
            }
        }
        Ok(false)
    }

    /// `game.status` for the running disc's id/version/title plus a locally computed sha1 of the
    /// content image at `EMUCAP_CONTENT` — PPSSPP's WS API never exposes a content path or hash
    /// itself (`game.status`'s `game` object is just `{id, version, title}`, see
    /// `GameSubscriber.cpp`). Shape mirrors the NDS bridge's `get_rom_info`
    /// (`name`/`path`/`sha1`/`size`/`media_type`); `sha1` is what `emucap-mcp`'s
    /// `normalize_rom_sha1` promotes to the uniform `rom_sha1` field.
    pub(super) fn get_rom_info(&mut self) -> BridgeResult<Value> {
        let content = self.content.clone().ok_or_else(|| {
            BridgeError::BadParams("EMUCAP_CONTENT is not set for get_rom_info".into())
        })?;
        if !content.is_file() {
            return Err(BridgeError::BadParams(format!(
                "content image not found: {}",
                content.display()
            )));
        }
        let game_status = self.ws.call("game.status", json!({}))?;
        Ok(json!({
            "system": "psp",
            "adapter": "ppsspp-rust-ws",
            "name": content.file_name().and_then(|s| s.to_str()).unwrap_or(""),
            "path": absolute_display(&content),
            "sha1": sha1_file(&content)?,
            "size": content.metadata()?.len(),
            "media_type": content.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase(),
            "game": game_status.get("game").cloned().unwrap_or(Value::Null),
        }))
    }
}
