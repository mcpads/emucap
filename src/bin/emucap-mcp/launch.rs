use std::path::{Path, PathBuf};

use emucap::launch::{
    desmume_nds as desmume_nds_launch, dolphin as dolphin_launch, flycast as flycast_launch,
    mame as mame_launch, mame_neogeo as mame_neogeo_launch, mednafen as mednafen_launch,
    mesen as mesen_launch, mupen64plus as mupen64plus_launch, pcsx2 as pcsx2_launch,
    ppsspp as ppsspp_launch, RuntimeEnv,
};
use emucap::live::link::{EmulatorIdentity, EmulatorLink};
use emucap::live::runtime::{LeaseState, ManifestSpec, ProcessState, RuntimeStore};

use crate::args::{LaunchArgs, LaunchPlanArgs};
use crate::status::{
    button_hint_for_system, enrich_link_status, find_repo_root, make_bootstrap_value,
    runtime_paths, supported_systems_value, BUILD_HASH,
};

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;

#[path = "launch/media.rs"]
mod media;
#[path = "launch/plan.rs"]
mod plan;
#[path = "launch/run.rs"]
mod run;

pub(crate) use plan::make_launch_plan;
pub(crate) use run::{make_launch, occupied_graceful};

#[cfg(test)]
use media::*;
#[cfg(test)]
use plan::*;
#[cfg(test)]
use run::*;
