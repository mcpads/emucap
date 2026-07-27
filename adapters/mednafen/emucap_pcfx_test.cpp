#include "emucap_pcfx.h"

#include <cassert>

int main() {
  assert(emucap_v810_classify(0xAC00) == EmucapV810CallKind::call);
  assert(emucap_v810_classify(0x181F) == EmucapV810CallKind::return_from_call);
  assert(emucap_v810_classify(0x1803) == EmucapV810CallKind::other);

  EmucapPcfxAuxRange range{};
  assert(emucap_pcfx_aux_range("kram0", 0, 1, range)
         == EmucapPcfxAuxMapStatus::mapped);
  assert(range.start == 0 && range.end == 0);
  assert(emucap_pcfx_aux_range("kram1", 0x7FFFE, 0x7FFFF, range)
         == EmucapPcfxAuxMapStatus::mapped);
  assert(range.start == 0x7FFFF && range.end == 0x7FFFF);
  assert(emucap_pcfx_aux_range("vdcvram0", 0, 3, range)
         == EmucapPcfxAuxMapStatus::mapped);
  assert(range.start == 0x80000 && range.end == 0x80001);
  assert(emucap_pcfx_aux_range("vdcvram1", 1, 2, range)
         == EmucapPcfxAuxMapStatus::unaligned);
  assert(emucap_pcfx_aux_range("vdcvram1", 0, 0x20001, range)
         == EmucapPcfxAuxMapStatus::out_of_range);

  std::string memory_type;
  std::uint32_t address = 0;
  std::uint32_t length = 0;
  assert(emucap_pcfx_aux_public_range(
      0x40002, 2, memory_type, address, length));
  assert(memory_type == "kram1" && address == 4 && length == 4);
  assert(emucap_pcfx_aux_public_range(
      0x90001, 1, memory_type, address, length));
  assert(memory_type == "vdcvram1" && address == 2 && length == 2);
  assert(!emucap_pcfx_aux_public_range(
      0xA0000, 1, memory_type, address, length));

  std::vector<EmucapSnapshotSpec> snapshots;
  std::string error;
  assert(emucap_parse_snapshot_specs(
             R"({"snapshot":["ram:0x40:4","kram0:$20:2"]})",
             snapshots,
             error)
         == EmucapSnapshotParseStatus::valid);
  assert(snapshots.size() == 2);
  assert(snapshots[0].memory_type == "ram"
         && snapshots[0].address == 0x40
         && snapshots[0].length == 4);
  assert(snapshots[1].memory_type == "kram0"
         && snapshots[1].address == 0x20
         && snapshots[1].length == 2);
  assert(emucap_parse_snapshot_specs(
             R"({"snapshot":["ram:0x40:0"]})",
             snapshots,
             error)
         == EmucapSnapshotParseStatus::invalid);
  assert(emucap_parse_snapshot_specs(
             R"({"snapshot":["ram:0x40:4",]})",
             snapshots,
             error)
         == EmucapSnapshotParseStatus::invalid);
  return 0;
}
