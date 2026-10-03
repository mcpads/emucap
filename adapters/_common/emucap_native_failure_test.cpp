#include "emucap_native_failure.h"

#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <string>

#ifdef _WIN32
#include <direct.h>
#include <io.h>
#include <process.h>
#include <windows.h>
#else
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace {

int process_id() {
#ifdef _WIN32
  return ::_getpid();
#else
  return static_cast<int>(::getpid());
#endif
}

int make_dir(const std::string& path) {
#ifdef _WIN32
  return ::_mkdir(path.c_str());
#else
  return ::mkdir(path.c_str(), 0700);
#endif
}

void remove_file(const std::string& path) {
#ifdef _WIN32
  (void)::_unlink(path.c_str());
#else
  (void)::unlink(path.c_str());
#endif
}

void remove_dir(const std::string& path) {
#ifdef _WIN32
  (void)::_rmdir(path.c_str());
#else
  (void)::rmdir(path.c_str());
#endif
}

}  // namespace

int main() {
  EmucapNativeFailureSnapshot snapshot;
  snapshot.launch_id = "launch-test";
  snapshot.adapter = "mednafen-native";
  snapshot.emulator_build = "build-test";
  snapshot.content = "/content/test.cue";
  snapshot.operation = "service";
  snapshot.reason = "failure \"with\" newline\n";
  snapshot.execution_state = "unknown";
  snapshot.observed_at_unix_ms = 1730000000000ULL;
  snapshot.frame = 12345;
  snapshot.active = true;

  const std::string json = emucap_native_failure_json(snapshot);
  assert(json.size() <= EMUCAP_NATIVE_FAILURE_FILE_CAP);
  assert(json.find("\"kind\":\"adapter_internal_error\"") != std::string::npos);
  assert(json.find("\"operation\":\"service\"") != std::string::npos);
  assert(json.find("\"reason\":\"failure \\\"with\\\" newline\\n\"")
      != std::string::npos);
  assert(json.find("\"active\":true") != std::string::npos);
  assert(json.find("\"execution_state\":\"unknown\"") != std::string::npos);

  snapshot.reason.assign(EMUCAP_NATIVE_FAILURE_FILE_CAP * 2, 'x');
  const std::string bounded = emucap_native_failure_json(snapshot);
  assert(bounded.size() <= EMUCAP_NATIVE_FAILURE_FILE_CAP);
  assert(bounded.find("\"truncated\":true") != std::string::npos);

#ifdef _WIN32
  char temp_path[MAX_PATH + 1];
  const DWORD temp_size = GetTempPathA(sizeof(temp_path), temp_path);
  assert(temp_size > 0 && temp_size < sizeof(temp_path));
  const char* tmp = temp_path;
#else
  const char* tmp = std::getenv("TMPDIR");
#endif
  const std::string root =
      std::string(tmp != nullptr && tmp[0] != '\0' ? tmp : "/tmp")
      + "/emucap native failure $()-" + std::to_string(process_id());
  remove_dir(root);
  assert(make_dir(root) == 0);
  const std::string output = root + "/adapter-failure.json";
  std::string error;
  assert(emucap_write_native_failure_atomic(output, json, &error));

  std::ifstream file(output.c_str(), std::ios::binary);
  const std::string read(
      (std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
  assert(read == json);
  file.close();
#ifndef _WIN32
  struct stat status {};
  assert(::stat(output.c_str(), &status) == 0);
  assert((status.st_mode & 0777) == 0600);
#endif
  remove_file(output);
  remove_dir(root);
  return 0;
}
