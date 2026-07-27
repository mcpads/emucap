#pragma once

#include <cstdint>
#include <string>

enum class EmucapNgpMemory {
  invalid,
  ram,
  rom,
  bios,
};

inline EmucapNgpMemory emucap_ngp_memory(const std::string& name) {
  if (name == "ram") return EmucapNgpMemory::ram;
  if (name == "rom") return EmucapNgpMemory::rom;
  if (name == "bios") return EmucapNgpMemory::bios;
  return EmucapNgpMemory::invalid;
}

inline bool emucap_ngp_range_valid(
    std::uint32_t address,
    std::uint32_t length,
    std::uint64_t size) {
  return length != 0
      && static_cast<std::uint64_t>(address) + length <= size;
}

inline bool emucap_ngp_memory_writable(EmucapNgpMemory memory) {
  return memory == EmucapNgpMemory::ram;
}

inline std::uint32_t emucap_ngp_mask_pc(std::uint32_t value) {
  return value & 0xFFFFFFu;
}

inline std::uint8_t emucap_ngp_rfp(std::uint16_t sr) {
  return static_cast<std::uint8_t>((sr >> 8) & 3);
}

enum class EmucapNgpCodeView {
  invalid,
  ram,
  rom,
  bios,
};

struct EmucapNgpCodeLocation {
  EmucapNgpCodeView view;
  std::uint32_t offset;
};

inline EmucapNgpCodeLocation emucap_ngp_code_location(
    std::uint32_t cpu_address,
    std::uint64_t rom_size) {
  const std::uint32_t address = emucap_ngp_mask_pc(cpu_address);
  if (address >= 0x4000 && address < 0x8000)
    return {EmucapNgpCodeView::ram, address - 0x4000};
  if (address >= 0x200000 && address < 0x400000) {
    const std::uint32_t offset = address - 0x200000;
    if (offset < rom_size) return {EmucapNgpCodeView::rom, offset};
  }
  if (address >= 0x800000 && address < 0xA00000) {
    const std::uint32_t offset = 0x200000 + address - 0x800000;
    if (offset < rom_size) return {EmucapNgpCodeView::rom, offset};
  }
  if (address >= 0xFF0000)
    return {EmucapNgpCodeView::bios, address - 0xFF0000};
  return {EmucapNgpCodeView::invalid, 0};
}

inline bool emucap_ngp_safe_code_window(
    std::uint32_t cpu_address,
    std::uint32_t length,
    std::uint64_t rom_size) {
  if (length == 0) return false;
  const EmucapNgpCodeLocation first =
      emucap_ngp_code_location(cpu_address, rom_size);
  if (first.view == EmucapNgpCodeView::invalid) return false;
  const std::uint64_t end =
      static_cast<std::uint64_t>(emucap_ngp_mask_pc(cpu_address)) + length;
  if (end > 0x1000000ULL) return false;
  const EmucapNgpCodeLocation last =
      emucap_ngp_code_location(static_cast<std::uint32_t>(end - 1), rom_size);
  return last.view == first.view
      && static_cast<std::uint64_t>(last.offset)
             == static_cast<std::uint64_t>(first.offset) + length - 1;
}

struct EmucapNgpDecodeState {
  std::uint32_t pc;
  std::uint32_t mem;
  int size;
  std::uint8_t first;
  std::uint8_t second;
  std::uint8_t upper_register;
  std::uint8_t register_code;
  bool register_code_used;
};

class EmucapNgpDecodeRestore {
 public:
  EmucapNgpDecodeRestore(
      std::uint32_t& pc,
      std::uint32_t& mem,
      int& size,
      std::uint8_t& first,
      std::uint8_t& second,
      std::uint8_t& upper_register,
      std::uint8_t& register_code,
      bool& register_code_used)
      : pc_(pc),
        mem_(mem),
        size_(size),
        first_(first),
        second_(second),
        upper_register_(upper_register),
        register_code_(register_code),
        register_code_used_(register_code_used),
        saved_{pc, mem, size, first, second, upper_register, register_code,
               register_code_used} {}

  EmucapNgpDecodeRestore(const EmucapNgpDecodeRestore&) = delete;
  EmucapNgpDecodeRestore& operator=(const EmucapNgpDecodeRestore&) = delete;

  ~EmucapNgpDecodeRestore() {
    pc_ = saved_.pc;
    mem_ = saved_.mem;
    size_ = saved_.size;
    first_ = saved_.first;
    second_ = saved_.second;
    upper_register_ = saved_.upper_register;
    register_code_ = saved_.register_code;
    register_code_used_ = saved_.register_code_used;
  }

 private:
  std::uint32_t& pc_;
  std::uint32_t& mem_;
  int& size_;
  std::uint8_t& first_;
  std::uint8_t& second_;
  std::uint8_t& upper_register_;
  std::uint8_t& register_code_;
  bool& register_code_used_;
  EmucapNgpDecodeState saved_;
};

struct EmucapNgpExecRange {
  std::uint32_t start;
  std::uint32_t end;
};

inline bool emucap_ngp_exec_range_valid(const EmucapNgpExecRange& range) {
  return range.start <= range.end && range.end <= 0xFFFFFF;
}

inline bool emucap_ngp_exec_matches(
    const EmucapNgpExecRange& range,
    std::uint32_t pc) {
  const std::uint32_t masked = emucap_ngp_mask_pc(pc);
  return emucap_ngp_exec_range_valid(range)
      && masked >= range.start
      && masked <= range.end;
}
