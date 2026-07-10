#include "emucap_failure.h"

#include <cassert>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <utility>

#ifndef _WIN32
#include <sys/stat.h>
#endif

int main()
{
	EmucapSh4FailureSnapshot snapshot;
	snapshot.launch_id = "launch-test";
	snapshot.emulator_build = "build-test";
	snapshot.content = "/content/test.gdi";
	snapshot.reason = "Fatal: SH4 exception when blocked";
	snapshot.observed_at_unix_ms = 1730000000000ULL;
	snapshot.frame = 12345;
	snapshot.epc = 0x8c012340;
	snapshot.incoming_event = 0x180;
	snapshot.existing_expevt = 0x160;
	snapshot.existing_intevt = 0x320;
	snapshot.tea = 0x8c0abcde;
	for (std::size_t i = 0; i < snapshot.r.size(); ++i)
		snapshot.r[i] = static_cast<std::uint32_t>(0x1000 + i);
	for (std::size_t i = 0; i < snapshot.r_bank.size(); ++i)
		snapshot.r_bank[i] = static_cast<std::uint32_t>(0x2000 + i);
	snapshot.pc = 0x8c012342;
	snapshot.pr = 0x8c004000;
	snapshot.gbr = 0x8c100000;
	snapshot.vbr = 0x8c000000;
	snapshot.mach = 0x3001;
	snapshot.macl = 0x3002;
	snapshot.sr = 0x700000f1;
	snapshot.ssr = 0x4001;
	snapshot.spc = 0x4002;
	snapshot.sgr = 0x4003;
	snapshot.dbr = 0x4004;
	snapshot.fpul = 0x4005;
	snapshot.fpscr = 0x4006;
	for (std::size_t i = 0; i < snapshot.pc_ring.size(); ++i)
		snapshot.pc_ring[i] = static_cast<std::uint32_t>(0x8c000000 + i * 2);
	snapshot.pc_ring_head = 17;
	snapshot.pc_ring_count = snapshot.pc_ring.size();

	const std::string json = emucap_failure_json(snapshot);
	assert(json.size() <= EMUCAP_FAILURE_FILE_CAP);
	assert(json.find("\"launch_id\":\"launch-test\"") != std::string::npos);
	assert(json.find("\"emulator_build\":\"build-test\"") != std::string::npos);
	assert(json.find("\"content\":\"/content/test.gdi\"") != std::string::npos);
	assert(json.find("\"frame\":12345") != std::string::npos);
	assert(json.find("\"existing_expevt\":352") != std::string::npos);
	assert(json.find("\"existing_intevt\":800") != std::string::npos);
	assert(json.find("\"tea\":2349513950") != std::string::npos);
	assert(json.find("\"trace_scope\":\"interpreter\"") != std::string::npos);
	for (std::size_t i = 0; i < snapshot.r.size(); ++i)
		assert(json.find("\"r" + std::to_string(i) + "\":"
			+ std::to_string(snapshot.r[i])) != std::string::npos);
	for (std::size_t i = 0; i < snapshot.r_bank.size(); ++i)
		assert(json.find("\"r_bank" + std::to_string(i) + "\":"
			+ std::to_string(snapshot.r_bank[i])) != std::string::npos);
	const std::array<std::pair<const char*, std::uint32_t>, 13> special_registers{{
		{"pc", snapshot.pc}, {"pr", snapshot.pr}, {"gbr", snapshot.gbr},
		{"vbr", snapshot.vbr}, {"mach", snapshot.mach}, {"macl", snapshot.macl},
		{"sr", snapshot.sr}, {"ssr", snapshot.ssr}, {"spc", snapshot.spc},
		{"sgr", snapshot.sgr}, {"dbr", snapshot.dbr}, {"fpul", snapshot.fpul},
		{"fpscr", snapshot.fpscr},
	}};
	for (const auto& [name, value] : special_registers)
		assert(json.find("\"" + std::string(name) + "\":" + std::to_string(value))
			!= std::string::npos);
	const std::string ring_prefix = "\"pc_ring\":["
		+ std::to_string(snapshot.pc_ring[snapshot.pc_ring_head]);
	assert(json.find(ring_prefix) != std::string::npos);

	snapshot.reason.assign(EMUCAP_FAILURE_FILE_CAP * 2, 'x');
	const std::string bounded = emucap_failure_json(snapshot);
	assert(bounded.size() <= EMUCAP_FAILURE_FILE_CAP);
	assert(bounded.find("\"truncated\":true") != std::string::npos);

	snapshot.reason = "bounded";
	snapshot.content.assign(1023, 'x');
	snapshot.content += "\xed\x95\x9c"; // U+D55C; the 1024-byte cap would otherwise split it.
	const std::string utf8_bounded = emucap_failure_json(snapshot);
	assert(utf8_bounded.find("\xed") == std::string::npos);
	assert(utf8_bounded.find("\"truncated\":true") != std::string::npos);

	const auto unique = std::chrono::steady_clock::now().time_since_epoch().count();
	const std::filesystem::path dir = std::filesystem::temp_directory_path()
		/ ("emucap-failure-test-" + std::to_string(unique));
	std::filesystem::remove_all(dir);
	std::filesystem::create_directories(dir);
	const std::filesystem::path output = dir / "adapter-failure.json";
	std::string error;
	assert(emucap_write_failure_atomic(output.string(), json, &error));
	std::ifstream file(output, std::ios::binary);
	const std::string read((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
	assert(read == json);
#ifndef _WIN32
	struct stat status{};
	assert(::stat(output.c_str(), &status) == 0);
	assert((status.st_mode & 0777) == 0600);
#endif
	for (const auto& entry : std::filesystem::directory_iterator(dir))
		assert(entry.path() == output);
	std::filesystem::remove_all(dir);
	return 0;
}
