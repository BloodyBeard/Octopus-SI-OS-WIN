# Glossary

Terms whose meaning matters across Octopus. Constraints and reasons belong in
[Architecture](architecture.md) and the [contracts](contracts/README.md), not
in these definitions.

## The architecture

| Term | Meaning |
| --- | --- |
| System | Everything the system can act on: its environments and capabilities. A self-concept, not a container. |
| World | Everything else. Reached only through declared interfaces. |
| Kernel, runtime | The small core that admits artifacts, forms grants, and records movements. It knows nothing by name. |
| Interface | A typed port identified by its folder. Artifact in, artifact out. |
| World interface | An interface to something outside the system's control: a provider, a channel, a device, an API. |
| System interface | An internal boundary: a worker pin, the door of a room. |
| Artifact | A self-identifying manifest plus a reference to a payload. The only thing that crosses a boundary. |
| Movement | One recorded crossing between two interfaces. A row in the ledger. |
| Ledger | `movements.jsonl` - the append-only log of movements. The source of truth. |
| Run | A durable execution record: a worker, an environment, checkpoints. Data, not a process. |
| Checkpoint | An artifact crossing an interface, as authored on a run: what it awaits, where its output goes, what it reads. |
| Binding | An authored link from a checkpoint to an exact interface or artifact. |
| Worker | What other frameworks call an agent. Here it is data, not a program: a System interface `system_worker_<name>` with a policy ceiling. What a run wears. |
| Ceiling | A worker's static maximum: the interfaces, tools, room kinds, and marks it could ever hold. |
| Execution environment, EE | A scope classified by a goal and bounded by a policy. Runs live in EEs; EEs nest. |
| Goal | What classifies an EE. The root goal is `existence`. |
| Worker environment, room | Where tool effects physically happen: a workspace, a browser, a container. |
| Kind | The descriptor of a type of room and the executor behind it. |
| Door | The System interface a tool call is sent to in order to reach a room. |
| Tool | An executable behind a door. A tool call and its result are artifacts. |
| Policy | Descriptor rules and authored bindings from which the runtime forms a run's effective grant. |
| Grant | What one run may do right now: ceiling ∩ EE chain ∩ authored bindings. Formed at every arrival. |
| Formation | The deterministic computation of a grant. |
| Required verified artifact | A checkpoint requirement: this label, through this verifier, closes this mark. |
| Verifier | A removable script package that checks one artifact on one edge. |
| Mark | The recorded fact that a verifier passed. Written by the runtime, never by a model. |
| Descriptor | A JSON file in a registry folder that declares one of the things above. |
| Registry | The folders of descriptors. The system reads them live and can rewrite them under policy. |
| Home | The directory holding one installation's registry, runs, ledger, and interface state. |
| Driver | The code behind an interface. Any language; only artifacts cross out of it. |
| Life | An interface's own process, declared in `life.json`. |
| Cursor | An interface's bookmark in the ledger. It catches up from there after any restart. |

## The app

The desktop client uses its own words. They are presentation.

| In the app | In the architecture |
| --- | --- |
| Home | One continuous run in the root environment, bound to your chat and voice interfaces. |
| Project | An EE. |
| Agent | A named, reusable worker template: a `system_worker_*` pin plus authoring defaults. |
| Task | A run. Later messages continue the same run through new checkpoints. |
| Application | An installable capability presented through interface or context-artifact packages: a provider, connector, plugin, or knowledge package. |
| Driver | A worker-environment kind enabled for a project. |
| Extension | A client widget, tab, panel, or guided repair. It does not grant runtime authority. |
| Activity | The trace of a run, shown the way a coding agent's trace is shown. |

The Home screen and the Home directory are different uses of the same word.
The screen presents a run; the directory stores this installation's private
state. See the [client guide](client.md) for navigation and package setup.

## Research vocabulary

| Term | Meaning |
| --- | --- |
| Self-evolution | The process by which agents reshape their execution environment under policy, changing the conditions of subsequent work across goals and runs. |
| Trajectory | The course of activity shaped by accumulated decisions and persistent changes to goals, interfaces, triggers, tools, workers, and context. |
| Self-improvement | A research aim and a possible outcome of self-evolution: changes that actually improve the system's capabilities or performance. |

These terms describe the research focus and behavior expressed through the
existing primitives; they do not name additional runtime components.

## Terms from other frameworks

If you are looking for a familiar concept, this is where it lives in Octopus.

| Elsewhere | In Octopus |
| --- | --- |
| Agent | A worker: the same role, kept as a descriptor a run wears. |
| Session or conversation | An exact interface folder. A session is private vocabulary of the interface that owns it. |
| Heartbeat, tick, scheduler | Time arrives as an artifact through an interface such as a cron. |
| Supervisor, orchestrator | Checkpoint waits and the artifacts that arrive on them. |
| Skill store, memory store | Artifacts bound to runs, edited by ordinary runs under policy. |
| Workflow, pipeline, chain | Runs in an EE exchanging artifacts through bindings. |

The parallel is direct: a browser agent is a web worker, a coding agent is a
coder worker. The word changes because the thing does. An agent is usually a
process; a worker is a description that any run can pick up.
