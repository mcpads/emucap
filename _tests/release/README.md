# Build and release checks

`package_core.py` packages the core release executables.

The **Native adapter builds** workflow compiles the pinned Windows Mesen, Dolphin,
and PPSSPP recipes when native inputs change, or through manual dispatch. Core
version changes do not trigger these builds. Its exact cache key includes adapter
sources, patches, recipe, target and runner toolchain version. The three PowerShell
recipes do not consume MSYS2 shared helpers, so those helpers do not invalidate them.
Every retained runtime file is verified before reuse. Missing or invalid cache
contents cause a fresh build.

The workflow retains runtime payloads, file-hash manifests and build logs for seven days.
These are compile checks; game runtime qualification is recorded separately.
Native binary releases and their corresponding source bundles are not published
by this workflow. Rust bridges continue to follow the core version.

Run the cache identity and payload checks locally:

```sh
python3 _tests/release/native_build_tests.py
```

The **Native MSYS2 adapter builds** workflow compiles Mednafen, Flycast, NP2kai,
Mupen64Plus, openMSX, MAME PC-98, MAME Neo Geo, PCSX2, and DeSmuME on Windows from their shell
recipes. MAME builds only the adapter's drivers. It records compiler/package versions,
build output, and the resulting executable or core DLL hash. These additional checks have
no game-runtime qualification yet. Successful builds retain runnable payloads with
native DLL dependencies and required assets for seven days; core-only changes do
not trigger them. Manual dispatch can select one adapter for a targeted retry.
PCSX2's shell recipe holds the work-tree lock while its PowerShell helper builds
the upstream dependencies and emulator with Visual Studio C++ tools.

The **Native xemu Windows build** workflow uses the upstream Windows cross-toolchain
container, pinned by digest. It applies the maintained source patches and records
the Windows executable hash and retains the Windows distribution for seven days;
Windows runtime validation remains separate.

`inspect_github_artifact.py` reads a retained ZIP's directory and native manifest
using bounded HTTP ranges. It requires an authenticated `gh` CLI and refuses a
bulk-download fallback. This checks package contents before remote deployment;
the full archive digest must still be verified where the archive is downloaded.

`windows_software_gl.ps1` installs a digest-pinned software OpenGL package beside
selected validation executables. Supply a private plan with `asset.url`,
`asset.digest` (`sha256:...`) and application `directories` relative to the
validation root. It writes a deployment record and leaves renderer behavior to
the runtime witnesses.


`deploy_windows_payloads.py` deploys already-downloaded qualification ZIPs from a
private plan. It verifies each archive digest, supports a nested core ZIP and
member prefix, and resumes only over identical destination files. Run it on the
validation host, then freeze the complete deployed layout with
`_tests/live/windows_runtime_inventory.py` before executing the batch.


`upload_qualification_inputs.py` uploads a reviewed private file manifest under an
operator-owned bucket's `inputs/` prefix. It verifies local bytes and reads back
object metadata, permits identical resumes and refuses conflicting existing
objects. Downloaded-byte verification remains the remote preflight's job.

`deliver_windows_artifacts.py` runs on the operator machine after the Windows
host is ready for SSM commands. It obtains a fresh Actions download URL for each
archive and sends a download-and-hash command to that host. The GitHub token and
archive bytes stay off the operator-to-host transfer path. Its local ledger
retains command IDs so interrupted observation resumes the same command; only a
terminal failed attempt permits a retry with a new URL. Supply a complete
`deploy_windows_payloads.py` plan with each Actions entry's `artifact_id`.
