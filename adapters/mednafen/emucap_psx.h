#pragma once
#include <array>
#include <cstdint>

struct EmucapPsxPeekWindow {
  std::uint64_t address;
  std::uint64_t length;
};

// Native PSX MemPeek implements system control at 1f801000..1f801023.
// The rest of its device aperture is TODO. CPU PeekMemory maps KSEG0/1 aliases.
inline std::array<EmucapPsxPeekWindow, 4> emucap_psx_cpu_peek_windows() {
  return {{{0, 0x1f801024ULL},
           {0x1f803000ULL, 0x9f801024ULL - 0x1f803000ULL},
           {0x9f803000ULL, 0xbf801024ULL - 0x9f803000ULL},
           {0xbf803000ULL, 0x100000000ULL - 0xbf803000ULL}}};
}

inline bool emucap_psx_cpu_peek_range(std::uint64_t address, std::uint64_t length) {
  if (!length || address >= 0x100000000ULL || length > 0x100000000ULL - address)
    return false;
  for (const auto& window : emucap_psx_cpu_peek_windows()) {
    if (address >= window.address && address - window.address < window.length
        && length <= window.length - (address - window.address)) return true;
  }
  return false;
}
