#include "emucap_state.h"
#include <cassert>
#include <cstdio>
int main() {
    for (unsigned unit = 0; unit <= 5; ++unit) for (unsigned memory = 0; memory <= 4; ++memory) {
        std::vector<std::uint8_t> bytes(EMUCAP_FLYCAST_STATE_HEADER + 32, 0x79);
        emucap_state_header(bytes, unit, memory);
        std::uint32_t u = 99, m = 99;
        assert(emucap_state_parse(bytes, u, m) && u == unit && m == memory);
        for (unsigned i = EMUCAP_FLYCAST_STATE_HEADER; i < bytes.size(); ++i) assert(bytes[i] == 0x79);
        for (unsigned length = 0; length < bytes.size(); ++length) {
            auto short_bytes = bytes; short_bytes.resize(length); u = m = 99;
            assert(!emucap_state_parse(short_bytes, u, m) && u == 99 && m == 99);
        }
        for (unsigned offset : {0u, 8u, 12u, 16u}) {
            auto bad = bytes; bad[offset] = 0xff; u = m = 99;
            assert(!emucap_state_parse(bad, u, m) && u == 99 && m == 99);
        }
        bytes.push_back(0); u = m = 99;
        assert(!emucap_state_parse(bytes, u, m) && u == 99 && m == 99);
    }
    std::puts("FLYCAST STATE ENVELOPE PASSED");
}
