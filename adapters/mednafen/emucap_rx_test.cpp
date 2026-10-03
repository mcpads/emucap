#include "emucap_rx.h"
#include <cassert>
#include <string>

int main() {
  EmucapReceiveBuffer rx(8);
  std::string line;
  assert(rx.read_limit(8192) == 9);
  rx.append("ab", 2);
  assert(rx.peek(line) == EmucapReceiveBuffer::incomplete);
  assert(rx.read_limit(8192) == 7);
  rx.append("c\nnext\n", 7);
  assert(rx.peek(line) == EmucapReceiveBuffer::ready && line == "abc");
  // A wait may inspect an unsafe command and leave it for the native frame owner.
  assert(rx.peek(line) == EmucapReceiveBuffer::ready && line == "abc");
  rx.consume();
  assert(rx.peek(line) == EmucapReceiveBuffer::ready && line == "next");
  rx.consume();
  assert(rx.peek(line) == EmucapReceiveBuffer::incomplete);

  rx.append("12345678", 8);
  assert(rx.peek(line) == EmucapReceiveBuffer::incomplete && rx.read_limit(8192) == 1);
  rx.append("\n", 1);
  assert(rx.peek(line) == EmucapReceiveBuffer::ready && line == "12345678");
  rx.consume();
  rx.append("123456789", 9);
  assert(rx.peek(line) == EmucapReceiveBuffer::oversized && rx.read_limit(8192) == 0);
  rx.clear();
  assert(rx.peek(line) == EmucapReceiveBuffer::incomplete);
  rx.append("123456789\n", 10);
  assert(rx.peek(line) == EmucapReceiveBuffer::oversized);
  rx.clear();
  rx.append("old\nstale", 9);
  assert(rx.peek(line) == EmucapReceiveBuffer::ready);
  rx.clear();
  assert(rx.peek(line) == EmucapReceiveBuffer::incomplete);

  EmucapReceiveBuffer normal;
  assert(normal.read_limit(8192) == 8192);
  normal.append("\na\nb\n", 5);
  for (const char* expected : {"", "a", "b"}) {
    assert(normal.peek(line) == EmucapReceiveBuffer::ready && line == expected);
    normal.consume();
  }
  assert(normal.peek(line) == EmucapReceiveBuffer::incomplete);
  normal.append("begin\n12345\n", 12);
  assert(normal.peek(line) == EmucapReceiveBuffer::ready && line == "begin");
  normal.consume();
  normal.set_limit(4); // A parent was admitted after the coalesced receive.
  assert(normal.peek(line) == EmucapReceiveBuffer::oversized);
  normal.clear(); normal.append("1234\n", 5);
  assert(normal.peek(line) == EmucapReceiveBuffer::ready);
  normal.consume(); normal.set_limit(8 * 1024 * 1024);
  assert(normal.read_limit(8192) == 8192);
}
