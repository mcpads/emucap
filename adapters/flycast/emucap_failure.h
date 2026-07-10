#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <string>

constexpr std::size_t EMUCAP_CRASH_PC_CAP = 512;
constexpr std::size_t EMUCAP_FAILURE_FILE_CAP = 128 * 1024;

struct EmucapSh4FailureSnapshot {
	std::string launch_id;
	std::string emulator_build;
	std::string content;
	std::string reason;
	std::uint64_t observed_at_unix_ms = 0;
	std::uint64_t frame = 0;
	std::uint32_t epc = 0;
	std::uint32_t incoming_event = 0;
	std::uint32_t existing_expevt = 0;
	std::uint32_t existing_intevt = 0;
	std::uint32_t tea = 0;
	std::array<std::uint32_t, 16> r{};
	std::array<std::uint32_t, 8> r_bank{};
	std::uint32_t pc = 0;
	std::uint32_t pr = 0;
	std::uint32_t gbr = 0;
	std::uint32_t vbr = 0;
	std::uint32_t mach = 0;
	std::uint32_t macl = 0;
	std::uint32_t sr = 0;
	std::uint32_t ssr = 0;
	std::uint32_t spc = 0;
	std::uint32_t sgr = 0;
	std::uint32_t dbr = 0;
	std::uint32_t fpul = 0;
	std::uint32_t fpscr = 0;
	std::array<std::uint32_t, EMUCAP_CRASH_PC_CAP> pc_ring{};
	std::size_t pc_ring_head = 0;
	std::size_t pc_ring_count = 0;
};

/// Serialize a bounded, self-identifying failure artifact. Registers are always retained;
/// `truncated` marks bounded text or the last-resort omission of an oversized future ring payload.
std::string emucap_failure_json(const EmucapSh4FailureSnapshot& snapshot);

/// Write through a private temporary file and atomically replace `path`. The generation directory
/// is created by the Rust launcher; this function never creates or traverses parent directories.
bool emucap_write_failure_atomic(
	const std::string& path,
	const std::string& json,
	std::string* error = nullptr);
