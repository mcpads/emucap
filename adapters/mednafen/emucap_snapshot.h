#ifndef EMUCAP_SNAPSHOT_H
#define EMUCAP_SNAPSHOT_H
#include <mednafen/mednafen.h>
#include <mednafen/hash/sha256.h>
#include "emucap_state_file.h"
#include <array>
#include <vector>

// Native prefix followed by one bounded, digest-bound history record. Blocks are
// raster, filters, completed observation and optional active-frame output.
// Decode stages bytes only; participant preparation must precede guest mutation.
class EmucapSnapshot {
 public:
  using Blocks = std::array<std::vector<std::uint8_t>, 4>;
  using Identity = std::array<std::uint8_t, 32>;
  static constexpr std::size_t max_bytes = 512ULL * 1024 * 1024;
  std::size_t native_size = 0;
  bool history = false;
  Blocks blocks;

  static Identity identity(const char* module, const std::uint8_t* content) {
    Identity result{};
    const auto length = std::strlen(module);
    if (!length || length >= 16) throw std::runtime_error("invalid snapshot module identity");
    std::memcpy(result.data(), module, length);
    std::memcpy(result.data() + 16, content, 16);
    return result;
  }

  static std::vector<std::uint8_t> encode(const std::uint8_t* native, std::size_t size,
                                         const Identity& identity, const Blocks& blocks) {
    EmucapStateFile::validate(native, size);
    std::size_t total = size + header_size + 32;
    for (unsigned i = 0; i < blocks.size(); ++i) {
      check_size(i, blocks[i].size());
      if (blocks[i].size() > max_bytes - total) fail();
      total += blocks[i].size();
    }
    std::vector<std::uint8_t> bytes;
    bytes.reserve(total);
    bytes.insert(bytes.end(), native, native + size);
    const char magic[] = "ECHSTATE";
    bytes.insert(bytes.end(), magic, magic + 8);
    bytes.insert(bytes.end(), identity.begin(), identity.end());
    for (const auto& block : blocks)
      for (unsigned i = 0; i < 8; ++i) bytes.push_back(std::uint64_t(block.size()) >> (i * 8));
    for (const auto& block : blocks) bytes.insert(bytes.end(), block.begin(), block.end());
    const auto digest = Mednafen::sha256(bytes.data(), bytes.size());
    bytes.insert(bytes.end(), digest.begin(), digest.end());
    return bytes;
  }

  static EmucapSnapshot decode(const std::uint8_t* bytes, std::size_t size,
                                const Identity& identity) {
    if (size < 32 || size > max_bytes) fail();
    EmucapSnapshot result;
    result.native_size = word(bytes + 20, 4) & 0x7fffffff;
    if (result.native_size > size) fail();
    EmucapStateFile::validate(bytes, result.native_size);
    if (size == result.native_size) return result; // Explicit legacy native-only path.
    const auto* header = bytes + result.native_size;
    if (size - result.native_size < header_size + 32
        || std::memcmp(header, "ECHSTATE", 8)
        || std::memcmp(header + 8, identity.data(), identity.size())) fail();
    const auto digest = Mednafen::sha256(bytes, size - 32);
    if (std::memcmp(bytes + size - 32, digest.data(), 32))
      throw std::runtime_error("snapshot history digest differs");
    std::array<std::size_t, 4> lengths{};
    std::size_t cursor = result.native_size + header_size;
    for (unsigned i = 0; i < lengths.size(); ++i) {
      const auto length = word(header + 40 + i * 8, 8);
      check_size(i, length);
      if (length > size - 32 - cursor) fail();
      lengths[i] = length;
      cursor += length;
    }
    if (cursor != size - 32) fail();
    cursor = result.native_size + header_size;
    for (unsigned i = 0; i < lengths.size(); ++i) {
      result.blocks[i].assign(bytes + cursor, bytes + cursor + lengths[i]);
      cursor += lengths[i];
    }
    result.history = true;
    return result;
  }

 private:
  static constexpr std::size_t header_size = 72;
  static std::uint64_t word(const std::uint8_t* p, unsigned size) noexcept {
    std::uint64_t value = 0;
    for (unsigned i = 0; i < size; ++i) value |= std::uint64_t(p[i]) << (i * 8);
    return value;
  }
  static void check_size(unsigned index, std::uint64_t size) {
    const std::uint64_t limits[] = {256ULL * 1024 * 1024 + 128,
        128ULL * 1024 * 1024 + 40, 128ULL * 1024 * 1024 + 48, 4ULL * 1024 * 1024 + 84};
    if (size > limits[index] || (index != 3 && !size)) fail();
  }
  [[noreturn]] static void fail() { throw std::runtime_error("invalid snapshot history record"); }
};
#endif
