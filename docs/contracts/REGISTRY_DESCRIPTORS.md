# Registry descriptors

Octopus runs every dynamic registry through the same shape:

> A registry entry is a JSON descriptor in a folder named for its id, read by
> one fail-closed loader for its kind.

System behavior, tools, workers, and model bindings are defined through
descriptors rather than inline code checks. This is not hygiene. Registry-first
design is what lets the system improve itself: a model can inspect, modify, or
create descriptors mid-execution without touching the kernel.

## The registries

```text
registry/
  interface/<id>/interface.json                     every interface
  interface/system_worker_<name>/policy.json        a worker's ceiling
  ee_template/<id>/ee_template.json                 goals and their policy
  worker_environment_kind/<id>/worker_environment_kind.json
  tool/<id>/tool.json
  verifier/<id>/verifier.json
  mark/<id>/mark.json
  artifact/<id>/artifact.json                       declared context artifacts
```

The source tree's `registry/` is the library used to build the distribution.
The app ships a startup baseline and a local library of optional packages. A
Home's `<home>/registry/` contains the installed, active descriptors and their
local changes. That is what the running system reads and edits; it is not a
live mount of the checkout or necessarily a copy of the entire library.
See [the source and Home development loop](../development.md#source-and-home).

## What every registry must be

- **Declarative** - a descriptor, not a branch in engine code.
- **Introspectable** - the catalog is JSON anyone can read.
- **Replaceable on the fly** - editing a descriptor is editing the registry,
  and the change is picked up live, without restarting.
- **Fail-closed** - a malformed descriptor is rejected, never silently
  defaulted.

A central function that registers built-ins in code is *not* this shape. It is
hardcoded built-ins with a different file extension. Built-in components,
components someone authored, and anything the system created by editing itself
all land as descriptors and bind to each other through JSON.

## One shape

Every descriptor is a JSON object in a directory whose name equals its declared
id. Linking between registries is by reference, never by importing code: a
worker pin names interfaces, tools, and room kinds; an interface names its
driver family; an EE template names the interfaces it admits. All as ids
resolved through the registries.

"Kind" here means a descriptor kind - tool, interface, EE template. It has
nothing to do with artifacts, which carry no runtime type field at all.

## Providers, models, and endpoints are interfaces

These are not separate registry kinds. A provider integration is a concrete
`world_provider_<name>` interface. The model a run uses comes from its active
interface binding plus that interface's own config - not from a parallel model
registry and not from a call-time selector.

## Invariants

- **One JSON gate.** Every descriptor body crosses the same well-formedness
  boundary before any kind-specific reading. A malformed body fails before any
  identity, policy, or reference field is read. A kind's loader may be
  stricter; none may be softer.
- **No private soft readers.** Sidecar config and descriptor-adjacent data
  cross the same gate. Malformed input becomes explicit failure evidence, never
  a silent empty value.
- **Defaults are not repair values.** A default applies only after the live
  input has passed its own boundary.
- **Unknown fields fail.** Where a kind's loader is strict, a field it does
  not know is drift, not an extension point. Free-form data belongs in the
  fields declared for it, such as an interface's own driver config.
- **References must resolve.** A worker ceiling naming a missing mark, a tool
  naming a binary its kind does not own, a door that belongs to no kind - each
  fails closed at load.
- **Duplicates fail.** Load order is not a decision surface. Two descriptors
  claiming the same reference are refused rather than letting one silently win.

## Naming

The registry will accumulate thousands of descriptors. That is not a mess; it
is the artifact. Each one obeys:

- **An interface's name is its function, not a convenience alias.**
  `world_microphone` is the audio intake port. `world_speech_to_text` is the
  transcript-facing port produced after transcription. Do not collapse the two,
  and do not add parallel raw-transport names beside them.
- **Check the live registry before adding.** If an interface already covers the
  boundary, bind that folder. Do not author a parallel one.
- **A new worker is a new `system_worker_<name>/` folder**, not an entry
  somewhere else.
- **Function first.** Specialize through the descriptor's real fields and the
  interface's own config. None of those fields replaces the folder id as the
  routing and policy identity.

## Self-modification is file editing

Because everything is a descriptor and every link is a JSON reference, the
system extends and reshapes itself by writing descriptors and having them
picked up live. A run with a room whose path covers a registry folder can write
there with ordinary file tools. A run without that grant does not get a special
mutation API: it emits an artifact to a run that already holds the authority.

Authority is policy. No goal name and no supervising worker's identity is
hardcoded into the runtime as the one allowed to change the registry.

## Adding a registry kind

A new kind means a descriptor shape, a fail-closed loader, and declared
behavior - never an engine branch that knows the kind by name.
