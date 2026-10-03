#ifndef EMUCAP_ATOMIC_FILE_H
#define EMUCAP_ATOMIC_FILE_H
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <system_error>
#ifdef _WIN32
#include <windows.h>
#else
#include <cerrno>
#include <cstdio>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

// Same-directory publication. Errors before replacement preserve the destination;
// cleanup removes only the temporary file exclusively created by this invocation.
class EmucapAtomicFile {
 public:
  static void write(const std::string& path, const void* data, std::size_t size) {
    if (path.empty() || path.find('\0') != std::string::npos)
      throw std::runtime_error("invalid state output path");
    EmucapAtomicFile file(path);
    file.store(static_cast<const std::uint8_t*>(data), size);
    file.publish();
  }
 private:
#ifdef _WIN32
  using Path = std::wstring;
  HANDLE fd_ = INVALID_HANDLE_VALUE;
  static Path native(const std::string& path) {
    const int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path.c_str(), -1, nullptr, 0);
    if (!count) fail("decode state output path");
    Path result(count, 0);
    if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path.c_str(), -1, &result[0], count))
      fail("decode state output path");
    result.resize(count - 1);
    return result;
  }
  [[noreturn]] static void fail(const char* operation) {
    throw std::system_error(GetLastError(), std::system_category(), operation);
  }
  static void check_destination(const Path& path) {
    const auto attrs = GetFileAttributesW(path.c_str());
    if (attrs == INVALID_FILE_ATTRIBUTES) {
      if (GetLastError() != ERROR_FILE_NOT_FOUND && GetLastError() != ERROR_PATH_NOT_FOUND)
        fail("inspect state destination");
    } else if (attrs & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT))
      throw std::runtime_error("state destination must be a regular file");
  }
#else
  using Path = std::string;
  int fd_ = -1;
  static Path native(const std::string& path) { return path; }
  [[noreturn]] static void fail(const char* operation) {
    throw std::system_error(errno, std::generic_category(), operation);
  }
  static void check_destination(const Path& path) {
    struct stat st;
    if (::lstat(path.c_str(), &st)) {
      if (errno != ENOENT) fail("inspect state destination");
    } else if (!S_ISREG(st.st_mode))
      throw std::runtime_error("state destination must be a regular file");
  }
#endif
  Path destination_, temporary_;
  bool owned_ = false;

  explicit EmucapAtomicFile(const std::string& path) : destination_(native(path)) {
    check_destination(destination_);
#ifdef _WIN32
    const auto slash = path.find_last_of("/\\");
    const auto directory = slash == std::string::npos
        ? (path.size() > 1 && path[1] == ':' ? path.substr(0, 2) : std::string())
        : path.substr(0, slash + 1);
    const auto pid = GetCurrentProcessId();
#else
    const auto slash = path.find_last_of('/');
    const auto directory = slash == std::string::npos ? std::string() : path.substr(0, slash + 1);
    const auto pid = getpid();
#endif
    static std::atomic<unsigned long long> serial{0};
    const auto tick = std::chrono::steady_clock::now().time_since_epoch().count();
    for (unsigned attempt = 0; attempt < 128; ++attempt) {
      temporary_ = native(directory + ".emucap-state-" + std::to_string(pid) + "-"
          + std::to_string(tick) + "-" + std::to_string(serial.fetch_add(1)) + ".tmp");
#ifdef _WIN32
      fd_ = CreateFileW(temporary_.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW,
                        FILE_ATTRIBUTE_NORMAL, nullptr);
      if (fd_ != INVALID_HANDLE_VALUE) { owned_ = true; return; }
      if (GetLastError() != ERROR_FILE_EXISTS && GetLastError() != ERROR_ALREADY_EXISTS)
        fail("create temporary state file");
#else
      fd_ = ::open(temporary_.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
      if (fd_ >= 0) { owned_ = true; return; }
      if (errno != EEXIST) fail("create temporary state file");
#endif
    }
    throw std::runtime_error("temporary state filename collisions");
  }
  ~EmucapAtomicFile() noexcept {
#ifdef _WIN32
    if (fd_ != INVALID_HANDLE_VALUE) CloseHandle(fd_);
    if (owned_) DeleteFileW(temporary_.c_str());
#else
    if (fd_ >= 0) ::close(fd_);
    if (owned_) ::unlink(temporary_.c_str());
#endif
  }
  void store(const std::uint8_t* data, std::size_t size) {
    while (size) {
      const auto chunk = std::min<std::size_t>(size, 1024 * 1024);
#ifdef _WIN32
      DWORD done = 0;
      if (!WriteFile(fd_, data, DWORD(chunk), &done, nullptr)) fail("write state file");
#else
      const auto done = ::write(fd_, data, chunk);
      if (done < 0) { if (errno == EINTR) continue; fail("write state file"); }
#endif
      if (!done) throw std::runtime_error("state file write made no progress");
      data += done; size -= done;
    }
#ifdef _WIN32
    if (!FlushFileBuffers(fd_)) fail("sync state file");
    const auto handle = fd_; fd_ = INVALID_HANDLE_VALUE;
    if (!CloseHandle(handle)) fail("close state file");
#else
    while (::fsync(fd_)) { if (errno != EINTR) fail("sync state file"); }
    const auto handle = fd_; fd_ = -1;
    if (::close(handle)) fail("close state file");
#endif
  }
  void publish() {
    check_destination(destination_);
#ifdef _WIN32
    if (!MoveFileExW(temporary_.c_str(), destination_.c_str(),
                     MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) fail("publish state file");
#else
    if (::rename(temporary_.c_str(), destination_.c_str())) fail("publish state file");
#endif
    owned_ = false;
  }
  EmucapAtomicFile(const EmucapAtomicFile&) = delete;
  EmucapAtomicFile& operator=(const EmucapAtomicFile&) = delete;
};
#endif
