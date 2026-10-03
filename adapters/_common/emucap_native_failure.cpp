#include "emucap_native_failure.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <sstream>
#include <string>

#ifdef _WIN32
#include <io.h>
#include <windows.h>
#else
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace {

std::string json_escape(const std::string& value) {
  std::string escaped;
  escaped.reserve(value.size());
  for (unsigned char ch : value) {
    switch (ch) {
      case '"': escaped += "\\\""; break;
      case '\\': escaped += "\\\\"; break;
      case '\b': escaped += "\\b"; break;
      case '\f': escaped += "\\f"; break;
      case '\n': escaped += "\\n"; break;
      case '\r': escaped += "\\r"; break;
      case '\t': escaped += "\\t"; break;
      default:
        if (ch < 0x20) {
          char buffer[8];
          std::snprintf(buffer, sizeof(buffer), "\\u%04x", ch);
          escaped += buffer;
        } else {
          escaped += static_cast<char>(ch);
        }
    }
  }
  return escaped;
}

std::string bounded_text(
    const std::string& value,
    std::size_t cap,
    bool& truncated) {
  if (value.size() <= cap) return value;
  truncated = true;
  std::size_t cut = cap;
  while (cut > 0 && (static_cast<unsigned char>(value[cut]) & 0xC0) == 0x80)
    --cut;
  return value.substr(0, cut);
}

std::uint64_t unix_time_ms() {
  return static_cast<std::uint64_t>(
      std::chrono::duration_cast<std::chrono::milliseconds>(
          std::chrono::system_clock::now().time_since_epoch())
          .count());
}

struct PathParts {
  std::string parent;
  std::string filename;
  char separator;
};

bool split_path(const std::string& path, PathParts& parts) {
  if (path.empty()) return false;
  const std::size_t slash = path.find_last_of("/\\");
  if (slash == std::string::npos) {
    parts.parent = ".";
    parts.filename = path;
    parts.separator = '/';
  } else {
    parts.parent = slash == 0 ? path.substr(0, 1) : path.substr(0, slash);
    parts.filename = path.substr(slash + 1);
    parts.separator = path[slash];
  }
  return !parts.filename.empty();
}

#ifdef _WIN32
bool utf8_to_wide(const std::string& value, std::wstring& wide) {
  if (value.empty()) {
    wide.clear();
    return true;
  }
  const int size = ::MultiByteToWideChar(
      CP_UTF8, MB_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()), nullptr, 0);
  if (size <= 0) return false;
  wide.resize(static_cast<std::size_t>(size));
  return ::MultiByteToWideChar(
             CP_UTF8,
             MB_ERR_INVALID_CHARS,
             value.data(),
             static_cast<int>(value.size()),
             &wide[0],
             size) == size;
}
#endif

FILE* open_private_temp(const std::string& path) {
#ifdef _WIN32
  std::wstring wide;
  if (!utf8_to_wide(path, wide)) return nullptr;
  return ::_wfopen(wide.c_str(), L"wb");
#else
  const int fd = ::open(path.c_str(), O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, 0600);
  return fd < 0 ? nullptr : ::fdopen(fd, "wb");
#endif
}

bool sync_file(FILE* file) {
  if (std::fflush(file) != 0) return false;
#ifdef _WIN32
  return ::_commit(::_fileno(file)) == 0;
#else
  return ::fsync(::fileno(file)) == 0;
#endif
}

bool atomic_replace(
    const std::string& source,
    const std::string& target,
    const std::string& parent) {
#ifdef _WIN32
  (void)parent;
  std::wstring wide_source;
  std::wstring wide_target;
  return utf8_to_wide(source, wide_source) && utf8_to_wide(target, wide_target)
      && ::MoveFileExW(
             wide_source.c_str(),
             wide_target.c_str(),
             MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) != 0;
#else
  if (::rename(source.c_str(), target.c_str()) != 0) return false;
  const int dir = ::open(parent.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC);
  if (dir >= 0) {
    (void)::fsync(dir);
    ::close(dir);
  }
  return true;
#endif
}

void remove_file(const std::string& path) {
#ifdef _WIN32
  std::wstring wide;
  if (utf8_to_wide(path, wide)) (void)::DeleteFileW(wide.c_str());
#else
  (void)::unlink(path.c_str());
#endif
}

}  // namespace

