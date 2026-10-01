#pragma once
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

// Managed states carry the interpreter continuation omitted by upstream raw states.
// Header fields are little-endian; the native payload retains its upstream encoding.
constexpr std::size_t EMUCAP_FLYCAST_STATE_HEADER = 24;
inline void emucap_state_put(std::uint8_t* p, std::uint64_t value, unsigned count) {
    for (unsigned i = 0; i < count; ++i) p[i] = static_cast<std::uint8_t>(value >> (8 * i));
}
inline std::uint64_t emucap_state_get(const std::uint8_t* p, unsigned count) {
    std::uint64_t value = 0;
    for (unsigned i = 0; i < count; ++i) value |= std::uint64_t(p[i]) << (8 * i);
    return value;
}
inline void emucap_state_header(std::vector<std::uint8_t>& bytes, std::uint32_t unit,
                                std::uint32_t memory_ops) {
    std::memcpy(bytes.data(), "EMUCAPFC", 8);
    emucap_state_put(bytes.data() + 8, unit, 4);
    emucap_state_put(bytes.data() + 12, memory_ops, 4);
    emucap_state_put(bytes.data() + 16, bytes.size() - EMUCAP_FLYCAST_STATE_HEADER, 8);
}
inline bool emucap_state_parse(const std::vector<std::uint8_t>& bytes,
                               std::uint32_t& unit, std::uint32_t& memory_ops) {
    if (bytes.size() <= EMUCAP_FLYCAST_STATE_HEADER
        || std::memcmp(bytes.data(), "EMUCAPFC", 8) != 0)
        return false;
    const auto parsed_unit = emucap_state_get(bytes.data() + 8, 4);
    const auto parsed_memory = emucap_state_get(bytes.data() + 12, 4);
    if (parsed_unit > 5 || parsed_memory > 4
        || emucap_state_get(bytes.data() + 16, 8) != bytes.size() - EMUCAP_FLYCAST_STATE_HEADER)
        return false;
    unit = static_cast<std::uint32_t>(parsed_unit);
    memory_ops = static_cast<std::uint32_t>(parsed_memory);
    return true;
}
