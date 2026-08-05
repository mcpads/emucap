use serde::Serialize;

use super::manifest::{BundleManifest, Manifest, TriggerKind};

#[derive(Debug, Serialize)]
pub struct Summary {
    pub platform: String,
    pub rom_sha1: String,
    pub adapter: String,
    pub trigger_kind: String,
    pub trigger_frame: u64,
    pub slice_count: usize,
    pub frames: Vec<u64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "bundle_kind", rename_all = "snake_case")]
pub enum BundleSummary {
    Legacy { summary: Summary },
    Recording { summary: RecordingSummary },
}

#[derive(Debug, Serialize)]
pub struct RecordingSummary {
    pub format_version: u32,
    pub capture_id: String,
    pub system: String,
    pub content_sha1: Option<String>,
    pub content_sha256: Option<String>,
    pub adapter: String,
    pub frame_start: u64,
    pub frame_end: u64,
    pub event_count: u64,
    pub integrity: String,
}

pub fn summarize(m: &Manifest) -> Summary {
    Summary {
        platform: m.platform.clone(),
        rom_sha1: m.rom.sha1.clone(),
        adapter: m.adapter.name.clone(),
        trigger_kind: match m.trigger.kind {
            TriggerKind::Retrospective => "retrospective",
            TriggerKind::RecordWindow => "record_window",
        }
        .to_string(),
        trigger_frame: m.trigger.at_frame,
        slice_count: m.slices.len(),
        frames: m.slices.iter().map(|s| s.frame).collect(),
    }
}

pub fn render_json(s: &Summary) -> String {
    serde_json::to_string_pretty(s).expect("요약 직렬화")
}

pub fn render_table(s: &Summary) -> String {
    format!(
        "platform     : {}\n\
         rom sha1     : {}\n\
         adapter      : {}\n\
         trigger      : {} @ frame {}\n\
         slices       : {} (frames {:?})\n",
        s.platform, s.rom_sha1, s.adapter, s.trigger_kind, s.trigger_frame, s.slice_count, s.frames
    )
}

pub fn summarize_bundle(manifest: &BundleManifest) -> BundleSummary {
    match manifest {
        BundleManifest::Legacy(manifest) => BundleSummary::Legacy {
            summary: summarize(manifest),
        },
        BundleManifest::Recording(manifest) => BundleSummary::Recording {
            summary: RecordingSummary {
                format_version: manifest.format_version,
                capture_id: manifest.capture_id.clone(),
                system: manifest.runtime.system.clone(),
                content_sha1: manifest.runtime.content.sha1.clone(),
                content_sha256: manifest.runtime.content.sha256.clone(),
                adapter: manifest.runtime.adapter_id.clone(),
                frame_start: manifest.scope.f_start,
                frame_end: manifest.scope.f_end,
                event_count: manifest.counters.events,
                integrity: serde_json::to_value(manifest.terminal.integrity)
                    .expect("integrity serialization")
                    .as_str()
                    .expect("integrity serializes as string")
                    .to_string(),
            },
        },
    }
}

pub fn render_bundle_json(summary: &BundleSummary) -> String {
    serde_json::to_string_pretty(summary).expect("bundle summary serialization")
}

pub fn render_bundle_table(summary: &BundleSummary) -> String {
    match summary {
        BundleSummary::Legacy { summary } => render_table(summary),
        BundleSummary::Recording { summary } => format!(
            "format       : {}\n\
             capture      : {}\n\
             system       : {}\n\
             content sha1 : {}\n\
             content sha256: {}\n\
             adapter      : {}\n\
             frames       : [{}..{})\n\
             events       : {}\n\
             integrity    : {}\n",
            summary.format_version,
            summary.capture_id,
            summary.system,
            summary.content_sha1.as_deref().unwrap_or("-"),
            summary.content_sha256.as_deref().unwrap_or("-"),
            summary.adapter,
            summary.frame_start,
            summary.frame_end,
            summary.event_count,
            summary.integrity,
        ),
    }
}