std::string emucap_native_failure_json(const EmucapNativeFailureSnapshot& snapshot) {
  bool truncated = false;
  const std::string launch_id = bounded_text(snapshot.launch_id, 128, truncated);
  const std::string adapter = bounded_text(snapshot.adapter, 64, truncated);
  const std::string emulator_build =
      bounded_text(snapshot.emulator_build, 128, truncated);
  const std::string content = bounded_text(snapshot.content, 1024, truncated);
  const std::string operation = bounded_text(snapshot.operation, 128, truncated);
  const std::string reason = bounded_text(snapshot.reason, 1024, truncated);
  const std::string execution_state =
      bounded_text(snapshot.execution_state, 16, truncated);

  std::ostringstream out;
  out << "{\"schema_version\":1"
      << ",\"launch_id\":\"" << json_escape(launch_id) << '"'
      << ",\"adapter\":\"" << json_escape(adapter) << '"'
      << ",\"emulator_build\":\"" << json_escape(emulator_build) << '"'
      << ",\"content\":\"" << json_escape(content) << '"'
      << ",\"kind\":\"adapter_internal_error\""
      << ",\"operation\":\"" << json_escape(operation) << '"'
      << ",\"reason\":\"" << json_escape(reason) << '"'
      << ",\"observed_at_unix_ms\":" << snapshot.observed_at_unix_ms
      << ",\"frame\":" << snapshot.frame
      << ",\"active\":" << (snapshot.active ? "true" : "false")
      << ",\"execution_state\":\"" << json_escape(execution_state) << '"'
      << ",\"truncated\":" << (truncated ? "true" : "false") << '}';
  return out.str();
}

bool emucap_write_native_failure_atomic(
    const std::string& path,
    const std::string& json,
    std::string* error) {
  if (path.empty() || json.size() > EMUCAP_NATIVE_FAILURE_FILE_CAP) {
    if (error != nullptr)
      *error = path.empty() ? "failure path is empty" : "failure JSON exceeds 128 KiB";
    return false;
  }

  PathParts parts;
  if (!split_path(path, parts)) {
    if (error != nullptr) *error = "failure path has no filename";
    return false;
  }

  static std::atomic<unsigned long long> sequence(0);
  const long long stamp =
      std::chrono::steady_clock::now().time_since_epoch().count();
  const std::string temporary =
      parts.parent + parts.separator + "." + parts.filename + "."
      + std::to_string(stamp) + "." + std::to_string(sequence.fetch_add(1)) + ".tmp";

  FILE* file = open_private_temp(temporary);
  if (file == nullptr) {
    if (error != nullptr) *error = "cannot create private failure temp file";
    return false;
  }
  const bool wrote = std::fwrite(json.data(), 1, json.size(), file) == json.size();
  const bool synced = wrote && sync_file(file);
  const bool closed = std::fclose(file) == 0;
  if (!synced || !closed || !atomic_replace(temporary, path, parts.parent)) {
    remove_file(temporary);
    if (error != nullptr) *error = "cannot publish failure file atomically";
    return false;
  }
  return true;
}

bool emucap_publish_native_failure(
    const char* adapter,
    const char* emulator_build,
    std::uint64_t frame,
    const char* operation,
    const char* reason,
    bool active,
    const char* execution_state,
    std::string* error) {
  const char* failure_path = std::getenv("EMUCAP_FAILURE_FILE");
  if (failure_path == nullptr || failure_path[0] == '\0') {
    if (error != nullptr) *error = "EMUCAP_FAILURE_FILE is missing";
    return false;
  }

  EmucapNativeFailureSnapshot snapshot;
  const char* launch_id = std::getenv("EMUCAP_LAUNCH_ID");
  const char* content = std::getenv("EMUCAP_CONTENT");
  snapshot.launch_id = launch_id != nullptr ? launch_id : "";
  snapshot.adapter = adapter != nullptr ? adapter : "";
  snapshot.emulator_build = emulator_build != nullptr ? emulator_build : "";
  snapshot.content = content != nullptr ? content : "";
  snapshot.operation = operation != nullptr ? operation : "unknown";
  snapshot.reason = reason != nullptr ? reason : "unknown native adapter exception";
  snapshot.execution_state = execution_state != nullptr ? execution_state : "unknown";
  snapshot.observed_at_unix_ms = unix_time_ms();
  snapshot.frame = frame;
  snapshot.active = active;
  return emucap_write_native_failure_atomic(
      failure_path, emucap_native_failure_json(snapshot), error);
}
