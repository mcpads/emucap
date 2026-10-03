// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include "EmuCapOwner.h"
#include <json.hpp> // Pinned Dolphin tinygltf dependency; preserves unsigned 64-bit wire identities.

namespace EmuCap { inline constexpr size_t MAX_NDJSON_FRAME_BYTES = 8 * 1024 * 1024; }

namespace EmuCap::Temporal
{
using Json = nlohmann::json;
inline bool ReadKey(const Json& value, OperationKey& key)
{
  if (!value.is_object() || value.size() != 3 || !value.contains("runtime") ||
      !value.contains("owner_id") || !value.contains("operation_id") ||
      !value["runtime"].is_string() || !value["owner_id"].is_string() ||
      !value["operation_id"].is_string()) return false;
  key = {value["runtime"].get<std::string>(), value["owner_id"].get<std::string>(),
         value["operation_id"].get<std::string>()};
  return key.Valid();
}
inline bool ReadAttachment(const Json& value, Attachment& attachment)
{
  if (!value.is_object() || value.size() != 3 || !value.contains("broker_instance") ||
      !value.contains("registration") || !value.contains("session") ||
      !value["broker_instance"].is_string() || !value["registration"].is_number_unsigned() ||
      !value["session"].is_number_unsigned()) return false;
  attachment = {value["broker_instance"].get<std::string>(), value["registration"].get<uint64_t>(),
                value["session"].get<uint64_t>()};
  return attachment.Valid();
}
inline Json KeyJson(const OperationKey& key)
{
  return {{"runtime", key.runtime}, {"owner_id", key.owner_id}, {"operation_id", key.operation_id}};
}
inline Json TerminalJson(const CleanupPlan& plan)
{
  Json result = {{"status", "completed"}, {"parent", KeyJson(plan.key)},
                 {"cleanup_verified", true}, {"released_ports", plan.input_ports},
                 {"effects_started", plan.effects_started}};
  if (plan.effects_started) result["state"] = "frozen";
  return result;
}
inline Json Error(uint64_t id, const char* kind, const std::string& message)
{
  return {{"id", id}, {"ok", false}, {"error", {{"kind", kind}, {"message", message}}}};
}
inline Json Success(uint64_t id, Json result)
{
  return {{"id", id}, {"ok", true}, {"result", std::move(result)}};
}
inline const char* OwnerError(OwnerResult result)
{
  switch (result)
  {
  case OwnerResult::Busy: return "busy";
  case OwnerResult::InvalidIdentity: return "bad_params";
  case OwnerResult::WrongAttachment: return "wrong_attachment";
  case OwnerResult::NotActive: return "not_active";
  case OwnerResult::Retired: return "bad_state";
  case OwnerResult::Ok: return "ok";
  }
  return "bad_state";
}
struct Request
{
  uint64_t id = 0;
  std::string method;
  Json params;
  std::optional<OperationKey> child, parent;
};
inline bool ReadRequest(const Json& value, Request& request)
{
  request = {};
  if (!value.is_object() || value.contains("_control_session") ||
      value.contains("_control_attachment") || !value.contains("v") || !value["v"].is_number_unsigned() || value["v"] != 1 ||
      !value.contains("id") || !value["id"].is_number_unsigned() ||
      !value.contains("method") || !value["method"].is_string() ||
      !value.contains("params") || !(value["params"].is_object() || value["params"].is_null()))
    return false;
  request.id = value["id"].get<uint64_t>();
  request.method = value["method"].get<std::string>();
  request.params = value["params"].is_null() ? Json::object() : value["params"];
  if (request.method == "_control_session" || request.params.contains("_control_session"))
    return false;
  if (request.params.contains("_control"))
  {
    OperationKey key;
    if (request.method != "step" || !ReadKey(request.params["_control"], key)) return false;
    request.child = key;
  }
  if (request.method == "begin_temporal_operation" || request.method == "finish_temporal_operation")
  {
    if (request.child || !request.params.contains("parent")) return false;
    if (request.params.contains("_temporal_owner") &&
        request.params["_temporal_owner"] != request.params["parent"]) return false;
  }
  const char* field = request.params.contains("_temporal_owner") ? "_temporal_owner" :
      (request.method == "begin_temporal_operation" || request.method == "finish_temporal_operation") ?
          "parent" : nullptr;
  if (field)
  {
    OperationKey key;
    if (!request.params.contains(field) || !ReadKey(request.params[field], key)) return false;
    request.parent = key;
  }
  return true;
}
struct SessionEvent
{
  bool attach = false;
  std::string runtime;
  Attachment attachment;
};
inline bool ReadEvent(const Json& value, SessionEvent& event)
{
  if (!value.is_object() || value.size() != 1 || !value.contains("_control_session")) return false;
  const auto& inner = value["_control_session"];
  if (!inner.is_object() || inner.size() != 3 || !inner.contains("kind") ||
      !inner.contains("runtime") || !inner["runtime"].is_string() ||
      !inner.contains("attachment") || !ReadAttachment(inner["attachment"], event.attachment))
    return false;
  if (inner["kind"] != "attach" && inner["kind"] != "detach") return false;
  event.attach = inner["kind"] == "attach";
  event.runtime = inner["runtime"].get<std::string>();
  return OperationKey::ValidPart(event.runtime);
}
inline bool Authenticate(Request& request, const Attachment& current, bool broker)
{
  const bool stamped = request.params.contains("_control_attachment");
  if (stamped != broker) return false;
  if (!stamped) return true;
  Attachment observed;
  if (!ReadAttachment(request.params["_control_attachment"], observed) || observed != current)
    return false;
  request.params.erase("_control_attachment");
  return true;
}
} // namespace EmuCap::Temporal
