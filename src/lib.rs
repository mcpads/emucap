pub mod analysis;
pub mod build_identity;
pub mod bundle;
pub mod ccd;
pub mod content_identity;
pub mod contracts;
pub mod cue;
pub mod event_contracts;
pub mod gdb_rsp;
pub mod gdi;
pub mod input_movie;
pub mod launch;
pub mod live;
pub mod m3u;
pub mod mcp_result;
pub mod mcp_stdio;
pub mod media_graph;
#[cfg(unix)]
pub mod n64_adapter;
pub mod nds_bridge;
pub mod neogeo_bridge;
pub mod numparse;
pub mod offload;
pub mod openmsx_bridge;
pub mod path_safety;
pub mod pc98_bridge;
pub mod pcsx2_bridge;
pub mod ppsspp_bridge;
pub mod rom;
pub mod toc;
pub mod track;

#[cfg(test)]
pub(crate) mod test_env;

#[cfg(test)]
mod event_contracts_tests;

#[cfg(test)]
mod input_movie_tests;

#[cfg(test)]
mod path_safety_tests;

#[cfg(test)]
#[path = "mcp_stdio_tests.rs"]
mod mcp_stdio_tests;
