pub mod analysis;
pub mod bundle;
pub mod contracts;
pub mod cue;
pub mod gdb_rsp;
pub mod launch;
pub mod live;
pub mod mcp_result;
pub mod mcp_stdio;
#[cfg(unix)]
pub mod n64_adapter;
pub mod nds_bridge;
pub mod neogeo_bridge;
pub mod numparse;
pub mod offload;
pub mod openmsx_bridge;
pub mod pc98_bridge;
pub mod pcsx2_bridge;
pub mod ppsspp_bridge;
pub mod rom;
pub mod track;

#[cfg(test)]
pub(crate) mod test_env;

#[cfg(test)]
#[path = "mcp_stdio_tests.rs"]
mod mcp_stdio_tests;
