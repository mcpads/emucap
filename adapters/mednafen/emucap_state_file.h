#ifndef EMUCAP_STATE_FILE_H
#define EMUCAP_STATE_FILE_H
#include <cstdint>
#include <cstring>
#include <set>
#include <stdexcept>
#include <string>

// Structural admission only. Required sections, field types and guest invariants
// remain the native loader's responsibility; its mutating failures quarantine control.
class EmucapStateFile {
 public:
  static constexpr std::uint64_t max_bytes = 128ULL * 1024 * 1024;
  static void validate(const std::uint8_t* bytes, std::size_t size) {
    if (size < 32 || size > max_bytes
        || (std::memcmp(bytes, "MDFNSVST", 8) && std::memcmp(bytes, "MEDNAFENSVESTATE", 16)))
      fail("invalid native state header or size");
    const auto version = word(bytes + 16);
    if (version < 0x900 || version > 0x7fffffff) fail("unsupported native state version");
    if ((word(bytes + 20) & 0x7fffffff) != size) fail("native state length differs");
    const std::uint64_t width = word(bytes + 24), height = word(bytes + 28);
    if (height && width > (size - 32) / 3 / height) fail("native preview exceeds state");
    std::size_t cursor = 32 + width * height * 3;
    std::set<std::string> sections;
    std::size_t total_fields = 0;
    while (cursor < size) {
      require(cursor, 36, size);
      const auto section = name(bytes + cursor, 32);
      if (sections.size() >= 4096 || !sections.insert(section).second)
        fail("duplicate or excessive native state sections");
      const auto length = word(bytes + cursor + 32);
      cursor += 36;
      require(cursor, length, size);
      const auto end = cursor + length;
      std::set<std::string> fields;
      while (cursor < end) {
        require(cursor, 1, end);
        const auto name_size = bytes[cursor++];
        require(cursor, std::size_t(name_size) + 4, end);
        const auto field = name(bytes + cursor, name_size);
        if (++total_fields > 65536 || !fields.insert(field).second)
          fail("duplicate or excessive native state fields");
        cursor += name_size;
        const auto count = word(bytes + cursor);
        cursor += 4;
        require(cursor, count, end);
        cursor += count;
      }
    }
  }
 private:
  static std::uint32_t word(const std::uint8_t* p) noexcept {
    return std::uint32_t(p[0]) | (std::uint32_t(p[1]) << 8)
        | (std::uint32_t(p[2]) << 16) | (std::uint32_t(p[3]) << 24);
  }
  static void require(std::size_t cursor, std::size_t count, std::size_t end) {
    if (cursor > end || count > end - cursor) fail("truncated native state record");
  }
  static std::string name(const std::uint8_t* p, std::size_t count) {
    std::size_t size = 0;
    while (size < count && p[size]) ++size;
    if (!size) fail("empty native state name");
    return std::string(reinterpret_cast<const char*>(p), size);
  }
  [[noreturn]] static void fail(const char* reason) { throw std::runtime_error(reason); }
};
#endif
