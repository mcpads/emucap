//! Frozen RAM batches and guest-time pacing through the maintained xemu QMP extension.
use super::*;
use crate::live::memory_batch::{
    BatchRange, MemoryBatchCapability, MemoryWindow, CONSISTENCY_FROZEN_BOUNDARY,
};
use crate::live::pacing::{self, ExecutionSpeedCapability, PercentDomain, SpeedRequest};

const PACING_MIN_PERCENT: u64 = 1;
const PACING_MAX_PERCENT: u64 = 1_000;
const BATCH_MAX_RANGES: u64 = 64;
const BATCH_MAX_BYTES: u64 = 65_536;
const INSTRUCTION_CLOCK_THROUGHPUT: &str = "fixed_instruction_clock_host_throughput";

/// Requests that neither change guest memory nor move the stop.
pub(super) const OBSERVATION_METHODS: &[&str] = &[
    "hello",
    "status",
    "get_rom_info",
    "get_state",
    "read_memory",
    "read_memory_batch",
    "find_pattern",
    "dump_memory",
    "list_breakpoints",
    "poll_events",
    "disassemble",
    "call_stack",
    "screenshot",
    "save_state",
    "execution_speed",
];

/// Native pacing readback; `percent` 0 is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct XemuPacing {
    pub(super) percent: u64,
    pub(super) revision: u64,
}

impl XemuPacing {
    pub(super) fn from_reply(reply: &Value) -> Option<Self> {
        Some(Self {
            percent: reply.get("percent")?.as_u64()?,
            revision: reply.get("revision")?.as_u64()?,
        })
    }

    pub(super) fn from_status(extension: &Value) -> Option<Self> {
        Some(Self {
            percent: extension.get("pacing-percent")?.as_u64()?,
            revision: extension.get("pacing-revision")?.as_u64()?,
        })
    }

