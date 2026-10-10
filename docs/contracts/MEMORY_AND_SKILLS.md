# Memory, skills, and persona

Memory, skills, and persona are artifacts bound to runs. Updating one is an
ordinary run under policy.

## Memory is an artifact

Memory **state** is an artifact made visible through bindings. A memory
**change** is a run.

Retrieval is not a free global read. A run retrieves only what it holds a
binding for. "Give me relevant memory" is an artifact binding, not a
side-channel database query and not a memory-specific runtime interface.

An ordinary run artifact is not automatically memory. It becomes memory, in the
sense a person would mean, when it is bound into a run's context.

The typical flow:

1. An operator-facing run receives a message through a chat interface.
2. It decides part of it is worth keeping and sends an artifact toward a
   memory-maintenance environment.
3. A memory worker run there interprets the artifact and decides what to
   store.
4. The resulting artifact becomes visible only to runs whose bindings and
   formed policy admit it.

## Skills are artifacts

A skill is an artifact. Some are written by hand at setup time and then
maintained by the system. A "how the system architecture works" artifact is
bound into almost every worker's context for grounding. A coder additionally
binds a "how the codebase is laid out" artifact.

After a successful run, another run can notice the world changed - a module was
added - and update the relevant skill so the next run sees the new reality.
That update is an ordinary worker run editing an artifact. It is not a skill
subsystem.

The registry ships `skill.capture`, `skill.upsert`, `skill.search`,
`skill.list`, and `skill.get` as **ordinary tools** behind the system room's
door. A concept-named tool is fine. A concept-named runtime primitive is not.

- **Authority is policy, never worker identity.** Whoever's formed grant
  includes the tool may call it. A memory worker is the typical writer in a
  maintenance environment because policy grants it there. That is
  configuration, not doctrine.
- **Visibility is binding.** A worker receives a concrete skill artifact
  through an artifact binding, or searches when policy grants the tool. Search
  narrows within binding-gated access; it is not a global lookup.
- **Runs carry provenance.** A skill is written from inside a run, so its
  source run and trace are honest by construction. Needing to fabricate
  provenance is the signal you are off the road.

### Three ways a skill gets written

1. **Seed.** Any run with the grant writes it directly. Hand-written seeds are
   first-class.
2. **Capture.** A worker whose policy grants capture emits a proposal artifact.
   What happens next is ordinary artifact movement: a granted worker may
   inspect it, refine it, reject it, or persist it.
3. **Delegation.** A run that should not hold write grants emits an artifact
   through a System-interface binding to a run that does.

## Persona is the same shape

Persona is an artifact bound to a run, and it can differ per run. The Home run
holds a binding to a persona artifact. It starts essentially empty and it is
mutable: changing it means running a worker in the right environment, under
policy, that edits the artifact like any other.

## The two-layer rule

**The registries the system may change. The runtime it may not - but the
runtime must support any registry change.**

- Skill-named or memory-named **registry objects are fine**: tool descriptors,
  artifact descriptors, EE templates that allow those tools.
- Skill-named or memory-named **engine code is the violation**. If the runtime
  knows the word "skill", a registry concept has been hardcoded into the runtime
  and can no longer be evolved from the registries.

## Native emergence

Do not solve a missing behavior by creating a module named after it.

If skills are not being created, the answer is not a skill module. Inspect the
native path: a finished run → an experience artifact → policy permitting its
transfer → arrival at a receiving checkpoint → a memory worker transforming it
→ an updated artifact visible through permitted bindings. If that path is
broken, fix the primitive or the contract that broke it.

If self-improvement is not working, the answer is not a self-improver.
Self-improvement is the system using its model of itself to create goals, runs,
interfaces, or descriptor changes when policy permits. Those are framework
operations, not a feature loop.
