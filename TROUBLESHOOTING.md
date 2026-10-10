# Troubleshooting

Octopus fails closed. When something is wrong you should see an error, not a
quiet fallback. This page says where to look.

## The first five minutes

1. Run the doctor against the Home you are using. From the checkout root:

   ```sh
   ./app/build/Octopus.app/Contents/Resources/app/bin/octopus doctor
   ```

   For a custom Home, pass the same `OCTOPUS_HOME` you launch with.
2. Read the error the app shows and use its own retry or sign-in action.
3. Do not delete the Home or replace its descriptors to make an error go away.
   To start clean, launch with a separate Home instead:

   ```sh
   OCTOPUS_HOME="$HOME/.octopus-preview" ./app/build/Octopus.app/Contents/MacOS/Octopus
   ```

## Where the evidence is

| You want to know | Look at |
| --- | --- |
| Why setup stopped | `<home>/client/setup-state/operations/*.json` - the `last_error` field |
| Why an interface is not answering | `<home>/interfaces/<interface-id>/interface-life.stderr.*.log` |
| What moved, and what was refused | `<home>/ledger/movements.jsonl` - look for `DeliveryRefused` and `VerificationFailed` |
| What a run is waiting for | `<home>/state/runs/<run>.json` - its `active_checkpoints` |

The screen shows a short message. The files above carry the exact reason.

## Compare source, installation, and local JSON

For a package/configuration bug, collect a scoped report before changing the
Home. From the checkout root, set the Home you actually use:

```sh
OCTOPUS_DIAG_HOME="$HOME/.octopus"
OCTOPUS_DIAG_SOURCE="$PWD/registry"
OCTOPUS_DIAG_LIBRARY="$PWD/app/distribution/local-packages"

node app/electron/clientDoctor.cjs \
  "$OCTOPUS_DIAG_SOURCE" "$OCTOPUS_DIAG_HOME/registry" \
  "$OCTOPUS_DIAG_LIBRARY" "$OCTOPUS_DIAG_HOME/client/package-state" \
  --package world_provider_codex > package-diagnostic.json
```

`--package` takes the local package id. To inspect one descriptor instead,
replace `--package world_provider_codex` with:

```sh
--registry-file interface/world_provider_codex/interface.json
```

Use the source belonging to the build being diagnosed. For a packaged app,
set `OCTOPUS_DIAG_SOURCE` to its
`Contents/Resources/app/local-packages/registry` and `OCTOPUS_DIAG_LIBRARY` to
`Contents/Resources/app/local-packages`. A checkout is useful for comparing
against a proposed update, but it is not evidence of what an older app shipped.

The report compares three things: the selected source, the last installation
receipt or baseline, and the active files. For packages, it distinguishes
package-owned content from manifest-declared configuration fields. A changed
field is not automatically corruption, and a descriptor created only in Home
does not need a source package owner. `null` means there is not enough evidence
for that comparison.

The versioned output is `octopus.configuration_diagnostic`, version `1`.
By default it omits names, paths, JSON values, active-content hashes, logs,
credentials, and environment variables. `--include-paths` adds relative names
and configuration pointers for a more precise local investigation; review
those names before sharing. Values stay omitted. Nothing is uploaded, repaired,
installed, or booted by this command.

Exit `0` means the observation completed, not that the installation is healthy.
Exit `2` means inspection could not complete. The report is bounded to 1 MiB
and 256 files per package member, prioritizing differences; omitted counts are
explicit. It is not an atomic snapshot or an edit history. Package inspection
still hashes the selected owner folders, so a large package can take time.

The ordinary Doctor and **Copy diagnostics** in Settings remain local
diagnostics and can include private paths or details. They are not this
redacted export. Inspect the scoped report first; if a maintainer needs a
specific setting afterward, share only the relevant sanitized value.

## The build fails

- **`Packaging native PiP requires explicit OCTOPUS_NATIVE_NODE_HEADERS`**  - 
  fetch the pinned Node 24.15.0 headers and set the variable, as shown in
  [Getting started](docs/getting-started.md#build).
- **Native rebuild or SQLite check errors** - you are on Node 20 or 22. Use
  Node 24.
- **Rust errors about the edition or a locked dependency** - update to current
  stable Rust.
- **The packager built the wrong architecture** - it builds for the
  architecture of the Node process running it. Use a native arm64 Node on Apple
  silicon.

## The launch terminal shows an error, or nothing happens after Enter

- **The Home path is too long.** macOS limits Unix socket paths to about 104
  bytes, and the runtime keeps its sockets inside the Home. Use a short path
  such as `~/.octopus` or `~/.octopus-preview`.
- **The Home path goes through a symlink.** `/tmp` is a symlink on macOS. Point
  `OCTOPUS_HOME` at a real path under your home directory.
- **Another runtime already holds this Home.** One runtime per Home. Quit the
  other app instance, or run `octopus down` with the same `OCTOPUS_HOME`.

## Sign in with ChatGPT does not complete

- Finish the flow in the browser window that opened, then return to the app.
- Cancel in the app and start again if the browser tab was closed.
- The account must have access to the model you select. Availability and usage
  limits belong to the provider account, not to Octopus.

## Setup says it needs attention

Open the newest file in `<home>/client/setup-state/operations/` and read
`last_error`. Use the app's retry after fixing what it names. Setup is a
transaction with recorded steps; retrying continues from the step that failed.

## Home does not reply

1. Check that the provider interface is alive: read the newest
   `interface-life.stderr.*.log` under the Home's provider interface folder in
   `<home>/interfaces/`.
2. Search the end of `ledger/movements.jsonl` for `DeliveryRefused`. The row
   names the reason - `policy`, `no_binding`, `checkpoint_spent`, and so on  - 
   and the run and checkpoint it happened on.
3. Send a short message again after the cause is fixed. A run is data; nothing
   needs to be restarted for it to continue.

## A tool call was refused

That is usually the system working. The refusal row in the ledger says whether
the route was outside the run's grant (`policy`), had no authored binding
(`no_binding`), or failed its verifier (`verification_failed`). Grant the
project the driver or application it needs in the app rather than editing the
run record by hand.

## Open a useful bug report

Include the commit, macOS version and architecture, the steps, what you
expected, and what happened. For installation/configuration issues, attach the
scoped report above and identify the public package/version separately. For a
runtime failure, include the relevant `last_error` or refusal row. Remove
credentials, sign-in URLs, private prompts, project paths, and artifact contents
from those raw excerpts. Do not attach the whole Home.
