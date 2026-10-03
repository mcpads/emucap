//! Bounded frozen observation and native host pacing.
use serde_json::{json, Value};

use super::{
    debuggable_name, memory_type, required_num, BridgeResult, OpenMsxBridge, OpenMsxBridgeError,
    OpenMsxControl, MAX_MEMORY_TRANSFER,
};
use crate::live::memory_batch::{self, BatchRange, MemoryBatchCapability, MemoryWindow};
use crate::live::pacing::{self, ExecutionSpeedCapability, SpeedRequest};

const HALT_KIND: &str = "global_pause_and_debug_break";

/// Native policy revision and single-command pacing transactions. openMSX settings are Tcl
/// variables; every write, including hotkeys and console commands, passes the trace. Each proc runs
/// as one command on the openMSX main thread, so no other setting change interleaves with it.
const PACING_TCL: &str = r#"namespace eval ::emucap {
    variable policy_revision 0

    proc policy_bump {args} {
        variable policy_revision
        incr policy_revision
        return
    }

    foreach name {throttle speed fastforward fullspeedwhenloading} {
        trace add variable ::$name write ::emucap::policy_bump
    }

    proc policy {} {
        variable policy_revision
        return [join [list $policy_revision [set ::throttle] [set ::speed] \
            [set ::fastforward] [set ::fullspeedwhenloading]] |]
    }

    proc boundary {} {
        return [join [list [machine] [machine_info time] [::emucap::frame_seq] \
            [set ::pause] [debug breaked]] |]
    }

    proc observe_policy {} {
        return [join [list [boundary] [policy]] ";"]
    }

    proc apply_policy {throttle speed} {
        set before [boundary]
        set previous [policy]
        set values [list [set ::throttle] [set ::speed] [set ::fastforward] \
            [set ::fullspeedwhenloading]]
        if {[catch {
            set ::fastforward false
            set ::fullspeedwhenloading false
            set ::speed $speed
            set ::throttle $throttle
            if {![expr {
                [set ::speed] == $speed &&
                bool([set ::throttle]) == bool($throttle) &&
                !bool([set ::fastforward]) && !bool([set ::fullspeedwhenloading])
            }]} {
                error "requested pacing settings were not applied"
            }
        } message]} {
            lassign $values t s f l
            set restored [expr {![catch {
                set ::speed $s
                set ::throttle $t
                set ::fastforward $f
                set ::fullspeedwhenloading $l
                if {![expr {
                    [set ::speed] == $s &&
                    bool([set ::throttle]) == bool($t) &&
                    bool([set ::fastforward]) == bool($f) &&
                    bool([set ::fullspeedwhenloading]) == bool($l)
                }]} {
                    error "previous pacing settings were not restored"
                }
            }]}]
            # These endpoints bound the whole apply/restore command, not only the
            # partially applied policy interval. Restoring settings never rewinds time.
            set after [boundary]
            error "emucap-policy-apply-failed restored=$restored: $message; clock_domains=openmsx_emutime_seconds,emucap_frame_seq; before=$before; after=$after; previous=$previous; final=[policy]"
        }
        return [join [list $before $previous [policy] [boundary]] ";"]
    }

    proc restore_policy {expected throttle speed fastforward loading} {
        variable policy_revision
        if {$policy_revision != $expected} {
            error "emucap-policy-conflict [policy]"
        }
        set ::speed $speed
        set ::throttle $throttle
        set ::fastforward $fastforward
        set ::fullspeedwhenloading $loading
        return [join [list [policy] [boundary]] ";"]
    }
}"#;

/// One observed native policy tuple.
#[derive(Debug, Clone, PartialEq)]
struct NativePolicy {
    revision: u64,
    throttle: bool,
    speed: String,
    fastforward: bool,
    loading: bool,
}

