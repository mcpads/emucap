#pragma once

#include <cctype>
#include <cstdint>
#include <cstdlib>
#include <limits>
#include <string>
#include <vector>

enum class EmucapV810CallKind {
  other,
  call,
  return_from_call,
};

inline EmucapV810CallKind emucap_v810_classify(std::uint16_t first_halfword) {
  const std::uint8_t low = first_halfword & 0xFF;
  const std::uint8_t high = first_halfword >> 8;
  const std::uint8_t opcode = (high & 0xE0) == 0x80 ? high >> 1 : high >> 2;
  if (opcode == 0x2B) return EmucapV810CallKind::call;  // JAL disp26
  if (opcode == 0x06 && (low & 0x1F) == 31)
    return EmucapV810CallKind::return_from_call;  // JMP [r31]
  return EmucapV810CallKind::other;
}

enum class EmucapPcfxAuxMapStatus {
  mapped,
  unsupported_region,
  unaligned,
  out_of_range,
};

struct EmucapPcfxAuxRange {
  std::uint32_t start;
  std::uint32_t end;
};

inline EmucapPcfxAuxMapStatus emucap_pcfx_aux_range(
    const std::string& memory_type,
    std::uint32_t public_start,
    std::uint32_t public_end,
    EmucapPcfxAuxRange& output) {
  std::uint32_t base = 0;
  std::uint32_t size = 0;
  if (memory_type == "kram0") {
    size = 0x80000;
  } else if (memory_type == "kram1") {
    base = 0x40000;
    size = 0x80000;
  } else if (memory_type == "vdcvram0") {
    base = 0x80000;
    size = 0x20000;
  } else if (memory_type == "vdcvram1") {
    base = 0x90000;
    size = 0x20000;
  } else {
    return EmucapPcfxAuxMapStatus::unsupported_region;
  }
  if (public_start >= size || public_end >= size || public_end < public_start)
    return EmucapPcfxAuxMapStatus::out_of_range;
  if ((public_start & 1) != 0 || (public_end & 1) == 0)
    return EmucapPcfxAuxMapStatus::unaligned;
  output.start = base + public_start / 2;
  output.end = base + public_end / 2;
  return EmucapPcfxAuxMapStatus::mapped;
}

inline bool emucap_pcfx_aux_public_range(
    std::uint32_t backend_address,
    std::uint32_t backend_word_count,
    std::string& memory_type,
    std::uint32_t& public_address,
    std::uint32_t& public_length) {
  std::uint32_t base = 0;
  if (backend_address < 0x40000) {
    memory_type = "kram0";
  } else if (backend_address < 0x80000) {
    memory_type = "kram1";
    base = 0x40000;
  } else if (backend_address < 0x90000) {
    memory_type = "vdcvram0";
    base = 0x80000;
  } else if (backend_address < 0xA0000) {
    memory_type = "vdcvram1";
    base = 0x90000;
  } else {
    return false;
  }
  if (backend_word_count == 0
      || backend_word_count > UINT32_MAX / 2
      || backend_address - base > UINT32_MAX / 2)
    return false;
  public_address = (backend_address - base) * 2;
  public_length = backend_word_count * 2;
  return true;
}

struct EmucapSnapshotSpec {
  std::string memory_type;
  std::uint32_t address;
  std::uint32_t length;
};

enum class EmucapSnapshotParseStatus {
  absent,
  valid,
  invalid,
};

inline bool emucap_parse_snapshot_number(
    const std::string& value,
    std::uint32_t& output) {
  const char* begin = value.c_str();
  int base = 10;
  if (value.size() > 2 && value[0] == '0'
      && (value[1] == 'x' || value[1] == 'X')) {
    begin += 2;
    base = 16;
  } else if (value.size() > 1 && value[0] == '$') {
    begin += 1;
    base = 16;
  }
  if (*begin == '\0' || *begin == '-' || *begin == '+') return false;
  char* end = nullptr;
  const unsigned long long parsed = std::strtoull(begin, &end, base);
  if (end == begin || *end != '\0'
      || parsed > std::numeric_limits<std::uint32_t>::max())
    return false;
  output = static_cast<std::uint32_t>(parsed);
  return true;
}

inline EmucapSnapshotParseStatus emucap_parse_snapshot_specs(
    const std::string& input,
    std::vector<EmucapSnapshotSpec>& output,
    std::string& error) {
  const std::string key = "\"snapshot\"";
  std::size_t pos = input.find(key);
  if (pos == std::string::npos) return EmucapSnapshotParseStatus::absent;
  pos = input.find(':', pos + key.size());
  if (pos == std::string::npos) {
    error = "snapshot is missing ':'";
    return EmucapSnapshotParseStatus::invalid;
  }
  pos++;
  while (pos < input.size() && std::isspace(static_cast<unsigned char>(input[pos]))) pos++;
  if (pos >= input.size() || input[pos++] != '[') {
    error = "snapshot must be an array";
    return EmucapSnapshotParseStatus::invalid;
  }
  output.clear();
  for (;;) {
    while (pos < input.size() && std::isspace(static_cast<unsigned char>(input[pos]))) pos++;
    if (pos >= input.size()) {
      error = "snapshot array is unterminated";
      return EmucapSnapshotParseStatus::invalid;
    }
    if (input[pos] == ']') return EmucapSnapshotParseStatus::valid;
    if (input[pos++] != '"') {
      error = "snapshot entries must be strings";
      return EmucapSnapshotParseStatus::invalid;
    }
    const std::size_t begin = pos;
    while (pos < input.size() && input[pos] != '"') {
      if (input[pos] == '\\') {
        error = "snapshot entries do not accept escaped characters";
        return EmucapSnapshotParseStatus::invalid;
      }
      pos++;
    }
    if (pos >= input.size()) {
      error = "snapshot string is unterminated";
      return EmucapSnapshotParseStatus::invalid;
    }
    const std::string value = input.substr(begin, pos - begin);
    pos++;
    const std::size_t second = value.rfind(':');
    const std::size_t first =
        second == std::string::npos ? std::string::npos : value.rfind(':', second - 1);
    EmucapSnapshotSpec spec{};
    if (first == std::string::npos || second == std::string::npos
        || first == 0 || first + 1 == second || second + 1 == value.size()
        || !emucap_parse_snapshot_number(
            value.substr(first + 1, second - first - 1), spec.address)
        || !emucap_parse_snapshot_number(value.substr(second + 1), spec.length)
        || spec.length == 0) {
      error = "snapshot entries must be memory_type:address:length";
      return EmucapSnapshotParseStatus::invalid;
    }
    spec.memory_type = value.substr(0, first);
    output.push_back(spec);
    while (pos < input.size() && std::isspace(static_cast<unsigned char>(input[pos]))) pos++;
    if (pos >= input.size()) {
      error = "snapshot array is unterminated";
      return EmucapSnapshotParseStatus::invalid;
    }
    if (input[pos] == ',') {
      pos++;
      std::size_t next = pos;
      while (next < input.size()
             && std::isspace(static_cast<unsigned char>(input[next])))
        next++;
      if (next >= input.size() || input[next] == ']') {
        error = "snapshot array has a trailing comma";
        return EmucapSnapshotParseStatus::invalid;
      }
      continue;
    }
    if (input[pos] != ']') {
      error = "snapshot entries must be comma-separated";
      return EmucapSnapshotParseStatus::invalid;
    }
  }
}