    pub(super) fn public(&self) -> Value {
        let (mode, percent) = if self.percent == 0 {
            ("unlimited", Value::Null)
        } else if (PACING_MIN_PERCENT..=PACING_MAX_PERCENT).contains(&self.percent) {
            ("limited", json!(self.percent))
        } else {
            ("custom", Value::Null)
        };
        json!({
            "mode": mode,
            "percent": percent,
            "source": "native",
            "policy_revision": self.revision.to_string(),
            "host_constraints": [INSTRUCTION_CLOCK_THROUGHPUT],
            "diagnostics": {"native_percent": self.percent},
        })
    }
}

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        MemoryBatchCapability {
            max_ranges: BATCH_MAX_RANGES,
            max_range_bytes: BATCH_MAX_BYTES,
            max_total_bytes: BATCH_MAX_BYTES,
            consistency: CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec!["qemu_vm_stop_nv2a_writers_parked".into()],
            windows: vec![MemoryWindow {
                memory_type: "main".into(),
                address: 0,
                length: XBOX_RAM_SIZE,
            }],
        }
    }

    pub(super) fn execution_speed_capability() -> ExecutionSpeedCapability {
        ExecutionSpeedCapability {
            modes: vec!["limited".into(), "unlimited".into()],
            percent: PercentDomain {
                min: Some(PACING_MIN_PERCENT as f64),
                max: Some(PACING_MAX_PERCENT as f64),
                quantum: Some(1.0),
                values: None,
            },
            states: vec!["running".into(), "frozen".into()],
            scope: pacing::SCOPE_HOST_PACING.into(),
            source: "native".into(),
            control_service_ms: 20,
            host_constraints: vec![INSTRUCTION_CLOCK_THROUGHPUT.into()],
        }
    }

    fn native_pacing(&mut self, arguments: Option<Value>) -> XemuResult<(XemuPacing, u64, Value)> {
        let reply = self.qmp.execute("xemu-emucap-pacing", arguments)?;
        self.verify_clock_profile(&reply)?;
        let pacing = XemuPacing::from_reply(&reply).ok_or_else(|| {
            XemuBridgeError::Emulator(format!("invalid xemu-emucap-pacing readback: {reply}"))
        })?;
        let frame = reply
            .get("frame-boundary")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                XemuBridgeError::Emulator("pacing reply omitted frame-boundary".into())
            })?;
        Ok((pacing, frame, reply))
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> XemuResult<Value> {
        let capability = Self::execution_speed_capability();
        let mode = match params.get("mode") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| XemuBridgeError::BadParams("mode must be a string".into()))?,
            ),
        };
        let percent =
            match params.get("percent") {
                None => None,
                Some(value) => Some(value.as_f64().ok_or_else(|| {
                    XemuBridgeError::BadParams("percent must be a number".into())
                })?),
            };
        let request = pacing::parse_request(mode, percent).map_err(XemuBridgeError::BadParams)?;
        capability
            .admit(request)
            .map_err(XemuBridgeError::BadParams)?;
        let set = match request {
            SpeedRequest::Query => return Ok(self.native_pacing(None)?.0.public()),
            SpeedRequest::Unlimited => json!({"unlimited": true}),
            SpeedRequest::Limited { centi_percent } => json!({"percent": centi_percent / 100}),
        };
        let (observed, _, probe) = self.native_pacing(None)?;
        if probe.get("transaction-version").and_then(Value::as_u64) != Some(1) {
            return Err(XemuBridgeError::Unsupported(
                "native pacing transaction version 1 is required".into(),
            ));
        }
        let mut native_result = None;
        let outcome: XemuResult<Value> = (|| {
            let transaction = self.qmp.execute("xemu-emucap-pacing", Some(set))?;
            native_result = Some(transaction.clone());
            self.verify_clock_profile(&transaction)?;
            let invalid = || XemuBridgeError::Emulator("invalid native pacing transaction".into());
            let after = XemuPacing::from_reply(&transaction).ok_or_else(invalid)?;
            let frame = transaction
                .get("frame-boundary")
                .and_then(Value::as_u64)
                .ok_or_else(invalid)?;
            let before = XemuPacing {
                percent: transaction
                    .get("previous-percent")
                    .and_then(Value::as_u64)
                    .ok_or_else(invalid)?,
                revision: transaction
                    .get("previous-revision")
                    .and_then(Value::as_u64)
                    .ok_or_else(invalid)?,
            };
            let start_frame = transaction
                .get("start-frame-boundary")
                .and_then(Value::as_u64)
                .ok_or_else(invalid)?;
            if transaction
                .get("transaction-version")
                .and_then(Value::as_u64)
                != Some(1)
                || before.percent > PACING_MAX_PERCENT
                || after.percent > PACING_MAX_PERCENT
                || after.revision
                    != before
                        .revision
                        .wrapping_add(u64::from(before.percent != after.percent))
                || frame < start_frame
            {
                return Err(invalid());
            }
            let state = if self.is_running()? {
                "running"
            } else {
                "frozen"
            };
            let reply = json!({
                "status": "completed", "state": state,
                "previous": before.public(), "execution_speed": after.public(), "frame": frame,
                "clock_domain": "xbox.nv2a_vblank",
            });
            capability
                .verify_change(request, &reply)
                .map_err(XemuBridgeError::Emulator)?;
            Ok(reply)
        })();
        outcome.map_err(|error| {
            self.control_unverified = true;
            XemuBridgeError::Emulator(format!("execution_speed unverified: {error}; pre-command observation: {}; native_result={native_result:?}; guest progress may have occurred", observed.public()))
        })
    }

    pub(super) fn read_memory_batch(&mut self, params: &Value) -> XemuResult<Value> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| XemuBridgeError::BadParams("ranges must be an array".into()))?
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: range
                        .get("memory_type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            XemuBridgeError::BadParams("memory_type is required".into())
                        })?
                        .to_string(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect::<XemuResult<Vec<_>>>()?;
        Self::memory_batch_capability()
            .admit(&ranges)
            .map_err(XemuBridgeError::BadParams)?;
        self.drain_gdb_stops(true)?;
        if self.is_running()? {
            return Err(XemuBridgeError::BadState(
                "read_memory_batch requires a frozen VM; call pause first".into(),
            ));
        }
        let native: Vec<Value> = ranges
            .iter()
            .map(|range| json!({"address": range.address, "length": range.length}))
            .collect();
        let reply = self.qmp.execute(
            "xemu-emucap-read-memory-batch",
            Some(json!({"ranges": native})),
        )?;
        let (Some(frame), Some(virtual_ns), Some(hex)) = (
            reply.get("frame-boundary").and_then(Value::as_u64),
            reply.get("virtual-ns").and_then(Value::as_u64),
            reply.get("reads").and_then(Value::as_array),
        ) else {
            return Err(XemuBridgeError::Emulator(format!(
                "invalid xemu-emucap-read-memory-batch reply: {reply}"
            )));
        };
        if hex.len() != ranges.len() {
            return Err(XemuBridgeError::Emulator(format!(
                "xemu returned {} reads for {} ranges",
                hex.len(),
                ranges.len()
            )));
        }
        let mut reads = Vec::with_capacity(ranges.len());
        for (index, (range, hex)) in ranges.iter().zip(hex).enumerate() {
            let hex = hex.as_str().filter(|hex| {
                hex.len() as u64 == range.length * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit())
            });
            let hex = hex.ok_or_else(|| {
                XemuBridgeError::Emulator(format!("xemu read {index} has the wrong length"))
            })?;
            reads.push(json!({
                "index": index, "memory_type": range.memory_type, "address": range.address,
                "length": range.length, "hex": hex,
            }));
        }
        let epoch = format!("f{frame}#s{}", self.boundary_seq);
        Ok(json!({
            "state": "frozen",
            "consistency": CONSISTENCY_FROZEN_BOUNDARY,
            "boundary": {
                "runtime_generation": self.env.launch_id.clone()
                    .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id())),
                "stop_epoch": epoch,
                "memory_mapping_epoch": epoch,
                "clocks": [
                    {"domain": "xbox.nv2a_vblank", "value": frame},
                    {"domain": "qemu.virtual_ns", "value": virtual_ns},
                ],
            },
            "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
            "reads": reads,
        }))
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
