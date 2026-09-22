"""Package tagged public source plus core binaries; smoke-test the extracted payload."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tarfile
import tempfile
import threading
import tomllib
import zipfile

CORE = ("emucap", "emucap-mcp", "emucap-track-mcp", "emucap-broker")


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def smoke(root, version, revision, scratch):
    suffix = ".exe" if os.name == "nt" else ""
    bindir = root / "target/release"
    env = dict(os.environ, EMUCAP_REGISTER_DRY_RUN="1", EMUCAP_PORT="0",
               EMUCAP_EMU_HOME=str(scratch / "home"), EMUCAP_TRACK_ROOT=str(scratch / "track"),
               EMUCAP_REPO_ROOT=str(root))
    env.pop("EMUCAP_BROKER", None)
    script = root / "tools" / ("register-codex-mcp.ps1" if suffix else "register-codex-mcp.sh")
    command = ["pwsh", "-NoProfile", "-File"] if suffix else ["sh"]
    output = run(*command, str(script), env=env)
    for name in CORE[1:3]:
        assert str(bindir / (name + suffix)) in output, output
    assert "Usage:" in run(str(bindir / ("emucap" + suffix)), "--help", env=env)
    evidence = {"emucap": {"help": "passed"}}
    for name in CORE[1:3]:
        with tempfile.TemporaryFile(mode="w+") as errors:
            process = subprocess.Popen([str(bindir / (name + suffix))], cwd=scratch,
                                       env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=errors, text=True)
            responses = queue.Queue()

            def read_lines():
                for line in process.stdout:
                    responses.put(json.loads(line))

            reader = threading.Thread(target=read_lines, daemon=True)
            reader.start()

            def request(number, method, params):
                process.stdin.write(json.dumps(dict(jsonrpc="2.0", id=number,
                                                     method=method, params=params)) + "\n")
                process.stdin.flush()
                while True:
                    response = responses.get(timeout=30)
                    if response.get("id") == number:
                        assert "error" not in response, response
                        return response["result"]

            try:
                initialized = request(1, "initialize", {
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": {"name": "release-package-check", "version": "1"}})
                assert initialized["serverInfo"]["version"] == version, initialized
                process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
                process.stdin.flush()
                catalog = request(2, "tools/list", {})
                assert catalog["tools"], catalog
                evidence[name] = {"serverInfo": initialized["serverInfo"],
                                  "tool_count": len(catalog["tools"])}
                if name == "emucap-mcp":
                    result = request(3, "tools/call", {"name": "status", "arguments": {}})
                    state = result.get("structuredContent")
                    if state is None:
                        state = json.loads(result["content"][0]["text"])
                    assert state["server_build"] == revision, state
                    assert state["connected"] is False, state
                    evidence[name]["server_build"] = state["server_build"]
            finally:
                process.stdin.close()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                reader.join(timeout=5)
                errors.seek(0)
                if process.returncode:
                    raise RuntimeError(errors.read())
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("source", "target", "version", "revision", "output"):
        parser.add_argument("--" + option, required=True)
    args = parser.parse_args()
    compiler = run("rustc", "-vV")
    assert f"host: {args.target}" in compiler, compiler
    source, output = Path(args.source).resolve(), Path(args.output).resolve()
    assert run("git", "-C", str(source), "rev-parse", "HEAD") == args.revision
    assert not run("git", "-C", str(source), "status", "--porcelain")
    assert tomllib.loads((source / "Cargo.toml").read_text())["package"]["version"] == args.version
    short = run("git", "-C", str(source), "rev-parse", "--short", "HEAD")
    suffix = ".exe" if args.target.endswith("windows-msvc") else ""
    name = f"emucap-{args.version}-{args.target}"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="emucap-release-") as temporary:
        temporary = Path(temporary)
        stage = temporary / name
        stage.mkdir()
        archive = temporary / "source.tar"
        subprocess.run(["git", "-C", str(source), "archive", "--format=tar",
                        "-o", str(archive), "HEAD"], check=True)
        with tarfile.open(archive) as packed:
            packed.extractall(stage, filter="data")
        bindir = stage / "target/release"
        bindir.mkdir(parents=True)
        digests = {}
        for binary in CORE:
            path = bindir / (binary + suffix)
            shutil.copy2(source / "target" / args.target / "release" / path.name, path)
            digests[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
        manifest = dict(version=args.version, source_revision=args.revision,
                        target=args.target, binaries=digests,
                        rustc=compiler)
        (stage / "CORE-BUILD.json").write_text(json.dumps(manifest, indent=2) + "\n")
        (stage / "PREBUILT-CORE.md").write_text(
            "# Prebuilt emucap core\n\n"
            "The four core executables are in `target/release/`. Keep this directory tree intact.\n"
            "Run `sh tools/register-codex-mcp.sh` (macOS/Linux) or "
            "`pwsh -File tools/register-codex-mcp.ps1` (Windows), then reconnect both MCP servers.\n"
            "The core build step in README can be skipped. Adapter bridges and emulator hosts "
            "are separate builds; follow the selected adapter's README. Rust and C/C++ tools "
            "may still be required for those builds. No emulator, firmware or game is bundled.\n"
            "Apple Silicon binaries are not Developer ID signed or notarized.\n")
        if suffix:
            package = Path(shutil.make_archive(str(output / name), "zip", temporary, name))
        else:
            package = output / (name + ".tar.gz")
            with tarfile.open(package, "w:gz") as packed:
                packed.add(stage, arcname=name)
        extracted = temporary / "extracted"
        extracted.mkdir()
        if suffix:
            with zipfile.ZipFile(package) as packed:
                packed.extractall(extracted)
        else:
            with tarfile.open(package) as packed:
                packed.extractall(extracted, filter="data")
        manifest["validation"] = smoke(extracted / name, args.version, short, temporary)
        (output / (name + ".json")).write_text(json.dumps(manifest, indent=2) + "\n")
        digest = hashlib.sha256(package.read_bytes()).hexdigest()
        (output / (name + ".sha256")).write_text(f"{digest}  {package.name}\n")
        print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
