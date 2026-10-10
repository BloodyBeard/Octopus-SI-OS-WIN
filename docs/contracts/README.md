# Contracts

[Architecture](../architecture.md) is the picture. These pages are the detail:
what each primitive is, what the runtime guarantees about it, and what it
refuses to do.

Where a page and the code disagree, that is a bug in one of them. Report it
rather than building on either.

| Page | What it settles |
| --- | --- |
| [Policy](POLICY.md) | How a run's grant is formed. Ceiling versus grant, narrowing by place, checkpoints as the central capability. Start here. |
| [Artifacts](ARTIFACTS.md) | What crosses a boundary, why there is no runtime `kind`, fan-out, bindings as the visibility boundary. |
| [Interfaces](INTERFACES.md) | Folder identity, delivery as ledger movement, living interfaces, providers as drivers, secrets. |
| [Workers](WORKER_TYPES.md) | A worker is a System-interface pin with a ceiling. How runs wear one and reach another. |
| [Environments](ENVIRONMENTS.md) | The two axes - goal scope and tool room - and why they never merge. |
| [Tools](TOOL_REGISTRY.md) | Tools live below rooms. Descriptors, invocation, physical authority. |
| [Marks and verifiers](VERIFIERS.md) | Required verified artifacts, verifier packages, marks as proof and as coordination. |
| [The ledger and its indexes](LEDGER_INDEX.md) | The movement log as truth; caches that can always be rebuilt. |
| [Registry descriptors](REGISTRY_DESCRIPTORS.md) | One shape for every registry; naming; self-modification as file editing. |
| [Memory, skills, and persona](MEMORY_AND_SKILLS.md) | Artifacts under policy, nothing more. |
| [Extending Octopus](EXTENSION_SURFACES.md) | The cookbook. |

## Vocabulary

The [glossary](../glossary.md) defines every term used here, maps the app's
words to the architecture's, and lists the common agent-framework words that
Octopus deliberately does not have.
