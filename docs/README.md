# Octopus docs

## Using Octopus

- [Getting started](getting-started.md) - build the macOS app, first setup,
  where your data lives.
- [Desktop client](client.md) - Home, Tasks, voice, gestures, Applications,
  Drivers, Extensions, and where packages come from.
- [Troubleshooting](../TROUBLESHOOTING.md) - where to look when something
  fails.
- [Security model](security-model.md) - what stands between a model's output
  and an effect.

## Understanding Octopus

- [Architecture](architecture.md) - System and World, the primitives, one turn
  end to end, why there is no agent loop at the center.
- [Glossary](glossary.md) - every term, and what the app's words mean
  underneath.
- [Scenarios](scenarios.md) - what the primitives are meant to express.

---

## Working on Octopus

Start with [Development](development.md) and the
[contribution policy](../CONTRIBUTING.md). Coding agents start at
[AGENTS.md](../AGENTS.md).

The contracts record decisions and constraints that the source alone does not explain.

- [Policy](contracts/POLICY.md) - how a grant is formed. Start here.
- [Artifacts](contracts/ARTIFACTS.md)
- [Interfaces](contracts/INTERFACES.md)
- [Workers](contracts/WORKER_TYPES.md)
- [Environments](contracts/ENVIRONMENTS.md)
- [Tools](contracts/TOOL_REGISTRY.md)
- [Marks and verifiers](contracts/VERIFIERS.md)
- [The ledger and its indexes](contracts/LEDGER_INDEX.md)
- [Registry descriptors](contracts/REGISTRY_DESCRIPTORS.md)
- [Memory, skills, and persona](contracts/MEMORY_AND_SKILLS.md)
- [Extending Octopus](contracts/EXTENSION_SURFACES.md) - the cookbook.

Authoring guides live beside the descriptors they describe:
[interface](../registry/interface/GUIDE.md) ·
[worker environment kind](../registry/worker_environment_kind/GUIDE.md) ·
[tool](../registry/tool/GUIDE.md) ·
[verifier](../registry/verifier/GUIDE.md) ·
[mark](../registry/mark/GUIDE.md)

### What goes here, and what does not

These docs are deliberately few. A page earns its place by recording a
decision, a constraint that spans components, or a trap that is hard to
discover from the source. Before adding a paragraph, ask what a maintainer
would get wrong without it.

Plans, research notes, execution journals, review logs, and reference
checkouts are working material. They stay local, in ignored paths, and are not
published.