impl NativePolicy {
    fn parse(raw: &str) -> BridgeResult<Self> {
        let invalid =
            || OpenMsxBridgeError::Protocol(format!("invalid native pacing readback {raw:?}"));
        let parts: Vec<_> = raw.trim().split('|').collect();
        let [revision, throttle, speed, fastforward, loading] = parts[..] else {
            return Err(invalid());
        };
        let boolean = |value: &str| match value {
            "true" | "1" | "on" => Ok(true),
            "false" | "0" | "off" => Ok(false),
            _ => Err(invalid()),
        };
        let numeric = speed.parse::<f64>().map_err(|_| invalid())?;
        if !numeric.is_finite() || numeric <= 0.0 {
            return Err(invalid());
        }
        Ok(Self {
            revision: revision.parse().map_err(|_| invalid())?,
            throttle: boolean(throttle)?,
            speed: speed.to_string(),
            fastforward: boolean(fastforward)?,
            loading: boolean(loading)?,
        })
    }

    fn same_settings(&self, other: &Self) -> bool {
        self.throttle == other.throttle
            && self.speed.parse::<f64>().ok() == other.speed.parse::<f64>().ok()
            && self.fastforward == other.fastforward
            && self.loading == other.loading
    }

    /// Effective common policy. Fast-forward and loading overrides replace the limited target, so
    /// they are reported as custom rather than as the underlying `speed` setting.
    fn public(&self, capability: &ExecutionSpeedCapability) -> Value {
        let limited = self
            .speed
            .parse::<f64>()
            .ok()
            .and_then(pacing::centi_percent)
            .filter(|centi| capability.percent.contains(*centi));
        let (mode, percent) = match (self.fastforward || self.loading, self.throttle, limited) {
            (false, false, _) => ("unlimited", Value::Null),
            (false, true, Some(centi)) => ("limited", pacing::percent_value(centi)),
            _ => ("custom", Value::Null),
        };
        json!({
            "mode": mode, "percent": percent, "source": capability.source,
            "policy_revision": self.revision.to_string(),
            "host_constraints": capability.host_constraints,
            "diagnostics": {"throttle": self.throttle, "speed": self.speed,
                "fastforward": self.fastforward, "fullspeedwhenloading": self.loading},
        })
    }
}

/// `machine|emutime|frame|pause|breaked` from one native command.
#[derive(Debug, Clone, PartialEq)]
struct NativeBoundary {
    machine: String,
    time: String,
    frame: u64,
    frozen: bool,
}

impl NativeBoundary {
    fn parse(raw: &str) -> BridgeResult<Self> {
        let invalid = || OpenMsxBridgeError::Protocol(format!("invalid native boundary {raw:?}"));
        let parts: Vec<_> = raw.trim().split('|').collect();
        let [machine, time, frame, pause, breaked] = parts[..] else {
            return Err(invalid());
        };
        if machine.is_empty()
            || !time
                .parse::<f64>()
                .is_ok_and(|seconds| seconds.is_finite() && seconds >= 0.0)
        {
            return Err(invalid());
        }
        Ok(Self {
            machine: machine.into(),
            time: time.into(),
            frame: frame.parse().map_err(|_| invalid())?,
            frozen: matches!(pause, "true" | "1" | "on") && breaked == "1",
        })
    }
}

impl<C: OpenMsxControl> OpenMsxBridge<C> {
    pub(super) fn initialize_pacing(&mut self) -> BridgeResult<()> {
        self.control.command(PACING_TCL).map(|_| ())
    }

    pub(super) fn memory_batch_capability(&self) -> MemoryBatchCapability {
        MemoryBatchCapability {
            max_ranges: memory_batch::CORE_MAX_RANGES,
            max_range_bytes: MAX_MEMORY_TRANSFER,
            max_total_bytes: memory_batch::CORE_MAX_TOTAL_BYTES,
            consistency: memory_batch::CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec![HALT_KIND.into()],
            // openMSX debuggables are peeks: no I/O, mapper or watchpoint side effects.
            windows: self
                .region_sizes
                .iter()
                .map(|(name, size)| MemoryWindow {
                    memory_type: (*name).into(),
                    address: 0,
                    length: *size,
                })
                .collect(),
        }
    }

