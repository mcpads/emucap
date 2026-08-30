use std::path::{Path, PathBuf};

use emucap::launch::{
    desmume_nds as desmume_nds_launch, dolphin as dolphin_launch, flycast as flycast_launch,
    mame as mame_launch, mame_neogeo as mame_neogeo_launch, mednafen as mednafen_launch,
    mesen as mesen_launch, mupen64plus as mupen64plus_launch, np2kai as np2kai_launch,
    openmsx as openmsx_launch, pcsx2 as pcsx2_launch, ppsspp as ppsspp_launch, xemu as xemu_launch,
    RuntimeEnv,
};
use emucap::live::link::{EmulatorIdentity, EmulatorLink};
use emucap::live::runtime::{LeaseState, ManifestSpec, ProcessState, RuntimeStore};
use emucap::live::task_entry::{
    admit_generation_transition, observe_runtime, EntryReason, ListenerState, TransitionAdmission,
    TransitionIntent,
};

use crate::args::{LaunchArgs, LaunchPlanArgs};
use crate::status::{
    button_hint_for_system, enrich_link_status, find_repo_root, observe_control_state,
    runtime_paths, supported_system_ids_value, system_catalog_revision, BUILD_HASH,
};

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;

#[path = "launch/actions.rs"]
mod actions;
#[path = "launch/controlled.rs"]
mod controlled;
#[path = "launch/mame_neogeo.rs"]
mod mame_neogeo;
#[path = "launch/media.rs"]
mod media;
#[path = "launch/mesen.rs"]
mod mesen;
#[path = "launch/openmsx.rs"]
mod openmsx;
#[path = "launch/pc98.rs"]
mod pc98;
#[path = "launch/pc98_preconditions.rs"]
mod pc98_preconditions;
#[path = "launch/plan.rs"]
mod plan;
#[path = "launch/recording.rs"]
mod recording;
#[path = "launch/run.rs"]
mod run;
#[path = "launch/system.rs"]
mod system;
#[path = "launch/xemu.rs"]
mod xemu;

pub(crate) use actions::apply_task_entry_transition;
pub(crate) use plan::make_launch_plan;
pub(crate) use run::{make_launch, occupied_graceful};

use controlled::*;
use mesen::*;
use pc98::*;
use pc98_preconditions::*;

#[cfg(test)]
use media::*;
#[cfg(test)]
use plan::*;
#[cfg(test)]
use run::*;
