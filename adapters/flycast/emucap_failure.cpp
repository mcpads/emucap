#include "emucap_failure.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <sstream>

#ifdef _WIN32
#include <io.h>
#include <windows.h>
#else
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace {

std::string json_escape(const std::string& value)
{
	std::string escaped;
	escaped.reserve(value.size());
	for (unsigned char ch : value)
	{
		switch (ch)
		{
		case '"': escaped += "\\\""; break;
		case '\\': escaped += "\\\\"; break;
		case '\n': escaped += "\\n"; break;
		case '\r': escaped += "\\r"; break;
		case '\t': escaped += "\\t"; break;
		default:
			if (ch < 0x20)
			{
				char buffer[8];
				std::snprintf(buffer, sizeof(buffer), "\\u%04x", ch);
				escaped += buffer;
			}
			else
				escaped += static_cast<char>(ch);
		}
	}
	return escaped;
}

std::string bounded_text(const std::string& value, std::size_t cap, bool& truncated)
{
	if (value.size() <= cap)
		return value;
	truncated = true;
	std::size_t cut = cap;
	// Runtime paths are UTF-8 on supported launchers. Do not cut inside a multibyte code point and
	// turn the entire otherwise-durable JSON artifact into invalid UTF-8.
	while (cut > 0 && (static_cast<unsigned char>(value[cut]) & 0xC0) == 0x80)
		--cut;
	return value.substr(0, cut);
}

FILE* open_private_temp(const std::filesystem::path& path)
{
#ifdef _WIN32
	return _wfopen(path.c_str(), L"wb");
#else
	int fd = ::open(path.c_str(), O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, 0600);
	return fd < 0 ? nullptr : ::fdopen(fd, "wb");
#endif
}

bool sync_file(FILE* file)
{
	if (std::fflush(file) != 0)
		return false;
#ifdef _WIN32
	return ::_commit(::_fileno(file)) == 0;
#else
	return ::fsync(::fileno(file)) == 0;
#endif
}

bool atomic_replace(const std::filesystem::path& source, const std::filesystem::path& target)
{
#ifdef _WIN32
	return ::MoveFileExW(
		source.c_str(),
		target.c_str(),
		MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) != 0;
#else
	if (::rename(source.c_str(), target.c_str()) != 0)
		return false;
	int dir = ::open(target.parent_path().c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC);
	if (dir >= 0)
	{
		(void)::fsync(dir);
		::close(dir);
	}
	return true;
#endif
}

} // namespace

