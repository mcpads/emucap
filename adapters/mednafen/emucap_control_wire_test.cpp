#include "emucap_control_wire.h"
#include <cassert>
#include <limits>

int main() {
  using namespace EmucapControl;
  Json value;
  for (const char* invalid : {
      R"({"id":1,"id":2})", R"({"id":1,"\u0069d":2})",
      R"({"params":{"port":0,"port":1}})", R"([{"a":0,"a":1}])",
      R"({"v":1} {})", R"({"x":"\uD800"})", R"({"x":NaN})",
      R"({"x":01})", R"({"x":1e9999})", R"({"x":true,})"}) {
    assert(!Parse(invalid, value));
  }
  assert(!Parse(std::string("\"\xff\"", 3), value));
  assert(!Parse(std::string(33, '[') + "0" + std::string(33, ']'), value));
  assert(Parse(std::string(32, '[') + "0" + std::string(32, ']'), value));
  assert(Parse("{\"a\":1}", value, 7));
  assert(!Parse("{\"a\":1} ", value, 7));
  assert(Parse(R"([{"a":1},{"a":2}])", value));

  Attachment attachment;
  assert(Parse(R"({"broker_instance":"broker","registration":18446744073709551615,"session":9007199254740993})", value));
  assert(ReadAttachment(value, attachment));
  assert(attachment.registration == std::numeric_limits<std::uint64_t>::max());
  assert(attachment.session == 9007199254740993ULL);
  for (const char* number : {"0", "-1", "1.0", "1e0", "18446744073709551616", "\"1\"", "true"}) {
    assert(Parse(std::string("{\"broker_instance\":\"b\",\"registration\":") + number + ",\"session\":1}", value));
    assert(!ReadAttachment(value, attachment));
  }
  Key key;
  assert(Parse(R"({"runtime":"r","owner_id":"o","operation_id":"p"})", value));
  assert(ReadKey(value, key));
  value["runtime"] = std::string("r\0other", 7);
  assert(!ReadKey(value, key));
  value["runtime"] = std::string(257, 'r');
  assert(!ReadKey(value, key));

  Request request;
  assert(Parse(R"({"params":null,"method":"status","id":18446744073709551615,"v":1})", value));
  assert(ReadRequest(value, request));
  assert(request.id == std::numeric_limits<std::uint64_t>::max());
  assert(request.params.is_object() && !HasControl(request));
  for (const char* field : {"_control", "_temporal_owner", "_control_session", "_control_attachment"}) {
    request.params = Json::object(); request.params[field] = nullptr;
    assert(HasControl(request));
  }
  value["id"] = 1.0;
  assert(!ReadRequest(value, request));
  assert(Parse(R"({"params":{"method":"reset","id":99},"id":1,"method":"status","v":1})", value));
  assert(ReadRequest(value, request) && request.method == "status" && request.id == 1);
  value["_control_attachment"] = Json::object();
  assert(!ReadRequest(value, request));

  const Json parent = {{"runtime", "runtime"}, {"owner_id", "owner"}, {"operation_id", "parent"}};
  Json child = parent; child["operation_id"] = "child";
  request.method = "step";
  request.params = {{"_control", child}, {"_temporal_owner", parent}, {"frames", 4}};
  Scope scope;
  assert(ReadScope(request, scope) && scope.parent && scope.child);
  const Json native = NativeParams(request);
  assert(native == Json({{"frames", 4}}));
  request.params.erase("_temporal_owner");
  assert(!ReadScope(request, scope) && !scope.parent && !scope.child);
  request.params["_temporal_owner"] = child;
  assert(!ReadScope(request, scope)); // Parent and child IDs must be distinct.
  request.params["_temporal_owner"] = parent;
  request.params["_control"]["owner_id"] = "another";
  assert(!ReadScope(request, scope));
  request.params["_control"] = child;
  request.method = "resume";
  assert(!ReadScope(request, scope));
  request.method = "step_instructions";
  assert(ReadScope(request, scope));
  request.method = "begin_temporal_operation";
  assert(!ReadScope(request, scope));
  request.params = {{"parent", parent}, {"_temporal_owner", child}};
  assert(!ReadScope(request, scope));
  request.params["_temporal_owner"] = parent;
  assert(ReadScope(request, scope) && scope.parent && !scope.child);

  const Attachment current("broker", std::numeric_limits<std::uint64_t>::max(), 9007199254740993ULL);
  const Json stamp = {{"broker_instance", current.broker_instance},
      {"registration", current.registration}, {"session", current.session}};
  assert(!Authenticate(request, current, true));
  assert(Authenticate(request, current, false));
  request.params["_control_attachment"] = stamp;
  assert(!Authenticate(request, current, false));
  assert(!Authenticate(request, Attachment("broker", current.registration, current.session + 1), true));
  assert(request.params["_control_attachment"] == stamp); // Failure cannot sanitize an unauthenticated request.
  assert(Authenticate(request, current, true) && !request.params.contains("_control_attachment"));
  assert(!Authenticate(request, Attachment(), false));

  SessionEvent event;
  value = {{"_control_session", {{"kind", "attach"}, {"runtime", "runtime"}, {"attachment", stamp}}}};
  assert(ReadEvent(value, event) && event.attach && event.attachment == current);
  value["_control_session"]["kind"] = "detach";
  assert(ReadEvent(value, event) && !event.attach);
  value["id"] = 1;
  assert(!ReadEvent(value, event));
  value.erase("id"); value["_control_session"]["extra"] = 1;
  assert(!ReadEvent(value, event));
}
