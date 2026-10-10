# Collaboration-mode role artifacts (Plan / Default / Execute / Pair Programming)

Conducted home of the codex `collaboration-mode-templates` markdowns (codex-cut-map §9.6:
`collab-templates → EE template / System worker-interface pin` — scope presets as AUTHORED DATA + bound role
artifacts, not compiled `include_str!`). Each `artifact-collab-mode-*` descriptor carries one
mode's developer-instructions text as an ordinary bound context artifact (`context.reference`),
exactly like the worker role/grounding artifacts beside it: a "mode" is not a runtime primitive —
it is a role artifact a binding attaches plus whatever tool-surface narrowing the worker
pin policy already expresses (Plan mode's "no mutating actions" is a policy fact, not a prompt
switch the kernel knows about). No new ee_template was minted: the mode text is instruction data;
scope stays with the existing EE-template and worker-pin descriptors that bind it.

`{{KNOWN_MODE_NAMES}}` in the default-mode text is a render slot substituted by the interface
that binds the artifact (context assembly lives in the interface — constitution §4).

RESIDUAL (cross-front, world_terminal): the compiled crate
`registry/interface/world_provider_codex/packages/octopus-collab-templates` (4-line
`include_str!` shim over the same markdowns) is still linked by
`octopus-tui/src/collaboration_modes.rs` + its Cargo.toml. THIS registry home is now the
authoritative authored source; when the terminal front re-points its mode presets at bound
artifacts (or its own folder data), `git rm` the crate and its `templates/`. Do not edit the
crate's markdowns — edit the artifact descriptors here.