    pub(super) fn execution_speed_capability(&self) -> ExecutionSpeedCapability {
        ExecutionSpeedCapability {
            modes: vec!["limited".into(), "unlimited".into()],
            percent: pacing::PercentDomain {
                min: Some(0.01),
                max: Some(10000.0),
                quantum: Some(0.01),
                values: None,
            },
            states: vec!["running".into(), "frozen".into()],
            scope: pacing::SCOPE_HOST_PACING.into(),
            source: "native".into(),
            control_service_ms: 50,
            host_constraints: vec![],
        }
    }

    /// Opaque tokens for one native stop. The bridge sequence covers resets and debugger writes
    /// that leave the emulated time unchanged; any write through `memory` may remap slots.
    fn boundary_value(&self, boundary: &NativeBoundary) -> Value {
        let epoch = format!(
            "{}@{}#{}",
            boundary.machine, boundary.time, self.boundary_seq
        );
        json!({
            "runtime_generation": self.runtime_generation(),
            "stop_epoch": epoch,
            "memory_mapping_epoch": epoch,
            "clocks": [{"domain": "emucap.vdp_frame_boundary", "value": boundary.frame}],
        })
    }

    pub(super) fn runtime_generation(&self) -> String {
        self.launch_id
            .clone()
            .unwrap_or_else(|| format!("unmanaged-pid:{}", self.control.child_pid()))
    }

    pub(super) fn read_memory(&mut self, params: &Value) -> BridgeResult<Value> {
        self.require_frozen("read_memory")?;
        let memory_type = memory_type(params)?;
        let address = required_num(params, "address")?;
        let length = required_num(params, "length")?;
        if length > MAX_MEMORY_TRANSFER {
            return Err(OpenMsxBridgeError::BadParams(format!(
                "read_memory length {length} exceeds {MAX_MEMORY_TRANSFER}"
            )));
        }
        self.validate_range(memory_type, address, length)?;
        let debuggable = debuggable_name(memory_type);
        let command =
            format!("binary encode hex [debug read_block {debuggable} {address} {length}]");
        let encoded = self.control.command(&command)?;
        let bytes = hex::decode(encoded.trim()).map_err(|error| {
            OpenMsxBridgeError::Protocol(format!("openMSX returned invalid memory hex: {error}"))
        })?;
        if bytes.len() as u64 != length {
            return Err(OpenMsxBridgeError::Protocol(format!(
                "openMSX returned {} bytes for a {length}-byte read",
                bytes.len()
            )));
        }
        Ok(json!({
            "memory_type": memory_type,
            "address": address,
            "length": length,
            "hex": hex::encode(bytes),
        }))
    }

