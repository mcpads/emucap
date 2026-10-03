#pragma once
#include "emucap_json.hpp"
#include "emucap_control_identity.h"
#include <memory>
#include <cstdint>
#include <set>
#include <string>
#include <vector>

namespace EmucapControl {
using Json = nlohmann::json;

// Validate structure before building a DOM: bounded depth, no duplicate decoded
// keys (including escaped aliases). nlohmann owns JSON/UTF-8/number grammar.
class Structure : public nlohmann::json_sax<Json> {
 public:
  bool null() override { return true; }
  bool boolean(bool) override { return true; }
  bool number_integer(number_integer_t) override { return true; }
  bool number_unsigned(number_unsigned_t) override { return true; }
  bool number_float(number_float_t, const string_t&) override { return true; }
  bool string(string_t&) override { return true; }
  bool binary(binary_t&) override { return false; }
  bool start_object(std::size_t) override { return push(true); }
  bool start_array(std::size_t) override { return push(false); }
  bool end_object() override { return pop(true); }
  bool end_array() override { return pop(false); }
  bool key(string_t& name) override {
    return !frames.empty() && frames.back().object && frames.back().keys.insert(name).second;
  }
  bool parse_error(std::size_t, const std::string&, const nlohmann::detail::exception&) override {
    return false;
  }
 private:
  struct Frame { bool object; std::set<std::string> keys; };
  std::vector<Frame> frames;
  bool push(bool object) {
    if (frames.size() >= 32) return false;
    frames.push_back(Frame{object, {}});
    return true;
  }
  bool pop(bool object) {
    if (frames.empty() || frames.back().object != object) return false;
    frames.pop_back();
    return true;
  }
};

inline bool Parse(const std::string& text, Json& value, std::size_t limit = 8 * 1024 * 1024) {
  value = Json();
  if (text.size() > limit) return false;
  Structure structure;
  if (!Json::sax_parse(text, &structure)) return false;
  value = Json::parse(text, nullptr, false);
  return !value.is_discarded();
}
inline bool IdentityPart(const Json& value) {
  if (!value.is_string()) return false;
  return ValidIdentityPart(value.get_ref<const std::string&>());
}
inline bool ReadKey(const Json& value, Key& key) {
  if (!value.is_object() || value.size() != 3 || !value.contains("runtime") ||
      !value.contains("owner_id") || !value.contains("operation_id") ||
      !IdentityPart(value["runtime"]) || !IdentityPart(value["owner_id"]) ||
      !IdentityPart(value["operation_id"])) return false;
  key = {value["runtime"].get<std::string>(), value["owner_id"].get<std::string>(),
         value["operation_id"].get<std::string>()};
  return true;
}
inline bool ReadAttachment(const Json& value, Attachment& attachment) {
  if (!value.is_object() || value.size() != 3 || !value.contains("broker_instance") ||
      !value.contains("registration") || !value.contains("session") ||
      !IdentityPart(value["broker_instance"]) || !value["registration"].is_number_unsigned() ||
      !value["session"].is_number_unsigned()) return false;
  const auto registration = value["registration"].get<std::uint64_t>();
  const auto session = value["session"].get<std::uint64_t>();
  if (!registration || !session) return false;
  attachment.broker_instance = value["broker_instance"].get<std::string>();
  attachment.registration = registration;
  attachment.session = session;
  return true;
}
struct Request {
  std::uint64_t id = 0;
  std::string method;
  Json params;
};
inline bool ReadRequest(const Json& value, Request& request) {
  if (!value.is_object() || value.size() != 4 || !value.contains("v") ||
      !value["v"].is_number_unsigned() || value["v"] != 1 || !value.contains("id") ||
      !value["id"].is_number_unsigned() || !value.contains("method") ||
      !IdentityPart(value["method"]) || !value.contains("params") ||
      !(value["params"].is_object() || value["params"].is_null())) return false;
  request.id = value["id"].get<std::uint64_t>();
  request.method = value["method"].get<std::string>();
  request.params = value["params"].is_null() ? Json::object() : value["params"];
  return true;
}
struct SessionEvent {
  bool attach = false;
  std::string runtime;
  Attachment attachment;
};
inline bool ReadEvent(const Json& value, SessionEvent& event) {
  if (!value.is_object() || value.size() != 1 || !value.contains("_control_session")) return false;
  const auto& inner = value["_control_session"];
  if (!inner.is_object() || inner.size() != 3 || !inner.contains("kind") ||
      !inner.contains("runtime") || !IdentityPart(inner["runtime"]) ||
      !inner.contains("attachment") || !ReadAttachment(inner["attachment"], event.attachment) ||
      (inner["kind"] != "attach" && inner["kind"] != "detach")) return false;
  event.attach = inner["kind"] == "attach";
  event.runtime = inner["runtime"].get<std::string>();
  return true;
}
struct Scope {
  std::unique_ptr<Key> parent, child;
};
inline bool ReadScope(const Request& request, Scope& scope) {
  scope = Scope();
  const auto& params = request.params;
  if (!params.is_object() || request.method == "_control_session" || params.contains("_control_session")) return false;
  Scope next;
  if (params.contains("_control")) {
    if (request.method != "step" && request.method != "step_instructions") return false;
    next.child.reset(new Key);
    if (!ReadKey(params["_control"], *next.child)) return false;
  }
  const bool parent_method = request.method == "begin_temporal_operation" || request.method == "finish_temporal_operation";
  if (parent_method) {
    if (next.child || !params.contains("parent")) return false;
    next.parent.reset(new Key);
    if (!ReadKey(params["parent"], *next.parent)) return false;
  }
  if (params.contains("_temporal_owner")) {
    Key owner;
    if (!ReadKey(params["_temporal_owner"], owner) || (next.parent && !(owner == *next.parent))) return false;
    next.parent.reset(new Key(owner));
  }
  if (next.child && (!next.parent || !ChildOf(*next.child, *next.parent))) return false;
  scope = std::move(next);
  return true;
}
inline bool Authenticate(Request& request, const Attachment& current, bool broker) {
  if (!current.Valid() || !request.params.is_object()) return false;
  const bool stamped = request.params.contains("_control_attachment");
  if (stamped != broker) return false;
  if (stamped) {
    Attachment observed;
    if (!ReadAttachment(request.params["_control_attachment"], observed) || !(observed == current)) return false;
    request.params.erase("_control_attachment");
  }
  return true;
}
inline Json NativeParams(const Request& request) {
  Json params = request.params;
  params.erase("_temporal_owner");
  params.erase("_control");
  params.erase("_control_attachment");
  return params;
}
// Identify reserved transport/scope metadata before ordinary pacing dispatch.
// Such requests require authenticated ownership handling, never legacy fallback.
inline bool HasControl(const Request& request) {
  return request.method == "_control_session" || request.method == "begin_temporal_operation" ||
      request.method == "finish_temporal_operation" || request.method == "cancel_operation" ||
      request.params.contains("_control_session") || request.params.contains("_control_attachment") ||
      request.params.contains("_temporal_owner") || request.params.contains("_control");
}
} // namespace EmucapControl
