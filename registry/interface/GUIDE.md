# Writing an interface package

An interface is a folder-identity in `registry/interface`: an artifact goes in,
an artifact comes out. The runtime does not call interface code and does not
select a compiled family. If an interface needs to live, its folder declares a
separate process in `life.json`; the process reads its own movements and hands
new artifacts to the runtime's single intake over a stdin/stdout protocol.

Background: [Interfaces](../../docs/contracts/INTERFACES.md).

## Package files

```text
registry/interface/<id>/interface.json
registry/interface/<id>/life.json
registry/interface/<id>/<package code or scripts>
```

`interface.json` is required. `life.json` is needed only by an interface with
an executable life. A driver-less System worker pin legitimately consists of
its descriptor and `policy.json` alone.

A new instance of an existing family may declare a `reference` to a root
interface folder. If the instance has no `life.json` of its own, the resolver
takes exactly one `life.json` from that explicit reference. Chains,
self-reference, and cycles are refused. Identity is still the new folder.

## interface.json

```json
{
  "id": "world_my_channel",
  "interface_type": "world",
  "role": "user",
  "direction": "bidirectional",
  "source_names": ["my_channel"],
  "interface_driver_config": {
    "transport": {
      "base_url": "https://example.invalid/api",
      "wire": "json",
      "auth_env_key": "MY_CHANNEL_TOKEN"
    }
  }
}
```

The folder and `id` match. `interface_driver_config` is the interface's own
data; the kernel does not interpret transport, model, retries, or rate limits.
A secret value never goes into JSON: the config holds only the name of an
environment variable, and `life.json` selects it explicitly by JSON pointer.

## life.json

```json
{
  "processes": [
    {
      "bin": "/usr/bin/python3",
      "args": ["registry/interface/world_my_channel/life.py"],
      "required_files": ["registry/interface/world_my_channel/life.py"],
      "inherit_env_from_interface": [
        "/interface_driver_config/transport/auth_env_key"
      ],
      "authority": {
        "read_root_slots": [],
        "write_root_slots": [],
        "executable_root_slots": [],
        "read_paths": [],
        "write_paths": [],
        "executable_paths": [],
        "executable_commands": [],
        "read_env_paths": [],
        "write_env_paths": [],
        "executable_env_paths": [],
        "allow_network": true,
        "allow_local_ipc": false
      },
      "read_handles": ["ledger", "blobs"]
    }
  ]
}
```

The process starts fail-closed with a cleared environment and physical
authority. It always receives only its own interface folder, its own writable
`OCTOPUS_VAR_DIR`, a private stdin/stdout intake, and the handles it lists. The
ledger, the blob cache, and the Spotlight index are available only as explicit
read handles. Root slots, environment paths, network, local IPC, package files,
and executables are declared as data; the sandbox denies anything undeclared.

`coordinate_root_slots` passes absolute coordinates without file access. Code
that authors an ordinary file-tool artifact for a permitted path needs this;
the life process itself gains no right to read or write from a coordinate.

An executable in the interface's own folder is addressed as
`owner://packages/...`. One owned by another interface is addressed as
`interface://<source-interface-id>/packages/...`.

## The process protocol

The process prints one JSON line at a time:

```json
{
  "kind": "arrival",
  "request_id": 1,
  "artifact_manifest_path": "/absolute/private-var/output.artifact.json",
  "evidence": []
}
```

The runtime answers on stdin with a `delivery_verdict` carrying the same
`request_id`. The manifest must be an ordinary non-symlink file inside the
private var root. A System-interface emission uses `kind: system_emission` and
explicitly names the emitting run, checkpoint, from-interface, and
to-interface; the runtime still re-forms the grant and may refuse.

## Cursor

A cursor is only this reader's bookmark in the append-only ledger. Commit it
after an admitted output artifact or an admitted failure artifact. A transient
failure, a crash, an unavailable intake, or an unavailable process does not
move the cursor. A poisoned movement is first turned into a machine-readable
failure artifact; if that could not be handed in, the row is retried after
recovery.

## Invariants

- only `life.json` chooses the executable, not a compiled mount table;
- adding an interface folder does not change the runtime or app bootstrap;
- the process receives its descriptor and config, read-only handles, the
  intake, and only explicitly declared roots;
- the artifact payload stays opaque to the kernel;
- `reference` reuses a life but creates no second identity mechanism.

## A working example

`registry/interface/world_pyecho/` is a complete interface in one Python file.
