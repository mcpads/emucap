#pragma once
#include <cstdint>
#include <string>
#include <utility>

namespace EmucapControl {
inline bool ValidIdentityPart(const std::string& value) {
  return !value.empty() && value.size() <= 256 && value.find('\0') == std::string::npos;
}
struct Key {
  std::string runtime, owner_id, operation_id;
  bool Valid() const {
    return ValidIdentityPart(runtime) && ValidIdentityPart(owner_id) && ValidIdentityPart(operation_id);
  }
  bool operator==(const Key& other) const {
    return runtime == other.runtime && owner_id == other.owner_id && operation_id == other.operation_id;
  }
};
struct Attachment {
  std::string broker_instance;
  std::uint64_t registration, session;
  Attachment(std::string broker = {}, std::uint64_t registration_id = 0, std::uint64_t session_id = 0)
      : broker_instance(std::move(broker)), registration(registration_id), session(session_id) {}
  bool Valid() const {
    return ValidIdentityPart(broker_instance) && registration != 0 && session != 0;
  }
  bool operator==(const Attachment& other) const {
    return broker_instance == other.broker_instance && registration == other.registration && session == other.session;
  }
};
inline bool ChildOf(const Key& child, const Key& parent) {
  return child.Valid() && parent.Valid() && child.runtime == parent.runtime &&
      child.owner_id == parent.owner_id && child.operation_id != parent.operation_id;
}
} // namespace EmucapControl
