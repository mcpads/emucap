#!/usr/bin/env python3
"""emucap ↔ Flycast(Dreamcast) GDB-스텁 브리지 (PoC).

위치: 이 브리지는 emucap의 둘째 진입점(라이브 제어)을 Dreamcast로 확장하는 PoC다.
- 한쪽: emucap-mcp 서버의 어댑터 리스너에 **TCP 클라이언트**로 접속해 NDJSON을 말한다
  (다른 어댑터와 동일 — 서버가 `{"v":1,"id","method","params"}`를 보내면 `{"id","ok","result|error"}`로 답).
- 다른쪽: Flycast 내장 GDB 스텁(기본 127.0.0.1:3263)에 **GDB-RSP 클라이언트**로 접속해 각 메서드를 번역.

이로써 포크/빌드 없이 Dreamcast에서 emucap 루프(read_memory/get_state/step/BP/pause)를 즉시 증명한다.
GDB 스텁이 못 주는 screenshot/입력/savestate/frame-step은 광고하지 않는다(강등) — 네이티브 포크에서 채운다.

전제: Flycast가 GDB 활성으로 ROM 실행 중이어야 한다(emu.cfg: Debug.GDBEnabled=yes, Debug.GDBPort=3263).

사용: emucap-gdb-bridge.py <EMUCAP_PORT> [GDB_HOST:PORT]
  EMUCAP_PORT = emucap-mcp status의 listening_port. GDB 기본 127.0.0.1:3263.
"""
import json
import os
import socket
import sys
import time


FRONT_WRITE_TIMEOUT = 5.0

# ── Flycast SH-4 GDB 레지스터 순서 (core/debug/debug_agent.h Sh4RegList, 59×u32 LE) ──
# 'g' 응답은 각 레지스터를 8 hex(=4바이트, little-endian)로 이어 붙인다(unpack 로직 = 첫 hex쌍이 LSB).
_REG_NAMES = (
    [f"r{i}" for i in range(16)]                      # 0-15
    + ["pc", "pr", "gbr", "vbr", "mach", "macl", "sr",  # 16-22 (16=nextpc)
       "fpul", "fpscr"]                                 # 23-24
    + [f"fr{i}" for i in range(16)]                     # 25-40
    + ["ssr", "spc"]                                    # 41-42
    + [None] * 8                                        # 43-50 (중복 r0-r7 — 무시)
    + [f"r{i}_bank" for i in range(8)]                  # 51-58
)


class GdbError(Exception):
    pass


