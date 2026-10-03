#ifndef EMUCAP_LIVE_INPUT_H
#define EMUCAP_LIVE_INPUT_H
#include <array>
#include <cstdint>
#include <cstring>
#include <stdexcept>
#include <vector>

// Destination frontend values, separate from device-consumed input saved by the
// core. Stage before external guest load; restore without changing native latches
// or agent ownership. Internal rewind/movie state loading keeps native behavior.
class EmucapLiveInput {
 public:
  struct View { std::uint32_t device; std::uint8_t* data; std::size_t size; };
  using Views = std::array<View, 16>;
  static EmucapLiveInput capture(const void* game, const Views& views) {
    if (!game) throw std::runtime_error("missing live input owner");
    EmucapLiveInput saved;
    saved.game_ = game;
    for (unsigned i = 0; i < views.size(); ++i) {
      const auto& view = views[i];
      if (view.size && !view.data) throw std::runtime_error("missing live input buffer");
      saved.devices_[i] = view.device;
      if (view.size) saved.data_[i].assign(view.data, view.data + view.size);
    }
    return saved;
  }

  bool restore(const void* game, const Views& views) const noexcept {
    if (game != game_) return false;
    // Validate all ports before writing any: loading may reject a configuration
    // change, but it must not partly rewrite the destination's frontend buffers.
    for (unsigned i = 0; i < views.size(); ++i)
      if (views[i].device != devices_[i] || views[i].size != data_[i].size()
          || (views[i].size && !views[i].data)) return false;
    for (unsigned i = 0; i < views.size(); ++i)
      if (views[i].size) std::memcpy(views[i].data, data_[i].data(), views[i].size);
    return true;
  }

 private:
  const void* game_ = nullptr;
  std::array<std::uint32_t, 16> devices_{};
  std::array<std::vector<std::uint8_t>, 16> data_;
};

namespace Mednafen {
EmucapLiveInput emucap_capture_live_input();
bool emucap_restore_live_input(const EmucapLiveInput&) noexcept;
}
#endif
