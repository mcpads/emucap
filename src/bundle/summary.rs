use serde::Serialize;

use super::manifest::{Manifest, TriggerKind};

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
