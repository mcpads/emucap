#ifndef EMUCAP_JSON_STRINGS_H
#define EMUCAP_JSON_STRINGS_H

#include <cctype>
#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

enum class EmucapJsonStringArrayStatus {
  absent,
  valid,
  invalid,
};

namespace emucap_json_strings_detail {

inline bool hex_digit(char value, std::uint32_t& digit) {
  if (value >= '0' && value <= '9') digit = static_cast<std::uint32_t>(value - '0');
  else if (value >= 'a' && value <= 'f') digit = static_cast<std::uint32_t>(value - 'a' + 10);
  else if (value >= 'A' && value <= 'F') digit = static_cast<std::uint32_t>(value - 'A' + 10);
  else return false;
  return true;
}

inline bool unicode_escape(
    const std::string& json,
    std::size_t& position,
    std::uint32_t& codepoint) {
  if (position + 4 > json.size()) return false;
  codepoint = 0;
  for (unsigned index = 0; index < 4; index++) {
    std::uint32_t digit = 0;
    if (!hex_digit(json[position + index], digit)) return false;
    codepoint = (codepoint << 4) | digit;
  }
  position += 4;
  return true;
}

inline bool append_utf8(std::uint32_t codepoint, std::string& output) {
  if (codepoint <= 0x7F) {
    output.push_back(static_cast<char>(codepoint));
  } else if (codepoint <= 0x7FF) {
    output.push_back(static_cast<char>(0xC0 | (codepoint >> 6)));
    output.push_back(static_cast<char>(0x80 | (codepoint & 0x3F)));
  } else if (codepoint <= 0xFFFF) {
    if (codepoint >= 0xD800 && codepoint <= 0xDFFF) return false;
    output.push_back(static_cast<char>(0xE0 | (codepoint >> 12)));
    output.push_back(static_cast<char>(0x80 | ((codepoint >> 6) & 0x3F)));
    output.push_back(static_cast<char>(0x80 | (codepoint & 0x3F)));
  } else if (codepoint <= 0x10FFFF) {
    output.push_back(static_cast<char>(0xF0 | (codepoint >> 18)));
    output.push_back(static_cast<char>(0x80 | ((codepoint >> 12) & 0x3F)));
    output.push_back(static_cast<char>(0x80 | ((codepoint >> 6) & 0x3F)));
    output.push_back(static_cast<char>(0x80 | (codepoint & 0x3F)));
  } else {
    return false;
  }
  return true;
}

inline bool parse_string(
    const std::string& json,
    std::size_t& position,
    std::string& output) {
  if (position >= json.size() || json[position] != '"') return false;
  position++;
  output.clear();
  while (position < json.size()) {
    const unsigned char value = static_cast<unsigned char>(json[position++]);
    if (value == '"') return true;
    if (value < 0x20) return false;
    if (value != '\\') {
      output.push_back(static_cast<char>(value));
      continue;
    }
    if (position >= json.size()) return false;
    const char escaped = json[position++];
    switch (escaped) {
      case '"': output.push_back('"'); break;
      case '\\': output.push_back('\\'); break;
      case '/': output.push_back('/'); break;
      case 'b': output.push_back('\b'); break;
      case 'f': output.push_back('\f'); break;
      case 'n': output.push_back('\n'); break;
      case 'r': output.push_back('\r'); break;
      case 't': output.push_back('\t'); break;
      case 'u': {
        std::uint32_t codepoint = 0;
        if (!unicode_escape(json, position, codepoint)) return false;
        if (codepoint >= 0xD800 && codepoint <= 0xDBFF) {
          if (position + 2 > json.size() || json[position] != '\\'
              || json[position + 1] != 'u') return false;
          position += 2;
          std::uint32_t low = 0;
          if (!unicode_escape(json, position, low) || low < 0xDC00 || low > 0xDFFF)
            return false;
          codepoint = 0x10000 + ((codepoint - 0xD800) << 10) + (low - 0xDC00);
        }
        if (!append_utf8(codepoint, output)) return false;
        break;
      }
      default: return false;
    }
  }
  return false;
}

inline void skip_space(const std::string& json, std::size_t& position) {
  while (position < json.size()
         && std::isspace(static_cast<unsigned char>(json[position]))) position++;
}

enum class KeySearchStatus {
  absent,
  found,
  invalid,
};

inline KeySearchStatus find_key_value(
    const std::string& json,
    const std::string& wanted_key,
    std::size_t& value_position) {
  bool found = false;
  std::size_t position = 0;
  while (position < json.size()) {
    if (json[position] != '"') {
      position++;
      continue;
    }
    std::string token;
    if (!parse_string(json, position, token)) return KeySearchStatus::invalid;
    std::size_t after = position;
    skip_space(json, after);
    if (after >= json.size() || json[after] != ':') continue;
    if (token != wanted_key) continue;
    if (found) return KeySearchStatus::invalid;
    found = true;
    value_position = after + 1;
  }
  return found ? KeySearchStatus::found : KeySearchStatus::absent;
}

}  // namespace emucap_json_strings_detail

inline EmucapJsonStringArrayStatus emucap_json_string_array(
    const std::string& json,
    const char* key,
    std::vector<std::string>& values) {
  values.clear();
  std::size_t position = 0;
  const emucap_json_strings_detail::KeySearchStatus key_status =
      emucap_json_strings_detail::find_key_value(json, key, position);
  if (key_status == emucap_json_strings_detail::KeySearchStatus::absent)
    return EmucapJsonStringArrayStatus::absent;
  if (key_status == emucap_json_strings_detail::KeySearchStatus::invalid)
    return EmucapJsonStringArrayStatus::invalid;
  emucap_json_strings_detail::skip_space(json, position);
  if (position >= json.size() || json[position++] != '[')
    return EmucapJsonStringArrayStatus::invalid;
  emucap_json_strings_detail::skip_space(json, position);
  if (position < json.size() && json[position] == ']')
    return EmucapJsonStringArrayStatus::valid;

  while (position < json.size()) {
    std::string value;
    if (!emucap_json_strings_detail::parse_string(json, position, value)
        || value.empty() || value.size() > 128 || values.size() >= 256)
      return EmucapJsonStringArrayStatus::invalid;
    values.push_back(value);
    emucap_json_strings_detail::skip_space(json, position);
    if (position >= json.size()) return EmucapJsonStringArrayStatus::invalid;
    if (json[position] == ']') return EmucapJsonStringArrayStatus::valid;
    if (json[position++] != ',') return EmucapJsonStringArrayStatus::invalid;
    emucap_json_strings_detail::skip_space(json, position);
    if (position >= json.size() || json[position] == ']')
      return EmucapJsonStringArrayStatus::invalid;
  }
  return EmucapJsonStringArrayStatus::invalid;
}

#endif
