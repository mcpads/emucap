use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;

use emucap::live::tools::StepUnit;

/// Numeric input accepted as a JSON integer or a hexadecimal string. Supported
/// forms include 8471, "0x2117", "0X2117", "$2117", "8471", and "0x80_420b".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Num(pub(crate) u64);
impl Num {
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

fn parse_num_str(s: &str) -> Result<u64, String> {
    // 공통 파서는 lib(emucap::numparse)에 둔다 — MCP와 CLI가 같은 규칙으로 0x/$ 16진을 받게 한다(#45).
    emucap::numparse::parse_num_str(s)
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Num;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an integer or a hexadecimal string prefixed with '0x' or '$'")
            }
            fn visit_u64<E>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Num, E> {
                u64::try_from(v)
                    .map(Num)
                    .map_err(|_| E::custom("negative values are not allowed"))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Num, E> {
                if v >= 0.0 && v.fract() == 0.0 {
                    Ok(Num(v as u64))
                } else {
                    Err(E::custom("only integer values are allowed"))
                }
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Num, E> {
                parse_num_str(s).map(Num).map_err(E::custom)
            }
            fn visit_string<E: serde::de::Error>(self, s: String) -> Result<Num, E> {
                self.visit_str(&s)
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for Num {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Num".into()
    }
    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A non-negative decimal integer or a hexadecimal string prefixed with '0x' or '$', for example 8471, \"0x2117\", or \"$2117\".",
            "anyOf": [ { "type": "integer", "minimum": 0 }, { "type": "string" } ]
        })
    }
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyArgs {}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BootstrapDetail {
    /// Include the full system routing catalog. The default response contains
    /// only system identifiers and a catalog revision.
    Systems,
    /// Include build and runtime installation paths.
    Installation,
    /// Include the bounded inventory of live managed generations in this
    /// server's direct listener range and their explicit reattach availability.
    Runtimes,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapArgs {
    /// Optional detail sections. Omit for the compact task-entry response.
    #[serde(default)]
    pub(crate) include: Vec<BootstrapDetail>,
}

impl BootstrapArgs {
    pub(crate) fn includes(&self, detail: BootstrapDetail) -> bool {
        self.include.contains(&detail)
    }
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatusArgs {
    /// Revision returned by a previous full status. When it still matches, the
    /// response omits the unchanged capability catalog but retains live state.
    #[serde(default)]
    pub(crate) known_capability_revision: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowLimitsArgs {
    /// Narrow the advertised event-record limit.
    #[serde(default)]
    pub(crate) max_events: Option<u64>,
    /// Narrow the advertised event-byte limit.
    #[serde(default)]
    pub(crate) max_bytes: Option<u64>,
    /// Narrow the advertised adapter deadline; publication follows guest closure.
    #[serde(default)]
    pub(crate) max_host_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordWindowOriginArgs {
    NextFrameBoundary,
    ResetRelease,
    StateLoad,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowInitialStateArgs {
    /// Opaque producer-managed ID returned in save_state.snapshot_receipt. Caller-authored paths,
    /// digests, and boundary claims are deliberately not accepted.
    pub(crate) snapshot_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowStopArgs {
    /// Selected stoppable event class.
    pub(crate) event_class: String,
    /// Positive occurrence within the frame bound.
    pub(crate) occurrence: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowStartArgs {
    /// Selected exact event class that begins observation at its first occurrence.
    pub(crate) event_class: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RecordWindowFilterTermArgs {
    /// Match an unsigned payload field in the half-open range [start, start + length).
    U64Range {
        /// Advertised payload field path.
        path: String,
        /// Inclusive range start.
        start: Num,
        /// Positive range length.
        length: Num,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowEventFilterArgs {
    /// Selected event class whose declared scope is narrowed.
    pub(crate) event_class: String,
    /// All terms must match. Check the live capability for supported fields.
    pub(crate) terms: Vec<RecordWindowFilterTermArgs>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordWindowEventScopeArgs {
    Transaction,
    Observation,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowEventArmingArgs {
    /// Selected event class whose advertised warmup scope is overridden.
    pub(crate) event_class: String,
    /// Producer-advertised event emission scope.
    pub(crate) scope: RecordWindowEventScopeArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowArgs {
    /// Existing absolute directory; Core creates one capture child.
    pub(crate) output_root: String,
    /// Guest frames to capture; returns frozen.
    pub(crate) frames: u64,
    /// Guest frames before selected observation-only classes are armed.
    #[serde(default)]
    pub(crate) warmup_frames: u64,
    /// Advertised event-class IDs; omit for defaults.
    #[serde(default)]
    pub(crate) event_classes: Vec<String>,
    /// Optional advertised per-class payload filters. Omit to capture the full selected classes.
    #[serde(default)]
    pub(crate) event_filters: Vec<RecordWindowEventFilterArgs>,
    /// Optional per-class warmup emission scopes. Requires positive warmup_frames without
    /// start_on. Use only combinations advertised by the live recording capability. Omit to
    /// preserve the producer default.
    #[serde(default)]
    pub(crate) event_arming_overrides: Vec<RecordWindowEventArmingArgs>,
    /// Advertised origin; omit for next_frame_boundary.
    #[serde(default)]
    pub(crate) origin: Option<RecordWindowOriginArgs>,
    /// Absolute dense movie path; check live capability.
    #[serde(default)]
    pub(crate) input_path: Option<String>,
    /// Producer-managed frozen state to load before a state_load recording origin. The live
    /// capability may require an explicit dense input movie, including an all-empty movie for
    /// neutral input.
    #[serde(default)]
    pub(crate) initial_state: Option<RecordWindowInitialStateArgs>,
    /// Selected stoppable event occurrence.
    #[serde(default)]
    pub(crate) stop_on: Option<RecordWindowStopArgs>,
    /// Begin observation at the first occurrence of this selected startable event.
    #[serde(default)]
    pub(crate) start_on: Option<RecordWindowStartArgs>,
    /// Callback-safe memory ranges captured at the exact event-aligned start.
    #[serde(default)]
    pub(crate) initial_snapshots: Vec<RecordWindowInitialSnapshotArgs>,
    /// Bounded frozen-terminal reads from `status.memory_regions`.
    #[serde(default)]
    pub(crate) terminal_snapshots: Vec<RecordWindowTerminalSnapshotArgs>,
    /// Advertised frozen-terminal state profile to preserve as one hashed JSON member.
    #[serde(default)]
    pub(crate) terminal_state_profile: Option<String>,
    /// Require a live repeatable-recording profile. Refuses before staging,
    /// reset, input, or guest advance when the current runtime does not
    /// advertise it.
    #[serde(default)]
    pub(crate) require_repeatable: bool,
    /// Optional narrower advertised limits.
    #[serde(default)]
    pub(crate) limits: Option<RecordWindowLimitsArgs>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowTerminalSnapshotArgs {
    /// Bundle member label: ASCII letters and digits separated by single hyphens.
    pub(crate) label: String,
    /// Live memory type.
    pub(crate) memory_type: String,
    /// Region-relative offset.
    pub(crate) address: Num,
    /// Byte length.
    pub(crate) length: Num,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordWindowInitialSnapshotArgs {
    /// Bundle member label: ASCII letters and digits separated by single hyphens.
    pub(crate) label: String,
    /// Callback-safe memory type advertised by the recording capability.
    pub(crate) memory_type: String,
    /// Region-relative offset.
    pub(crate) address: Num,
    /// Byte length.
    pub(crate) length: Num,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadMemoryArgs {
    /// Memory-type identifier. Read valid names from `status.memory_types`; see
    /// the corresponding adapter README for system-specific meanings.
    pub(crate) memory_type: String,
    /// Start address as an offset within the selected memory type.
    pub(crate) address: Num,
    /// Number of bytes to read.
    pub(crate) length: Num,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProbeArgs {
    /// Base savestate path. The adapter loads it, advances by `frame`, and reads
    /// the target memory.
    pub(crate) state: String,
    /// Frames to advance from the base state. A synchronous upper bound prevents
    /// an oversized probe from holding the link until its deadline.
    #[serde(deserialize_with = "deser_frame_count")]
    pub(crate) frame: u64,
    /// Memory type to read, using the same identifier as read_memory.
    pub(crate) memory_type: String,
    /// Start address to read.
    pub(crate) address: Num,
    /// Number of bytes to read.
    pub(crate) length: Num,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisassembleArgs {
    /// Start address for disassembly. Check `status.methods` and the adapter
    /// README for CPU, ISA, and support details. Returns [{addr,text,bytes}].
    pub(crate) address: Num,
    /// Number of instructions to decode. Default: 8; maximum: 256.
    #[serde(default = "default_disas_count")]
    pub(crate) count: u64,
    /// Write JSON results to this path and return a summary. Omit for inline results.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
    /// Target CPU for a multi-core backend, for example NDS `arm9` or `arm7`.
    /// Omit for the backend's default core.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
    /// Instruction-set mode when the backend supports an override, for example
    /// NDS `arm`, `thumb`, or `auto`. Omit for automatic selection.
    #[serde(default)]
    pub(crate) mode: Option<String>,
}
fn default_disas_count() -> u64 {
    8
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallStackArgs {
    /// Target CPU for a multi-core backend, for example NDS `arm9` or `arm7`.
    /// Omit for the backend's default core.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteMemoryFileArgs {
    /// Absolute path to a raw binary file read by Control MCP, not the adapter.
    pub(crate) path: String,
    /// Byte offset at which to start reading the file. Default: 0.
    #[serde(default)]
    pub(crate) offset: Option<Num>,
    /// Number of bytes to read from the file. Must not exceed
    /// `status.contracts.constraints["memory.write.max_bytes"]`.
    pub(crate) length: Num,
    /// Optional SHA-256 precondition. A mismatch rejects the request before
    /// emulator memory is changed.
    #[serde(default)]
    pub(crate) sha256: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteMemoryArgs {
    pub(crate) memory_type: String,
    /// Start address within the selected memory type.
    pub(crate) address: Num,
    /// Bytes to write as a hexadecimal string. Specify exactly one of `hex` and
    /// `input_file`.
    #[serde(default)]
    pub(crate) hex: Option<String>,
    /// Raw binary file slice. Specify exactly one of `input_file` and `hex`.
    #[serde(default)]
    pub(crate) input_file: Option<WriteMemoryFileArgs>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FindPatternArgs {
    /// Memory type to search, using the same identifier as read_memory. Prefer a
    /// linear region such as work RAM, VRAM, CRAM, or text RAM.
    pub(crate) memory_type: String,
    /// Even-length hexadecimal byte pattern. Example: "4901" means 0x49, 0x01.
    pub(crate) hex: String,
    /// Search start offset. Default: 0.
    #[serde(default)]
    pub(crate) start: Option<Num>,
    /// Search length in bytes. Omit to search to the end of the region. Backend
    /// limits may return `truncated:true`; move `start` forward to scan another
    /// chunk. Use `output_path` for a large match list.
    #[serde(default)]
    pub(crate) length: Option<Num>,
    /// Maximum number of matches to return. Default: 256.
    #[serde(default = "default_max_matches")]
    pub(crate) max_matches: u64,
    /// Return only matches at offsets divisible by this value. Default: 1.
    #[serde(default = "one")]
    pub(crate) align: u64,
    /// Write JSON results to this path and return a summary. Omit for inline results.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
}
fn default_max_matches() -> u64 {
    256
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// Persistent values for axes advertised by the current input capability. Axis names and
    /// integer bounds are runtime-defined; omitted axes are neutral because set_input replaces the
    /// complete controller state.
    #[serde(default)]
    pub(crate) axes: BTreeMap<String, i64>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PressArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// Frames for which to hold the buttons. A synchronous bound prevents an
    /// abandoned request from leaving input ownership active past the deadline.
    #[serde(deserialize_with = "deser_input_frames")]
    pub(crate) frames: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MovePointerArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// Signed horizontal relative movement. Read backend bounds from status contracts.
    pub(crate) dx: i64,
    /// Signed vertical relative movement. Read backend bounds from status contracts.
    pub(crate) dy: i64,
    /// Exact guest frames to advance after queueing the relative delta. Increase this when guest
    /// software polls its pointer less often. Default: 1.
    #[serde(default = "one", deserialize_with = "deser_positive_input_frames")]
    pub(crate) frames: u64,
}

fn left_pointer_button() -> String {
    "left".into()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClickPointerArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// Pointer button: left, right, or middle. Default: left.
    #[serde(default = "left_pointer_button")]
    pub(crate) button: String,
    /// Exact frames for which the button is held. Default: 2.
    #[serde(default = "two", deserialize_with = "deser_positive_input_frames")]
    pub(crate) press_frames: u64,
    /// Additional frames to advance after release. Default: 0.
    #[serde(default, deserialize_with = "deser_frame_count")]
    pub(crate) after_frames: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DragPointerArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// Pointer button: left, right, or middle. Default: left.
    #[serde(default = "left_pointer_button")]
    pub(crate) button: String,
    /// Signed horizontal relative movement. Read backend bounds from status contracts.
    pub(crate) dx: i64,
    /// Signed vertical relative movement. Read backend bounds from status contracts.
    pub(crate) dy: i64,
    /// Exact frames to keep the button held after queueing movement. Increase this when guest
    /// software polls its pointer less often. Default: 1.
    #[serde(default = "one", deserialize_with = "deser_positive_input_frames")]
    pub(crate) move_frames: u64,
    /// Additional frames to advance after the release edge. Default: 0.
    #[serde(default, deserialize_with = "deser_frame_count")]
    pub(crate) after_frames: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TouchArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// Lower touchscreen X coordinate (0-255). Required unless releasing.
    #[serde(default)]
    pub(crate) x: Option<u64>,
    /// Lower touchscreen Y coordinate (0-191). Required unless releasing.
    #[serde(default)]
    pub(crate) y: Option<u64>,
    /// Optional tap duration. When present, return after the frames have advanced
    /// and touch has been released. When absent, hold until the next touch call.
    #[serde(default)]
    pub(crate) frames: Option<u64>,
    /// Release touch input and ignore x and y.
    #[serde(default)]
    pub(crate) release: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HoldTouchArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// Lower touchscreen X coordinate (0-255).
    pub(crate) x: u64,
    /// Lower touchscreen Y coordinate (0-191).
    pub(crate) y: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseTouchArgs {
    #[serde(default)]
    pub(crate) port: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PulseTouchArgs {
    #[serde(default)]
    pub(crate) port: u64,
    /// Lower touchscreen X coordinate (0-255).
    pub(crate) x: u64,
    /// Lower touchscreen Y coordinate (0-191).
    pub(crate) y: u64,
    /// Frames for which to hold the touch before releasing it. This operation
    /// leaves guest execution running.
    #[serde(deserialize_with = "deser_input_frames")]
    pub(crate) frames: u64,
}

fn two() -> u64 {
    2
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TapArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// Frames for which to press the buttons. Default: 2, a short tap below
    /// typical auto-repeat timing.
    #[serde(default = "two", deserialize_with = "deser_input_frames")]
    pub(crate) press_frames: u64,
    /// Frames to advance after release. Default: 0. A positive value composes
    /// input and observation in one call while preserving frozen state.
    #[serde(default, deserialize_with = "deser_frame_count")]
    pub(crate) after_frames: u64,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HoldUntilArgs {
    #[serde(default)]
    pub(crate) port: u64,
    pub(crate) buttons: Vec<String>,
    /// Memory type to watch, using the same identifier as read_memory.
    pub(crate) memory_type: String,
    /// Start address to watch.
    pub(crate) address: Num,
    pub(crate) length: Num,
    /// Maximum frames to wait for a change. Default: 300. Input remains held
    /// during the operation, so the synchronous input bound applies.
    #[serde(default = "three_hundred", deserialize_with = "deser_input_frames")]
    pub(crate) max_frames: u64,
}
fn three_hundred() -> u64 {
    300
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathArgs {
    pub(crate) path: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveStateArgs {
    pub(crate) path: String,
    /// Preserve a producer-managed receipt for a later state-backed recording window.
    /// This requires an absolute path and an advertised state_load capability.
    #[serde(default)]
    pub(crate) preserve_for_recording: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeMediaArgs {
    /// Device identifier from status.media_devices, for example `flop1`.
    pub(crate) device: String,
    /// Absolute path to an existing media image. Mutually exclusive with eject=true.
    #[serde(default)]
    pub(crate) path: Option<String>,
    /// Eject the current image. Mutually exclusive with path.
    #[serde(default)]
    pub(crate) eject: bool,
    /// Optional SHA-1 precondition checked by the adapter immediately before mounting.
    #[serde(default)]
    pub(crate) expected_sha1: Option<String>,
}

/// Common bound for one synchronous frame or instruction advance. At 60 fps, 5,000 frames take
/// about 83 seconds, leaving cleanup time before the 300-second deferred deadline. Bridges that
/// step one instruction at a time also receive a finite work bound. Callers split longer travel
/// into terminally acknowledged requests.
pub(crate) const MAX_SYNC_ADVANCE_COUNT: u64 = emucap::live::temporal::MAX_SYNC_ADVANCE_COUNT;

/// Reject a supplied frame or instruction count above the common synchronous bound. Serde defaults
/// bypass this function when a field is absent; every default remains within the bound.
fn deser_frame_count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n > MAX_SYNC_ADVANCE_COUNT {
        return Err(serde::de::Error::custom(format!(
            "frame/instruction count {n} exceeds the synchronous limit {MAX_SYNC_ADVANCE_COUNT}; split the request and verify each terminal response"
        )));
    }
    Ok(n)
}

/// Frame bound for deferred input operations such as press, tap, and hold. An oversized request
/// could outlive the link deadline and leave a button override active after the MCP has given up.
/// Keep this equal to the common advance limit so a composed operation is not rejected only when it
/// reaches its internal step.
pub(crate) const MAX_INPUT_HOLD_FRAMES: u64 = MAX_SYNC_ADVANCE_COUNT;

fn deser_input_frames<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n > MAX_INPUT_HOLD_FRAMES {
        return Err(serde::de::Error::custom(format!(
            "input hold duration {n} exceeds the limit {MAX_INPUT_HOLD_FRAMES}; an oversized request could outlive the link deadline and leave input ownership active"
        )));
    }
    Ok(n)
}

fn deser_positive_input_frames<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n == 0 {
        return Err(serde::de::Error::custom(
            "pointer input frame duration must be at least 1",
        ));
    }
    if n > MAX_INPUT_HOLD_FRAMES {
        return Err(serde::de::Error::custom(format!(
            "input hold duration {n} exceeds the limit {MAX_INPUT_HOLD_FRAMES}; an oversized request could outlive the link deadline and leave input ownership active"
        )));
    }
    Ok(n)
}

fn one() -> u64 {
    1
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StepArgs {
    /// Number of units to advance. The legacy input name `frames` is accepted
    /// for compatibility.
    #[serde(
        default = "one",
        alias = "frames",
        deserialize_with = "deser_frame_count"
    )]
    pub(crate) count: u64,
    /// Advance unit. Default: frames. Check
    /// `status.contracts.constraints["execution.step.units"]`.
    #[serde(default)]
    pub(crate) unit: StepUnit,
    /// Target CPU for a multi-core backend, for example NDS `arm9` or `arm7`.
    /// Omit for the default core. Single-core backends ignore this field.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoutedOperationArgs {
    /// Call `describe` first, then use one operation returned by that response.
    pub(crate) operation: String,
    /// Exact object matching the selected operation's returned schema.
    #[serde(default)]
    pub(crate) arguments: Option<serde_json::Map<String, serde_json::Value>>,
    /// Capability revision returned by this drawer's describe response.
    #[serde(default)]
    pub(crate) known_capability_revision: Option<String>,
}
/// CPU selection for pause and resume.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CpuArgs {
    /// Target CPU for a multi-core backend. NDS accepts `arm9` (default), `arm7`,
    /// and `both` for resume. Single-core backends ignore this field.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BreakpointArgs {
    /// Breakpoint kind. Read allowed values and their address units,
    /// memory_type use, and snapshot support from `status.breakpoint_kinds`.
    pub(crate) kind: String,
    /// Memory type, following read_memory identifiers. Check whether the selected
    /// kind uses it in `status.breakpoint_kinds`.
    pub(crate) memory_type: String,
    /// Inclusive range start. Read its unit from `status.breakpoint_kinds`.
    pub(crate) start: Num,
    /// Inclusive range end. Use the same value as start for a single address.
    pub(crate) end: Num,
    /// Freeze on hit so status becomes frozen.
    #[serde(default)]
    pub(crate) pause_on_hit: bool,
    #[serde(default)]
    pub(crate) auto_savestate: bool,
    /// Inclusive causing-PC lower bound; supply pc_max too. For dma, this filters
    /// the VRAM-address range.
    #[serde(default)]
    pub(crate) pc_min: Option<Num>,
    /// Upper PC filter supplied with pc_min. For kind=dma, upper VRAM address.
    #[serde(default)]
    pub(crate) pc_max: Option<Num>,
    /// Optional value filter for read/write breakpoints. Break only when the
    /// access value matches `(value & value_mask)`. For kind=dma this field is
    /// the destination filter (for example VRAM, OAM, or CGRAM register targets).
    #[serde(default)]
    pub(crate) value: Option<Num>,
    /// Value-filter mask. Defaults to all bits.
    #[serde(default)]
    pub(crate) value_mask: Option<Num>,
    /// Compared value width in bytes, from 1 to 4. Default: 1.
    #[serde(default)]
    pub(crate) value_len: Option<Num>,
    /// Hit-time atomic memory slices as "memory_type:address:length". Check
    /// support in status.breakpoint_kinds.
    #[serde(default)]
    pub(crate) snapshot: Vec<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WatchRegisterArgs {
    /// Register to watch, using a get_state `cpu.*` name such as sp, pc, k, a, x,
    /// y, ps, d, or dbr. Default: sp.
    #[serde(default = "sp_reg")]
    pub(crate) register: String,
    /// Inclusive lower bound. Break on the instruction that moves the register below it.
    #[serde(default)]
    pub(crate) min: Num,
    /// Inclusive upper bound. Break on the instruction that moves the register
    /// above it. Example: min=0 and max=0x1fff for an SP range of $0000-$1FFF.
    #[serde(default = "u16_max")]
    pub(crate) max: Num,
    /// Freeze on the out-of-range instruction.
    #[serde(default = "default_true")]
    pub(crate) pause_on_hit: bool,
    /// Instruction budget before automatic disarm. Per-instruction register
    /// checks cannot run indefinitely without starving the emulator thread. On
    /// expiry, the adapter emits watch_disarmed. Omit for the adapter default.
    #[serde(default)]
    pub(crate) max_instructions: Option<u64>,
}
fn sp_reg() -> String {
    "sp".into()
}
fn u16_max() -> Num {
    Num(0xffff)
}
fn default_true() -> bool {
    true
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetTraceArgs {
    /// Enable or disable execution tracing.
    pub(crate) enabled: bool,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetTraceArgs {
    /// Number of recent instructions to return. Maximum: 256.
    #[serde(default = "two_fifty_six")]
    pub(crate) count: u64,
    /// Write JSON results to this path and return a summary. Omit for inline results.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
}
fn two_fifty_six() -> u64 {
    256
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PollEventsArgs {
    /// Write JSON results to this path and return a summary. Omit for inline results.
    #[serde(default)]
    pub(crate) output_path: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BreakOnResetArgs {
    /// Enable or disable reset-handler detection.
    pub(crate) enabled: bool,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClearBpArgs {
    pub(crate) id: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScreenshotArgs {
    /// Also save the PNG to this path when specified.
    #[serde(default)]
    pub(crate) save_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StateArgs {
    /// State groups to return, such as cpu, ppu, dmaController, spc,
    /// internalRegisters, or memoryManager. Omit for all groups.
    #[serde(default)]
    pub(crate) groups: Vec<String>,
    /// Target CPU for a multi-core backend, for example NDS `arm9` or `arm7`.
    /// Omit for the default core. Single-core backends ignore this field.
    #[serde(default)]
    pub(crate) cpu: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveTileArgs {
    /// NBG layer number from 0 through 3. Rotational RBG layers are unsupported.
    pub(crate) nbg: u32,
    /// Screen X coordinate in pixels.
    pub(crate) x: u32,
    /// Screen Y coordinate in pixels.
    pub(crate) y: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetLayerEnableArgs {
    /// Case-insensitive layer names to enable; all omitted layers are disabled.
    /// Read names from the response's `layer_names`, or query by omitting both
    /// layers and mask. Omit to leave state unchanged.
    #[serde(default)]
    pub(crate) layers: Option<Vec<String>>,
    /// Raw enable bitmask, with bit 0 representing the first layer. Prefer names
    /// because bit meanings are system-specific. Zero disables all layers.
    #[serde(default)]
    pub(crate) mask: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegressionRunArgs {
    /// Regression suite directory containing case subdirectories.
    pub(crate) suite_dir: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnalysisOperation {
    Describe,
    RegressionRun,
    VerifyDeterminism,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalysisArgs {
    /// Use describe first to load the operation-specific schemas, then select
    /// regression_run or verify_determinism.
    pub(crate) operation: AnalysisOperation,
    /// Operation-specific arguments returned by operation=describe. Omit for
    /// describe.
    #[serde(default)]
    pub(crate) arguments: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchPlanArgs {
    /// ROM, disc, or disk path to launch. Omit when unknown and use the returned
    /// system identifiers and required_user_input fields.
    #[serde(default)]
    pub(crate) content_path: Option<String>,
    /// Explicit system identifier. Provide it for ambiguous media such as CUE,
    /// CHD, or BIN. Use bootstrap(include=["systems"]) for the full catalog.
    #[serde(default)]
    pub(crate) system: Option<String>,
    /// PC-98 execution backend. Omit for the MAME debugger backend; select
    /// np2kai explicitly for HDI compatibility when launch_plan recommends it.
    #[serde(default)]
    pub(crate) pc98_backend: Option<Pc98BackendArgs>,
    /// Exact server-produced review value for files named indirectly by a
    /// descriptor. Echo the value returned by launch_plan; do not construct it.
    #[serde(default)]
    pub(crate) indirect_media_approval: Option<emucap::content_identity::IndirectMediaApproval>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchArgs {
    /// Required ROM, disc, or disk path. launch performs execution, not planning.
    pub(crate) content_path: String,
    /// Optional second disc or disk for titles that must boot with two media
    /// mounted concurrently. Currently used by PC-98 two-drive titles; other
    /// adapters ignore it.
    #[serde(default)]
    pub(crate) content_path2: Option<String>,
    /// Explicit system identifier. Provide it when media type is ambiguous.
    #[serde(default)]
    pub(crate) system: Option<String>,
    /// PC-98 execution backend. Omit for MAME. NP2kai is an explicit
    /// compatibility choice and is never selected automatically.
    #[serde(default)]
    pub(crate) pc98_backend: Option<Pc98BackendArgs>,
    /// Exact server-produced review value for files named indirectly by a
    /// descriptor. Obtain it from launch_plan before launching composite media.
    #[serde(default)]
    pub(crate) indirect_media_approval: Option<emucap::content_identity::IndirectMediaApproval>,
    /// Optional connection name exposed as `status.emulator_identity.name`.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Show an isolated native HITL window when supported. Default: false;
    /// unsupported adapters ignore this field.
    #[serde(default)]
    pub(crate) display: Option<bool>,
    /// Enable audio output independently of display. Currently supported by
    /// Mednafen and PC-98 systems. Default: false. Unsupported adapters reject
    /// true rather than silently ignoring it.
    #[serde(default)]
    pub(crate) sound: Option<bool>,
    /// Optional emulated sound board for PC-98. This is independent of host
    /// audio output: set `sound:true` as well to hear the selected board.
    /// Omit this field to keep the C-bus sound slot empty.
    #[serde(default)]
    pub(crate) pc98_sound_board: Option<Pc98SoundBoardArgs>,
    /// Return only after the adapter is connected at a guest-time-closed frozen
    /// entry boundary. Default: false. Unsupported adapters reject true before
    /// spawning a process.
    #[serde(default)]
    pub(crate) start_frozen: bool,
    /// Optional execution conditions selected before launch. `repeatable`
    /// implies start_frozen and is currently available only for SNES on the
    /// compatible Mesen host.
    #[serde(default)]
    pub(crate) execution_profile: Option<LaunchExecutionProfileArgs>,
    /// Explicitly replace a live process recorded by the current capsule. The
    /// launcher terminates only a generation whose PID and process-start identity
    /// both match; unverifiable ownership is rejected.
    #[serde(default)]
    pub(crate) replace: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Pc98SoundBoardArgs {
    Pc9801_26,
    Pc9801_86,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Pc98BackendArgs {
    Mame,
    Np2kai,
}

impl Pc98SoundBoardArgs {
    pub(crate) fn mame_slot(self) -> &'static str {
        match self {
            Self::Pc9801_26 => "pc9801_26",
            Self::Pc9801_86 => "pc9801_86",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LaunchExecutionProfileArgs {
    Repeatable,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopArgs {
    /// Exact current launch generation to terminate. Read it from
    /// `status.runtime_instance.launch_id`; a stale or different generation is
    /// rejected without signalling any process.
    pub(crate) launch_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReattachArgs {
    /// Exact managed launch generation to take over after its former control
    /// lease has been returned. Read candidates through runtime discovery.
    pub(crate) launch_id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VerifyDeterminismArgs {
    /// Regression case directory containing case.json and movie or savestate
    /// data. Uses reproduction and ROM fields; ignores the predicate.
    pub(crate) case_dir: String,
    /// Observation type: auto (default), memory, screenshot, or state.
    #[serde(default)]
    pub(crate) observe: Option<String>,
    /// Memory type for observe=memory, using read_memory identifiers.
    #[serde(default)]
    pub(crate) memory_type: Option<String>,
    /// Start address for observe=memory.
    #[serde(default)]
    pub(crate) address: Option<Num>,
    /// Byte length for observe=memory.
    #[serde(default)]
    pub(crate) length: Option<Num>,
    /// Replay count from 2 through 5. Default: 2.
    #[serde(default)]
    pub(crate) replays: Option<u32>,
}
