#!/usr/bin/env python3
"""emucap ↔ Dolphin(GameCube/Wii) GDB-스텁 브리지.

flycast 어댑터의 GDB 브리지를 PowerPC(Gekko/Broadway)로 이식한 것. Dolphin은
내장 GDB 스텁을 가지므로 포크/소스빌드 없이 emucap 라이브 제어 루프를 GameCube에서
바로 쓴다.

- 한쪽: emucap-mcp 서버의 어댑터 리스너에 **TCP 클라이언트**로 접속해 NDJSON을 말한다
  (서버가 `{"v":1,"id","method","params"}`를 보내면 `{"id","ok","result|error"}`로 답).
- 다른쪽: Dolphin 내장 GDB 스텁에 **GDB-RSP 클라이언트**로 접속해 각 메서드를 번역.

GDB 스텁이 못 주는 screenshot/입력/savestate/frame-step은 광고하지 않는다(강등).

전제: Dolphin이 GDB 스텁 활성으로 ROM 실행 중이어야 한다.
  Dolphin.ini[General] GDBPort=<port>, Core.CPUCore=CachedInterpreter(=5, JIT는 GDB 미지원).
  GDBPort가 켜지면 Dolphin은 부팅 직후 디버거 연결을 기다리며 멈춘다.

사용: emucap-gdb-bridge.py <EMUCAP_PORT> [GDB_HOST:PORT]
  EMUCAP_PORT = emucap-mcp status의 listening_port. GDB 기본 127.0.0.1:2159.

PowerPC(빅엔디안) 주의: 레지스터 값은 big-endian으로 파싱한다.
"""
import json
import os
import socket
import sys
import time


FRONT_WRITE_TIMEOUT = 5.0

# ── Gekko/Broadway (GameCube/Wii PPC) GDB 레지스터 순서 ──
# gdb powerpc 타깃 규약: r0-r31(32×4B), f0-f31(32×8B), pc, msr(ps), cr, lr, ctr, xer, fpscr.
# 값은 빅엔디안. 'g' 응답은 각 4B 레지스터=8hex, 8B(FPR)=16hex를 이어 붙인다.
_REG_LAYOUT = (
    [(f"r{i}", 4) for i in range(32)]
    + [(f"f{i}", 8) for i in range(32)]
    + [("pc", 4), ("msr", 4), ("cr", 4), ("lr", 4), ("ctr", 4), ("xer", 4), ("fpscr", 4)]
)
# T-정지응답의 PC 레지스터 번호 = 64(0x40) (32 GPR + 32 FPR 다음).
_PC_REGNUM = 0x40


class GdbError(Exception):
    pass


class GdbRsp:
    """GDB 원격 프로토콜(RSP) 최소 클라이언트. ack 모드, 이스케이프(`}`^0x20), RLE 없음."""

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
        data = payload.encode()
        frame = b"$" + data + b"#" + f"{self._checksum(data):02x}".encode()
        self.sock.sendall(frame)
        for _ in range(8):
            a = self._read_byte()
            if a == b"+":
                break
            if a == b"-":
                self.sock.sendall(frame)
        return self.recv() if expect_reply else None

    def interrupt(self):
        self.sock.sendall(b"\x03")
        return self.recv()

    def recv(self) -> str:
        while True:
            b = self._read_byte()
            if b == b"$":
                break
        raw = bytearray()
        while True:
            b = self._read_byte()
            if b == b"#":
                break
            raw += b
        self._read_byte(); self._read_byte()
        self.sock.sendall(b"+")
        out = bytearray()
        i = 0
        while i < len(raw):
            if raw[i] == 0x7D and i + 1 < len(raw):
                out.append(raw[i + 1] ^ 0x20)
                i += 2
            else:
                out.append(raw[i])
                i += 1
        return out.decode("latin-1")

    def recv_nonblocking(self):
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
        self.frozen = True  # GDBPort 활성 Dolphin은 연결 시 정지 상태로 대기
        self.bps = {}
        self.next_bp = 1
        self.events = []

    def hello(self, p):
        result = {
            "protocol_version": 1,
            "name": os.environ.get("EMUCAP_NAME") or "dolphin",
            "system": "gc",
            "adapter": "dolphin-gdb",
            "methods": [
                "read_memory", "write_memory", "get_state", "status",
                "pause", "resume", "step", "set_breakpoint",
                "clear_breakpoint", "list_breakpoints", "poll_events",
            ],
            # GameCube MEM1. address는 절대 EA(0x80000000 기반, 디스어셈블에 보이는 그대로).
            "memory_types": ["main"],
        }
        if token := os.environ.get("EMUCAP_SESSION_TOKEN"):
            result["session_token"] = token
        if content := os.environ.get("EMUCAP_CONTENT"):
            result["content"] = content
        if launch_id := os.environ.get("EMUCAP_LAUNCH_ID"):
            result["launch_id"] = launch_id
        return result

    def status(self, p):
        self._drain_stop()
        return {"connected": True, "state": "frozen" if self.frozen else "running",
                "adapter": "dolphin-gdb"}

    def read_memory(self, p):
        addr = int(_need(p, "address")); length = int(_need(p, "length"))
        r = self.gdb.send(f"m{addr:x},{length:x}")
        if r.startswith("E") or not r:
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
        off = 0
        for name, size in _REG_LAYOUT:
            nib = size * 2
            if off + nib > len(r):
                break
            state[f"cpu.{name}"] = int.from_bytes(bytes.fromhex(r[off:off + nib]), "big")
            off += nib
        return {"state": state}

    def pause(self, p):
        if not self.frozen:
            stop = self.gdb.interrupt()
            self._note_stop(stop)
            self.frozen = True
        return {"state": "frozen"}

    def resume(self, p):
        if self.frozen:
            self.gdb.send("c", expect_reply=False)
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
            raise GdbError("Dolphin GDB 스텁은 exec(소프트웨어) BP만 — HW watch 미지원")
        addr = int(_need(p, "start"))
        r = self.gdb.send(f"Z0,{addr:x},4")  # PPC 명령은 4바이트
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
        self.gdb.send(f"z0,{addr:x},4")
        del self.bps[bid]
        return {"cleared": bid}

    def list_breakpoints(self, p):
        return {"breakpoints": [{"id": i, "kind": "exec", "start": a, "end": a}
                                for i, a in self.bps.items()]}

    def poll_events(self, p):
        self._drain_stop()
        ev, self.events = self.events, []
        return {"events": ev, "dropped": 0}

    def _note_stop(self, stop):
        pc = None
        if stop and stop[0] == "T":
            for pair in stop[3:].split(";"):
                if ":" in pair:
                    k, v = pair.split(":", 1)
                    if k.lower() in (f"{_PC_REGNUM:x}", "pc"):
                        try:
                            pc = int.from_bytes(bytes.fromhex(v), "big")
                        except ValueError:
                            pass
        return pc

    def _drain_stop(self):
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
    gdb_host, gdb_port = "127.0.0.1", 2159
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
            except Exception as e:  # noqa: BLE001
                resp = {"id": rid, "ok": False,
                        "error": {"kind": "bridge_error", "message": repr(e)}}
        payload = (json.dumps(resp, separators=(",", ":")) + "\n").encode()
        if not _write_front_response(sock, payload):
            return


def _write_front_response(sock, payload):
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
