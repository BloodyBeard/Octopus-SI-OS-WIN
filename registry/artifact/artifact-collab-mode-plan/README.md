# Collaboration-mode artifacts

`artifact-collab-mode-*` contains Plan, Default, Execute, and Pair Programming instructions as bound `context.reference` artifacts. Edit the mode text here. Existing EE-template and System worker-interface bindings select the context and permitted tools; mode text does not create a runtime primitive or grant authority. Plan's mutation restriction must also be enforced by policy.

The binding interface substitutes `{{KNOWN_MODE_NAMES}}` in Default mode.

The terminal consumer in `octopus-tui/src/collaboration_modes.rs` and its Cargo.toml still links `registry/interface/world_provider_codex/packages/octopus-collab-templates`. Until it reads bound artifacts or its own folder data, leave that dependency in place. After the consumer changes, remove the crate and its `templates/`. Do not edit the crate's Markdown templates; these descriptors are the authored source.