std::string emucap_failure_json(const EmucapSh4FailureSnapshot& snapshot)
{
	bool truncated = false;
	const std::string launch_id = bounded_text(snapshot.launch_id, 128, truncated);
	const std::string emulator_build = bounded_text(snapshot.emulator_build, 128, truncated);
	const std::string content = bounded_text(snapshot.content, 1024, truncated);
	const std::string reason = bounded_text(snapshot.reason, 512, truncated);
	std::ostringstream out;
	out << "{\"schema_version\":1"
		<< ",\"launch_id\":\"" << json_escape(launch_id) << '"'
		<< ",\"adapter\":\"flycast-native\""
		<< ",\"emulator_build\":\"" << json_escape(emulator_build) << '"'
		<< ",\"content\":\"" << json_escape(content) << '"'
		<< ",\"kind\":\"sh4_fatal\""
		<< ",\"reason\":\"" << json_escape(reason) << '"'
		<< ",\"observed_at_unix_ms\":" << snapshot.observed_at_unix_ms
		<< ",\"frame\":" << snapshot.frame
		<< ",\"epc\":" << snapshot.epc
		<< ",\"incoming_event\":" << snapshot.incoming_event
		<< ",\"existing_expevt\":" << snapshot.existing_expevt
		<< ",\"existing_intevt\":" << snapshot.existing_intevt
		<< ",\"tea\":" << snapshot.tea
		<< ",\"trace_scope\":\"interpreter\""
		<< ",\"registers\":{";
	for (std::size_t i = 0; i < snapshot.r.size(); ++i)
		out << (i == 0 ? "" : ",") << "\"r" << i << "\":" << snapshot.r[i];
	for (std::size_t i = 0; i < snapshot.r_bank.size(); ++i)
		out << ",\"r_bank" << i << "\":" << snapshot.r_bank[i];
	out << ",\"pc\":" << snapshot.pc
		<< ",\"pr\":" << snapshot.pr
		<< ",\"gbr\":" << snapshot.gbr
		<< ",\"vbr\":" << snapshot.vbr
		<< ",\"mach\":" << snapshot.mach
		<< ",\"macl\":" << snapshot.macl
		<< ",\"sr\":" << snapshot.sr
		<< ",\"ssr\":" << snapshot.ssr
		<< ",\"spc\":" << snapshot.spc
		<< ",\"sgr\":" << snapshot.sgr
		<< ",\"dbr\":" << snapshot.dbr
		<< ",\"fpul\":" << snapshot.fpul
		<< ",\"fpscr\":" << snapshot.fpscr
		<< "},\"pc_ring\":[";
	const std::size_t count = std::min(snapshot.pc_ring_count, EMUCAP_CRASH_PC_CAP);
	const std::size_t head = snapshot.pc_ring_head & (EMUCAP_CRASH_PC_CAP - 1);
	for (std::size_t i = 0; i < count; ++i)
	{
		const std::size_t index = (head + EMUCAP_CRASH_PC_CAP - count + i)
			& (EMUCAP_CRASH_PC_CAP - 1);
		out << (i == 0 ? "" : ",") << snapshot.pc_ring[index];
	}
	out << "],\"truncated\":" << (truncated ? "true" : "false") << '}';
	std::string json = out.str();
	if (json.size() > EMUCAP_FAILURE_FILE_CAP && snapshot.pc_ring_count > 0)
	{
		// The fixed-size schema is normally below 16 KiB. Keep exact registers even if a future
		// schema addition accidentally exceeds the contract, and explicitly mark the ring omitted.
		// The count guard also makes this fallback single-shot if future fixed fields grow too large.
		EmucapSh4FailureSnapshot reduced = snapshot;
		reduced.launch_id = launch_id;
		reduced.emulator_build = emulator_build;
		reduced.content = content;
		reduced.reason = reason;
		reduced.pc_ring_count = 0;
		json = emucap_failure_json(reduced);
		const std::string marker = "\"truncated\":false";
		if (const std::size_t pos = json.rfind(marker); pos != std::string::npos)
			json.replace(pos, marker.size(), "\"truncated\":true");
	}
	return json;
}

bool emucap_write_failure_atomic(
	const std::string& path,
	const std::string& json,
	std::string* error)
{
	if (path.empty() || json.size() > EMUCAP_FAILURE_FILE_CAP)
	{
		if (error != nullptr)
			*error = path.empty() ? "failure path is empty" : "failure JSON exceeds 128 KiB";
		return false;
	}
	static std::atomic<unsigned long long> sequence{0};
	const auto stamp = std::chrono::steady_clock::now().time_since_epoch().count();
	const std::filesystem::path target = std::filesystem::u8path(path);
	const std::filesystem::path temporary = target.parent_path()
		/ ("." + target.filename().string() + "." + std::to_string(stamp) + "."
			+ std::to_string(sequence.fetch_add(1)) + ".tmp");
	FILE* file = open_private_temp(temporary);
	if (file == nullptr)
	{
		if (error != nullptr)
			*error = "cannot create private failure temp file";
		return false;
	}
	const bool wrote = std::fwrite(json.data(), 1, json.size(), file) == json.size();
	const bool synced = wrote && sync_file(file);
	const bool closed = std::fclose(file) == 0;
	if (!synced || !closed || !atomic_replace(temporary, target))
	{
		std::error_code ignored;
		std::filesystem::remove(temporary, ignored);
		if (error != nullptr)
			*error = "cannot publish failure file atomically";
		return false;
	}
	return true;
}
