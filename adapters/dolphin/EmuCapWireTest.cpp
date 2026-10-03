// Copyright 2026 emucap
// SPDX-License-Identifier: GPL-2.0-or-later
#include "EmuCapWire.h"
#include <cassert>
#include <limits>
int main()
{
  using namespace EmuCap::Temporal;
  Attachment a;
  auto value = Json::parse(R"({"broker_instance":"b","registration":18446744073709551615,"session":9007199254740993})", nullptr, false);
  assert(ReadAttachment(value, a));
  assert(a.registration == std::numeric_limits<uint64_t>::max());
  assert(a.session == 9007199254740993ULL);
  for (const char* number : {"-1", "1.0", "18446744073709551616"})
  {
    auto invalid = Json::parse(std::string("{\"broker_instance\":\"b\",\"registration\":") + number + ",\"session\":1}", nullptr, false);
    assert(!ReadAttachment(invalid, a));
  }
  Request request;
  auto wire = Json::parse(R"({"v":1,"id":18446744073709551615,"method":"status","params":null})", nullptr, false);
  assert(ReadRequest(wire, request));
  assert(request.id == std::numeric_limits<uint64_t>::max());
  assert(Authenticate(request, a, false));
  request.params["_control_attachment"] = value;
  assert(!Authenticate(request, a, false));
  assert(Authenticate(request, a, true));
  assert(!Authenticate(request, a, true));
  Json parent = {{"runtime","runtime"},{"owner_id","owner"},{"operation_id","parent"}};
  Json admission = Json::parse(R"({"v":1,"id":1,"method":"begin_temporal_operation","params":{}})", nullptr, false);
  admission["params"] = {{"parent",parent},{"_temporal_owner",parent}};
  assert(ReadRequest(admission, request));
  admission["params"]["_temporal_owner"]["operation_id"] = "other";
  assert(!ReadRequest(admission, request));
  wire["_control_attachment"] = value;
  assert(!ReadRequest(wire, request));
}