class GdbRsp:
    """Flycast GDB 원격 프로토콜(RSP) 최소 클라이언트. ack 모드, 이스케이프(`}`^0x20), RLE 없음."""

    def __init__(self, host, port, timeout=5.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(timeout)
        self.buf = b""

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    @staticmethod
    def _checksum(payload: bytes) -> int:
        return sum(payload) & 0xFF

    def _read_byte(self):
        if not self.buf:
            self.buf = self.sock.recv(4096)
            if not self.buf:
                raise GdbError("GDB 연결 끊김")
        b, self.buf = self.buf[:1], self.buf[1:]
        return b

    def send(self, payload: str, expect_reply=True):
        """패킷 송신 후 ack(+) 수신. expect_reply면 응답 패킷까지 읽어 돌려준다."""
        data = payload.encode()
        frame = b"$" + data + b"#" + f"{self._checksum(data):02x}".encode()
        self.sock.sendall(frame)
        # ack 대기 (+/-). 잡음(앞선 +) 흡수.
        for _ in range(8):
            a = self._read_byte()
            if a == b"+":
                break
            if a == b"-":
                self.sock.sendall(frame)  # 1회 재전송
        return self.recv() if expect_reply else None

    def interrupt(self):
        """Ctrl-C(\x03) 송신 → 정지 응답 패킷 수신(프레이밍 없음, ack 없음)."""
        self.sock.sendall(b"\x03")
        return self.recv()

    def recv(self) -> str:
        """`$<payload>#<cs>` 한 개 수신 → ack(+) 회신 → 이스케이프 해제한 payload 반환."""
        while True:
            b = self._read_byte()
            if b == b"$":
                break
            # 선행 +/- 또는 잡음 무시
        raw = bytearray()
        while True:
            b = self._read_byte()
            if b == b"#":
                break
            raw += b
        self._read_byte(); self._read_byte()  # 체크섬 2자 소비
        self.sock.sendall(b"+")               # 수신 ack
        # 이스케이프 해제: `}` 다음 바이트 ^ 0x20
        out = bytearray()
        i = 0
        while i < len(raw):
            if raw[i] == 0x7D and i + 1 < len(raw):  # '}'
                out.append(raw[i + 1] ^ 0x20)
                i += 2
            else:
                out.append(raw[i])
                i += 1
        return out.decode("latin-1")

    def recv_nonblocking(self):
        """비차단: 정지 응답이 이미 도착해 있으면 그 패킷을, 없으면 None(BP 히트 폴링용)."""
        self.sock.setblocking(False)
        try:
            chunk = self.sock.recv(4096)
        except (BlockingIOError, OSError):
            chunk = b""
        finally:
            self.sock.settimeout(5.0)
        if not chunk and not self.buf:
            return None
        self.buf += chunk
        if b"$" not in self.buf:
            return None
        return self.recv()


def _need(p, key):
    if key not in p or p[key] is None:
        raise GdbError(f"{key} 필요")
    return p[key]


class Bridge:
    def __init__(self, gdb: GdbRsp):
        self.gdb = gdb
        self.frozen = False
        self.bps = {}        # id -> addr
        self.next_bp = 1
        self.events = []     # poll_events 드레인용

    # ── 핸들러: (result_dict) 반환 또는 GdbError ──
    def hello(self, p):
        result = {
            "protocol_version": 1,
            "name": os.environ.get("EMUCAP_NAME") or "flycast",
            "system": "dreamcast",
            "adapter": "flycast-gdb",
            "methods": [
                "read_memory", "write_memory", "get_state", "status",
                "pause", "resume", "step", "set_breakpoint",
                "clear_breakpoint", "list_breakpoints", "poll_events",
            ],
        }
        if token := os.environ.get("EMUCAP_SESSION_TOKEN"):
            result["session_token"] = token
        if content := os.environ.get("EMUCAP_CONTENT"):
            result["content"] = content
        if launch_id := os.environ.get("EMUCAP_LAUNCH_ID"):
            result["launch_id"] = launch_id
        return result

    def status(self, p):
        self._drain_stop()  # BP 히트로 이미 멈췄으면 반영
        return {"connected": True, "state": "frozen" if self.frozen else "running",
                "adapter": "flycast-gdb"}

    def read_memory(self, p):
        addr = int(_need(p, "address")); length = int(_need(p, "length"))
        r = self.gdb.send(f"m{addr:x},{length:x}")
        if r.startswith("E"):
            raise GdbError(f"GDB m 실패: {r}")
        return {"hex": r}

    def write_memory(self, p):
        addr = int(_need(p, "address")); hexstr = _need(p, "hex")
        if len(hexstr) % 2 != 0:
            raise GdbError("hex는 짝수 길이여야")
        n = len(hexstr) // 2
        r = self.gdb.send(f"M{addr:x},{n:x}:{hexstr}")
        if r != "OK":
            raise GdbError(f"GDB M 실패: {r}")
        return {"written": n}

    def get_state(self, p):
        groups = p.get("groups") or []
        if groups and "cpu" not in groups:
            return {"state": {}}
        r = self.gdb.send("g")
        if r.startswith("E") or len(r) < 8:
            raise GdbError(f"GDB g 실패: {r}")
        state = {}
        for i, name in enumerate(_REG_NAMES):
            off = i * 8
            if name is None or off + 8 > len(r):
                continue
            val = int.from_bytes(bytes.fromhex(r[off:off + 8]), "little")
            state[f"cpu.{name}"] = val
        return {"state": state}

    def pause(self, p):
        if not self.frozen:
            stop = self.gdb.interrupt()
            self._note_stop(stop)
            self.frozen = True
        return {"state": "frozen"}

    def resume(self, p):
        if self.frozen:
            self.gdb.send("c", expect_reply=False)  # 실행 — 정지응답은 BP 히트 시 비동기로
            self.frozen = False
        return {"state": "running"}

    def step(self, p):
        if not self.frozen:
            raise GdbError("step은 frozen에서만 — 먼저 pause")
        stop = self.gdb.send("s")
        self._note_stop(stop)
        return {"status": "completed"}

    def set_breakpoint(self, p):
        kind = p.get("kind", "exec")
        if kind != "exec":
            raise GdbError("DC PoC는 exec(소프트웨어) BP만 지원 — GDB 스텁 한계(HW watch 없음)")
        addr = int(_need(p, "start"))
        r = self.gdb.send(f"Z0,{addr:x},2")
        if r != "OK":
            raise GdbError(f"GDB Z0 실패: {r}")
        bid = self.next_bp; self.next_bp += 1
        self.bps[bid] = addr
        return {"id": bid}

    def clear_breakpoint(self, p):
        bid = int(_need(p, "id"))
        addr = self.bps.get(bid)
        if addr is None:
            raise GdbError("그런 breakpoint 없음")
        self.gdb.send(f"z0,{addr:x},2")
        del self.bps[bid]
        return {"cleared": bid}

    def list_breakpoints(self, p):
        return {"breakpoints": [{"id": i, "kind": "exec", "start": a, "end": a}
                                for i, a in self.bps.items()]}

    def poll_events(self, p):
        self._drain_stop()
        ev, self.events = self.events, []
        return {"events": ev, "dropped": 0}

    # ── 정지 응답 처리 ──
    def _note_stop(self, stop):
        """정지 응답(S05/T05..)에서 PC를 뽑아 이벤트로. T 응답엔 reg:val; 쌍이 온다."""
        pc = None
        if stop and stop[0] == "T":
            for pair in stop[3:].split(";"):
                if ":" in pair:
                    k, v = pair.split(":", 1)
                    # SH-4 PC 레지스터 번호 = 16 (Sh4RegList nextpc)
                    if k.lower() in ("10", "pc"):  # 0x10 = 16
                        try:
                            pc = int.from_bytes(bytes.fromhex(v), "little")
                        except ValueError:
                            pass
        return pc

    def _drain_stop(self):
        """실행 중(frozen=False) BP 히트로 비동기 정지응답이 왔는지 확인 → frozen 전환 + 이벤트."""
        if self.frozen:
            return
        stop = self.gdb.recv_nonblocking()
        if stop and stop[0] in ("S", "T"):
            self.frozen = True
            pc = self._note_stop(stop)
            self.events.append({"type": "breakpoint_hit", "pc": pc, "signal": stop[1:3]})


def main():
    if len(sys.argv) < 2:
        print(__doc__); sys.exit(2)
    emucap_port = int(sys.argv[1])
    gdb_host, gdb_port = "127.0.0.1", 3263
    if len(sys.argv) >= 3 and ":" in sys.argv[2]:
        gdb_host, gp = sys.argv[2].rsplit(":", 1); gdb_port = int(gp)

    gdb = GdbRsp(gdb_host, gdb_port)
    bridge = Bridge(gdb)
    print(f"[bridge] GDB {gdb_host}:{gdb_port} 접속됨. emucap-mcp 127.0.0.1:{emucap_port} 연결…",
          file=sys.stderr)

    retry = 0.05
    while True:
        try:
            s = socket.create_connection(("127.0.0.1", emucap_port), timeout=1.0)
            s.settimeout(None)
            print("[bridge] emucap-mcp 연결됨. 요청 대기.", file=sys.stderr)
            _serve_emucap_session(s, bridge)
            print("[bridge] emucap-mcp 연결 종료. 재연결 중.", file=sys.stderr)
            retry = 0.05
        except OSError as err:
            print(f"[bridge] emucap-mcp 미가용({err}). 재시도.", file=sys.stderr)
        time.sleep(retry)
        retry = min(retry * 2.0, 2.0)


def _serve_emucap_session(sock, bridge):
    with sock, sock.makefile("rwb", buffering=0) as f:
      for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            env = json.loads(line)
        except json.JSONDecodeError:
            continue
        rid = env.get("id", 0)
        method = env.get("method", "")
        params = env.get("params") or {}
        h = getattr(bridge, method, None)
        if h is None:
            resp = {"id": rid, "ok": False,
                    "error": {"kind": "unknown_method", "message": method}}
        else:
            try:
                resp = {"id": rid, "ok": True, "result": h(params)}
            except GdbError as e:
                resp = {"id": rid, "ok": False,
                        "error": {"kind": "emulator_error", "message": str(e)}}
            except Exception as e:  # noqa: BLE001 — PoC: 모든 예외를 에러로 보고
                resp = {"id": rid, "ok": False,
                        "error": {"kind": "bridge_error", "message": repr(e)}}
        payload = (json.dumps(resp, separators=(",", ":")) + "\n").encode()
        if not _write_front_response(sock, payload):
            return


def _write_front_response(sock, payload):
    """Bound a bridge-to-MCP write without turning idle reads into periodic timeouts."""
    ok = False
    try:
        sock.settimeout(FRONT_WRITE_TIMEOUT)
        sock.sendall(payload)
        ok = True
    except OSError:
        pass
    finally:
        try:
            sock.settimeout(None)
        except OSError:
            ok = False
    return ok


if __name__ == "__main__":
    main()
