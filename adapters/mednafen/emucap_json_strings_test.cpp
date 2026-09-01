#include "emucap_json_strings.h"

#include <cassert>
#include <string>
#include <vector>

int main() {
  std::vector<std::string> values;

  assert(emucap_json_string_array(R"({"params":{}})", "groups", values)
         == EmucapJsonStringArrayStatus::absent);
  assert(values.empty());

  assert(emucap_json_string_array(
             R"({"params":{"groups":[]}})", "groups", values)
         == EmucapJsonStringArrayStatus::valid);
  assert(values.empty());

  assert(emucap_json_string_array(
             R"({"params":{"groups":["CPU", "Misc", "\u00670"]}})",
             "groups",
             values)
         == EmucapJsonStringArrayStatus::valid);
  assert((values == std::vector<std::string>{"CPU", "Misc", "g0"}));

  const std::string invalid[] = {
      R"({"params":{"groups":"CPU"}})",
      R"({"params":{"groups":[1]}})",
      R"({"params":{"groups":[""]}})",
      R"({"params":{"groups":["CPU",]}})",
      R"({"params":{"groups":["CPU" "Misc"]}})",
      R"({"params":{"groups":["\uD800"]}})",
      R"({"groups":[],"params":{"groups":["CPU"]}})",
  };
  for (const std::string& json : invalid) {
    assert(emucap_json_string_array(json, "groups", values)
           == EmucapJsonStringArrayStatus::invalid);
  }

  // A key-shaped substring inside a JSON string is not a selector.
  assert(emucap_json_string_array(
             R"({"note":"\"groups\":[\"CPU\"]","params":{}})",
             "groups",
             values)
         == EmucapJsonStringArrayStatus::absent);

  return 0;
}
