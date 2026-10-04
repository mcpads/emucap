# Compatibility policy

This policy applies from 1.0.0. Changes are recorded in the changelog.

## Stable 1.x surface

The supported consumer interface is the Control and Tracking MCP tools, documented
CLI commands and configuration, and explicitly versioned emucap-owned record formats.
For supported operations, 1.x preserves tool and argument names, accepted argument
meanings, required response fields and their types, documented error-code meanings,
and completion, interruption, cleanup and generation-identity guarantees.

Patch releases correct behavior to the published contract. Minor releases may add
optional arguments, fields, tools, capabilities and explicitly versioned formats.
Removing or incompatibly changing the stable surface requires a major release.
Consumers accept additional object fields and discover optional features from live
capabilities. Closed schema enums remain closed until explicitly versioned or revised
in a major release; extensible catalogs such as system IDs are discovered at runtime.
Human-readable messages and instruction wording may improve within 1.x.

## Runtime and adapter boundaries

Use the connected generation's `status` to admit operations. Availability depends on
host, emulator build, system and launch profile; 1.x does not promise identical tool
support on every emulator. Advertised operations keep their documented semantics.
An unsafe or unverifiable operation fails explicitly rather than reporting success.
A corrective restriction is documented with its affected profile and recovery path.

Experimental profiles are identified in the README and adapter documentation. Their
coverage and adapter-specific extensions may change with a changelog entry; stable
core response and lifecycle guarantees still apply when those profiles are used.

Native hosts, bridge binaries, patches and sidecars form a matched build. Host API
versions may advance within 1.x, requiring an adapter rebuild. Older incompatible
hosts are rejected explicitly. Adapter wire protocols and the internal Rust library
are implementation interfaces, outside the consumer compatibility promise.

## Stored data

Emucap-owned records use their declared schema versions. Existing supported versions
remain readable within 1.x, directly or through an explicit documented migration.
Writers identify new versions; readers reject unsupported versions without rewriting
the original. A receipt authenticates its specific artifact and snapshot bytes, not
compatibility with another emulator build.

Native save states remain emulator/build-specific. Keep the matching host and content
when restoration matters; releases describe known state compatibility changes.
Runtime homes, application copies, caches and qualification output are local artifacts,
not portable record formats. Deleting an artifact also deletes its ability to serve as
an original runtime witness; a retained hash or summary is only a historical record.