    fn batch_ranges(params: &Value) -> BridgeResult<Vec<BatchRange>> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| OpenMsxBridgeError::BadParams("ranges must be an array".into()))?;
        ranges
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: memory_type(range)?.into(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect()
    }

    pub(super) fn read_memory_batch(&mut self, params: &Value) -> BridgeResult<Value> {
        let ranges = Self::batch_ranges(params)?;
        self.memory_batch_capability()
            .admit(&ranges)
            .map_err(OpenMsxBridgeError::BadParams)?;
        self.require_frozen("read_memory_batch")?;
        // One Tcl command never yields guest execution. The stop conjunction and the native
        // boundary are checked before and after the reads inside that same command.
        let mut command = String::from(
            "set emucap_batch [list [::emucap::boundary]]; \
             if {![set pause] || ![debug breaked]} {error {batch requires frozen state}};",
        );
        for range in &ranges {
            command.push_str(&format!(
                " lappend emucap_batch [binary encode hex [debug read_block {} {} {}]];",
                debuggable_name(&range.memory_type),
                range.address,
                range.length
            ));
        }
        command.push_str(" lappend emucap_batch [::emucap::boundary]; join $emucap_batch {;}");
        let encoded = self.control.command(&command)?;
        let lines: Vec<_> = encoded.trim().split(';').collect();
        let decoded = (|| {
            if lines.len() != ranges.len() + 2 || lines.first() != lines.last() {
                return None;
            }
            let boundary = NativeBoundary::parse(lines[0]).ok().filter(|b| b.frozen)?;
            let mut reads = Vec::with_capacity(ranges.len());
            for (index, range) in ranges.iter().enumerate() {
                let bytes = hex::decode(lines[index + 1]).ok()?;
                if bytes.len() as u64 != range.length {
                    return None;
                }
                reads.push(json!({"index": index, "memory_type": range.memory_type,
                    "address": range.address, "length": range.length,
                    "hex": hex::encode(bytes)}));
            }
            Some(json!({
                "state": "frozen", "consistency": memory_batch::CONSISTENCY_FROZEN_BOUNDARY,
                "boundary": self.boundary_value(&boundary),
                "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
                "reads": reads,
                "frame": boundary.frame, "frame_domain": "emucap.vdp_frame_boundary",
            }))
        })();
        match decoded {
            Some(value) => Ok(value),
            None => self.fail_debugger("unverified native memory batch boundary or payload".into()),
        }
    }

    fn native_policy(&mut self) -> BridgeResult<NativePolicy> {
        NativePolicy::parse(&self.control.command("::emucap::policy")?)
    }

    pub(super) fn speed_readback(&mut self) -> BridgeResult<Value> {
        let capability = self.execution_speed_capability();
        Ok(self.native_policy()?.public(&capability))
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> BridgeResult<Value> {
        let capability = self.execution_speed_capability();
        let mode =
            match params.get("mode") {
                None => None,
                Some(value) => Some(value.as_str().ok_or_else(|| {
                    OpenMsxBridgeError::BadParams("mode must be a string".into())
                })?),
            };
        let percent =
            match params.get("percent") {
                None => None,
                Some(value) => Some(value.as_f64().ok_or_else(|| {
                    OpenMsxBridgeError::BadParams("percent must be a number".into())
                })?),
            };
        let request =
            pacing::parse_request(mode, percent).map_err(OpenMsxBridgeError::BadParams)?;
        capability
            .admit(request)
            .map_err(OpenMsxBridgeError::BadParams)?;
        let (throttle, speed) = match request {
            SpeedRequest::Query => return self.speed_readback(),
            SpeedRequest::Unlimited => (false, None),
            SpeedRequest::Limited { centi_percent } => (
                true,
                Some(format!(
                    "{}.{:02}",
                    centi_percent / 100,
                    centi_percent % 100
                )),
            ),
        };
        self.drain_debug_events()?;
        self.refresh_execution_state()?;
        // This pre-command observation survives response loss. It is not the
        // transaction's previous value, which only the apply reply can establish.
        let observed = self.control.command("::emucap::observe_policy")?;
        let (boundary, policy) = observed
            .split_once(';')
            .ok_or_else(|| OpenMsxBridgeError::Protocol("invalid pacing observation".into()))?;
        NativeBoundary::parse(boundary)?;
        let observed_policy = NativePolicy::parse(policy)?;
        let context = format!(
            "clock_domains=openmsx_emutime_seconds,emucap_frame_seq; pre_command_observation={observed:?}; requested={params}; guest progress may have occurred"
        );
        // Unlimited keeps the current `speed` setting; only the throttle governs it.
        let speed_arg = match speed {
            Some(speed) => speed,
            None => observed_policy.speed,
        };
        let command = format!("::emucap::apply_policy {throttle} {speed_arg}");
        let rejection_prefix = format!("openMSX rejected `{command}`: ");
        let raw = match self.control.command(&command) {
            Ok(raw) => raw,
            // The native command restored the previous settings itself when it reports so.
            Err(OpenMsxBridgeError::Emulator(message))
                if message.strip_prefix(&rejection_prefix).is_some_and(|native| {
                    native.starts_with("emucap-policy-apply-failed restored=1:")
                }) =>
            {
                return Err(OpenMsxBridgeError::Emulator(format!(
                    "execution_speed failed_restored: {message}"
                )))
            }
            Err(error) => {
                return self.fail_debugger(format!("execution_speed unverified: {error}; {context}; native_result=unavailable; final_boundary=unknown"))
            }
        };
        let context = format!("{context}; native_transaction={raw:?}");
        let lines: Vec<_> = raw.trim().split(';').collect();
        let [before, previous, applied, after] = lines[..] else {
            return self.fail_debugger(format!(
                "execution_speed unverified: invalid native pacing transaction reply; {context}"
            ));
        };
        // The command may already have changed native settings. A malformed receipt
        // cannot establish either success or a safe revision-guarded rollback.
        let decoded = (|| {
            Ok::<_, OpenMsxBridgeError>((
                NativeBoundary::parse(before)?,
                NativeBoundary::parse(after)?,
                NativePolicy::parse(previous)?,
                NativePolicy::parse(applied)?,
            ))
        })();
        let (before, after, previous, applied) = match decoded {
            Ok(receipt) => receipt,
            Err(error) => {
                return self
                    .fail_debugger(format!("execution_speed unverified: {error}; {context}"))
            }
        };
        let public = applied.public(&capability);
        let confirmed = capability.verify_change(
            request,
            &json!({"status":"completed", "state":"running", "previous": previous.public(&capability),
                "execution_speed": public}),
        );
        // A frozen stop must be the same native stop after the transaction.
        let boundary_kept = !before.frozen || before == after;
        if confirmed.is_ok() && boundary_kept {
            if let Err(error) = self.refresh_execution_state() {
                return self
                    .fail_debugger(format!("execution_speed unverified: {error}; {context}"));
            }
            return Ok(json!({
                "status": "completed",
                "state": if self.frozen { "frozen" } else { "running" },
                "previous": previous.public(&capability),
                "execution_speed": public,
                "frame": after.frame,
            }));
        }
        let reason = match confirmed {
            Err(error) => error,
            Ok(()) => "frozen boundary changed during the pacing transaction".into(),
        };
        self.restore_policy(&previous, &applied, &format!("{reason}; {context}"))
    }

    /// Restore only while the applied revision is still current; a newer external change is kept.
    fn restore_policy(
        &mut self,
        previous: &NativePolicy,
        applied: &NativePolicy,
        reason: &str,
    ) -> BridgeResult<Value> {
        let command = format!(
            "::emucap::restore_policy {} {} {} {} {}",
            applied.revision,
            previous.throttle,
            previous.speed,
            previous.fastforward,
            previous.loading
        );
        let rejection_prefix = format!("openMSX rejected `{command}`: ");
        match self.control.command(&command).map(|raw| {
            let (policy, boundary) = raw.split_once(';').ok_or_else(||
                OpenMsxBridgeError::Protocol(format!("invalid pacing restoration result {raw:?}")))?;
            Ok::<_, OpenMsxBridgeError>((NativePolicy::parse(policy)?, NativeBoundary::parse(boundary)?))
        }) {
            Ok(Ok((restored, boundary))) if restored.same_settings(previous) => Err(OpenMsxBridgeError::Emulator(
                format!("execution_speed failed_restored: {reason}; restored_policy={restored:?}; restore_end={boundary:?}; previous policy verified"),
            )),
            Err(OpenMsxBridgeError::Emulator(message))
                if message.strip_prefix(&rejection_prefix).is_some_and(|native| {
                    native.starts_with("emucap-policy-conflict ")
                }) => {
                Err(OpenMsxBridgeError::BadState(format!(
                    "execution_speed conflict: {reason}; an external policy change was kept: {message}"
                )))
            }
            outcome => self.fail_debugger(format!(
                "execution_speed unverified: {reason}; previous policy could not be restored: {outcome:?}"
            )),
        }
    }
}
