# WonderSwan idle progress witness

`idle.asm` is a self-authored 128 KiB ROM. It masks interrupt sources, writes
`0x1234` to RAM at `0x100`, then enters HLT. The next instruction would replace the
marker with `0x5678`; that instruction must remain unexecuted during the test.

Build with NASM installed:

```sh
python3 _tests/live/mednafen/ws-idle-fixture/build.py out/ws-idle.ws
```

Use a local profile whose `launch_plan.system` is `wswan` and whose
`launch_plan.content_path` resolves to that ROM. Run:

```sh
python3 _tests/live/mednafen/owned-advance.py --profile <profile.json> --output <new-directory> --idle-fixture
```

The witness starts an owned instruction advance after a completed frame, requests
cancellation, verifies the unchanged marker and requires zero executed instructions.
It retains raw replies, producer identity and owned-process exit evidence. No
commercial ROM or firmware is needed.
