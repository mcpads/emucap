#include "emucap_input_request.h"
#include <cassert>

int main() {
  using namespace EmucapControl;
  const auto lookup = [](const std::string& name, std::uint16_t& bit) {
    if (name == "a") { bit = 1; return true; }
    if (name == "b" || name == "alias-b") { bit = 2; return true; }
    return false;
  };
  InputRequest output;
  std::string error;
  const auto parse = [&](const char* text, bool timed = false) {
    Json params;
    assert(Parse(text, params));
    return ReadInputRequest(params, timed, 5000, lookup, "test", output, error);
  };
  assert(parse(R"({"port":0,"buttons":["A","alias-b","b"]})") && output.mask == 3);
  assert(parse(R"({"buttons":["\u0061"]})") && output.mask == 1);
  assert(parse(R"({"ignored":{"buttons":["a"],"port":1},"buttons":["b"]})") && output.mask == 2);
  assert(parse(R"({"buttons":[]})") && output.mask == 0);
  assert(parse("{}") && output.mask == 0 && output.frames == 1);
  assert(parse(R"({"buttons":["a"],"frames":5000})", true) && output.frames == 5000);
  assert(parse(R"({"buttons":["a"],"frames":2.0})", true) && output.frames == 2);
  for (const char* invalid : {
      "null", "[]", R"({"port":false})", R"({"port":0.0})", R"({"port":1})",
      R"({"port":-1})", R"({"port":"0"})", R"({"port":null})", R"({"port":{}})",
      R"({"buttons":[1]})", R"({"buttons":[null]})", R"({"buttons":[false]})",
      R"({"buttons":["a",1]})", R"({"buttons":[[]]})", R"({"buttons":[{}]})",
      R"({"buttons":null})", R"({"buttons":"a"})", R"({"buttons":{"other":[]}})",
      R"({"buttons":["unknown"]})", R"({"buttons":["a\u0000"]})",
      R"({"frames":0})", R"({"frames":-1})", R"({"frames":1.5})", R"({"frames":true})",
      R"({"frames":"2"})", R"({"frames":null})", R"({"frames":[]})", R"({"frames":{}})",
      R"({"frames":5001})", R"({"frames":18446744073709551615})"}) {
    output.mask = 0x80; output.frames = 9;
    assert(!parse(invalid, true));
    assert(output.mask == 0x80 && output.frames == 9 && !error.empty());
  }
}
