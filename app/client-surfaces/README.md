# Client Surface Documents

This directory is the file-backed authoring surface for the desktop client's
**Test profile**. A root-level file whose name ends in `.surface.json` is read,
verified, and materialized there without rebuilding or restarting Electron.
Files below `examples/` are inert templates until copied here with that suffix.

**Administrator** reads stable documents from `projection.client_surfaces.list`:
admitted `client.surface.document` artifact movements in the selected Home's
ledger. It does not load this authoring directory. For the user-facing
distinction between surfaces and Extensions, see the [client guide](../../docs/client.md).

The contract is versioned by:

```json
{
  "schema": "octopus.client_surface",
  "schema_version": 1,
  "release_channel": "lab"
}
```

`lab` documents are admitted only inside Client Lab. A document intended for
Administrator must explicitly declare `stable` and arrive through the artifact
projection path. Changing that field in a local file alone does not publish it.
Removing the field is refused by client admission.

The canonical schemas are:

- `app/schemas/client_surface_document.schema.json`
- `app/schemas/client_surface_admission.schema.json`

## Targets

A document becomes a compact Voice widget, a larger Voice window, or a full
workspace surface.

```json
{ "kind": "voice.widget", "slot": "right", "order": 15 }
```

```json
{ "kind": "voice.window", "slot": "left", "order": 20, "size": "wide" }
```

Voice windows use the same admitted native/sandbox render rails as widgets, but
receive window chrome and a `compact`, `standard`, or `wide` stage footprint.
They can be hidden from the Voice Widgets menu without deleting their source
document.

```json
{
  "kind": "workspace.surface",
  "placements": ["shell"],
  "order": 70,
  "chrome": { "topbar": "wide", "workspace": "panel", "titlebar_glyph": "metrics" }
}
```

Workspace documents enter the same surface registry as built-in tabs, so they
participate in navigation, split panes, visibility, and pinning. They are client
projections, not runtime entities. `glyph` is the navigation/sidebar mark;
`chrome.titlebar_glyph` is optional and independently controls the window-title
mark. Omitting it keeps the title text without an icon.

## Rendering rails

`native` documents use the app's own tokens and components. The current node
vocabulary is `stack`, `row`, `section`, `text`, `key_value`, `status`, `metric`,
`progress`, `sparkline`, `button`, `divider`, and `spacer`. This is the preferred
rail for widgets that must look native under every theme and window size.

`sandbox` documents carry HTML, CSS, and optional JavaScript. They run in an
iframe with a unique origin, no network, no forms, no nested frames, no host DOM
access, no Electron bridge, and no backend bridge. Local interaction, SVG,
Canvas, and animations are available. A resize observer reports only the frame
height to the host, clamped to the contract's bounds.

There is no `eval` of model text in the React renderer. Free-form code exists
only inside the sandbox boundary.

## Data

Literal data lives in the optional `data` object. Native values can bind to it:

```json
{ "type": "metric", "label": "Runs", "value": { "path": "data.run_count", "format": "number" } }
```

A document may declare up to eight read-only projection sources:

```json
{
  "id": "movement_summary",
  "kind": "projection",
  "method": "projection.movements.list",
  "params": { "limit": 1 }
}
```

The result is available at `sources.movement_summary`. Sandbox documents receive
the same object as `window.octopusSurface = { data, sources }`. Projection
failures fail the document closed and are rendered explicitly; stale or invented
values are not substituted.

## Client actions

Native buttons publish a frontend-only event:

```json
{
  "type": "button",
  "label": "Open details",
  "action": { "event": "widget.open-details", "payload": { "section": "runs" } }
}
```

Sandbox documents declare the same bounded actions under `render.actions` and
may request one by id; the iframe never supplies the resulting event or payload:

```json
{
  "kind": "sandbox",
  "html": "<button id=details>Open details</button>",
  "javascript": "details.onclick=()=>octopusSurface.invoke('open-details')",
  "actions": {
    "open-details": { "event": "widget.open-details", "payload": { "section": "runs" } }
  }
}
```

Unknown action ids are ignored. The admitted document remains the only source
of the host action, so sandbox code cannot fabricate a client command or payload.

The host event is `octopus:client-surface-action`. This is not an artifact send,
tool call, or runtime side channel. Product code may handle navigation and local
presentation actions. A future action that changes the world must still enter
Octopus through an ordinary declared interface, artifact movement, policy gate,
and evidence path.

## Admission and hot reload

In Test, Electron watches this directory and sends a generation signal after
filesystem changes. In Administrator, projection changes trigger a fresh read
of the artifact-backed documents. The renderer verifies each complete document
and atomically rematerializes the admitted set. Invalid JSON, unknown fields,
unsupported versions, excessive size/depth/node counts, duplicate ids, duplicate
source ids, and oversized action payloads are refused. The previous invalid
document is not kept as a hidden fallback.

Each source file is SHA-256 addressed. Admission returns the verifier-shaped
contract:

```json
{
  "passed": true,
  "details": {
    "verifier": "client_surface_document_v1",
    "contract": "octopus.client_surface@1",
    "document_id": "voice.weather",
    "revision": 1,
    "source_digest": "sha256:...",
    "node_count": 4,
    "max_depth": 2,
    "issues": []
  }
}
```

This is deliberately shaped like an Octopus verifier verdict, but it is client
admission today. When model-authored surface documents arrive as artifacts, the
same document contract can be enforced by a removable verifier package on a
checkpoint edge, for example:

```json
{
  "mark": "client.surface.document.admitted",
  "artifact_label": "client.surface.document",
  "verifier": "client_surface_document_v1",
  "params": { "schema_version": 1 }
}
```

That example is not a shipped runtime mark/verifier. The live projection reads
admitted artifact movements and the client validates their document contract.
Client admission is distinct from a runtime `MarkOutcomeAdmitted`; it does not
claim that a checkpoint verifier ran.

The Test authoring root may be overridden with the absolute
`OCTOPUS_CLIENT_SURFACES_DIR`. Otherwise the source checkout uses
`app/client-surfaces`; a packaged client with no discoverable checkout uses its
own application-data directory.
