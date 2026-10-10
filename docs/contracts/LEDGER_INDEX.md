# The ledger and its indexes

The system's core record is not a message bus or an event store. It is the
**log of artifacts and their movements**. Nothing in the framework works
without artifacts, and every meaningful action is an artifact crossing an
interface boundary, so this from→to ledger is what makes the whole system
traceable and replayable.

## The movement log

`ledger/movements.jsonl` is the source of truth: append-only, flushed per row,
replayable. Every row is one movement between two exact interface folders.

The following shows selected fields; content hashes and metadata are abbreviated:

```json
{
  "seq": 0,
  "lamport": 1,
  "recorded_at_unix_ms": 1791528512739,
  "trace": "trace:run_home:cp_operator_input",
  "kind": "InterfaceArrival",
  "from": { "Interface": "world_operator_chat_home" },
  "to":   { "Interface": "system_worker_operator_home" },
  "artifact": { "id": "operator.input:ac16…", "content": [172, 22, 24] },
  "invocation_metadata": { "runtime": { "run": "run_home", "checkpoint": "cp_operator_input", "epoch": 0, "ee": "ee_existence" } }
}
```

Endpoints are always interfaces. The run, checkpoint, EE, worker, grant, mark
outcome, and binding snapshot are typed provenance metadata *on* the edge,
never alternate endpoint kinds.

In durable invocation rows, `invocation_metadata.grant_content` references the
stored grant projection. Decoding resolves that content into `grant`; formation
still recomputes authority at the next arrival. See the
[artifact and metadata example](ARTIFACTS.md#payload-manifest-and-delivery-metadata).

| Kind | Meaning |
| --- | --- |
| `InterfaceArrival` | An artifact was admitted at a checkpoint pinning the arriving interface. |
| `InterfaceOutbound` | An artifact left toward an interface a checkpoint names as outbound. |
| `ArtifactMoved` | A plain artifact movement between two interfaces. |
| `MarkOutcomeAdmitted` | A verifier passed; the named mark is now a fact. |
| `VerificationFailed` | The artifact reached the checkpoint and its verifier did not pass. |
| `DeliveryRefused` | The gate turned the movement back, with a typed reason. |

Refusal reasons are a closed vocabulary: `no_binding`, `policy`,
`verification_failed`, `unknown_interface`, `unknown_run`, `stale_epoch`,
`checkpoint_spent`, `settled`, `malformed`.

A refusal is a movement like any other - logged and replayable. Asking for more
than your place allows is an observable event, not a silent drop.

Live streams - thinking, progress, liveness - are projections over this log.
They are not a second source of truth.

Every row carries a trace. Traces map back to the originating interface and
the goal the work belongs to.

### Reading a movement in the client

The optional desktop client's **Dashboard → Movements** is one reader of this
record. Expand a row to see the from/to interfaces, trace, and artifact
reference alongside its manifest and payload reference. The selected
`DeliveryRefused` below records a refused transfer; neighboring arrival or
outbound rows describe other edges, not success for this one.

![Movements with a DeliveryRefused row expanded beside its artifact and payload references](../images/client/movements.png)

This is the visible counterpart of the
[payload, manifest, and metadata example](ARTIFACTS.md#payload-manifest-and-delivery-metadata).
Another client can read the same ledger and indexes.

## Indexes

Indexes make the log fast to query. They never replace it. The design copies how an operating system holds indexes: the kernel
keeps in-memory caches over the data on disk, rebuilt at mount and updated per
write, and the searchable index is a separate, disposable file beside the data.
A database is a userspace application on top of the filesystem, never part of
the kernel.

### Layer 1 - the kernel index

In memory, read-through. The arrival path's idempotency questions - has this
exact checkpoint generation already admitted its artifacts, is this arrival
already recorded - are answered from an in-memory projection of arrival rows.

A checkpoint generation is `(run, checkpoint, epoch)`. Re-arming requires a new
checkpoint id or a new epoch. The index holds a watermark and catches up
incrementally before answering: the first question after boot folds the whole
ledger once; every later question folds only the delta. No arrival ever rescans
the biography of the world.

- One index pairs with one ledger for the process lifetime. A shrunk or
  non-contiguous log is a typed refusal; an arrival never answers from a
  phantom cache.
- A required arrival counts as admitted only when one exact, causally later
  `MarkOutcomeAdmitted` proves it. One mark proves one pending arrival;
  indistinguishable pending arrivals fail closed instead of sharing proof.
- Kernel decisions consult this layer only. Never SQLite.

### Layer 2 - the Spotlight index

`ledger/ledger-index.sqlite`, beside the log. One row per movement, with no
payloads - the sequence number addresses the authoritative line. Indexed by
run, endpoint, owning interface, artifact id, admitted mark, and kind. WAL
mode: one writer, any number of readers.

Who it is for - readers, in any language:

- an interface catching up on **its** movements from its own cursor, as one
  indexed lookup instead of parsing the whole log. The cursor itself stays the
  interface's own file; it never lives in the index.
- the client, rendering run summaries and movement history.
- a Python or JavaScript process, which opens SQLite natively.

What keeps it a cache:

- **Disposable.** Delete the file and nothing is lost. The next boot verifies
  the cache against the ledger and rebuilds it from zero on any mismatch. A
  different schema generation is dropped, never migrated.
- **Never blocks the ledger.** If ingest fails, the writer stops touching the
  broken cache, the arrival and its append proceed, and the next boot rebuilds.
  Fail-closed for truth, fail-open for cache.
- **Never consulted for a kernel decision**, and never written except as a
  projection of rows already appended.

## Caches over the JSON on disk

The ledger is one truth surface. The descriptor and run JSON on disk is the
other, and the same discipline applies: the arrival path answers from an
in-memory projection refreshed only where a file changed.

**The run-records index.** A reverse projection from interface folder to the
active checkpoints, in any run, that pin it. Before each resolution it stats
each run file and re-parses only those whose size or modification time changed.
A malformed run record becomes its own inert slot and contributes no target;
one broken run never fails another interface's arrival. This index only
accelerates discovery - the actual admission still reloads the target run
record from disk and re-forms its grant.

When a run file changes, already-admitted outputs for its newly visible
bindings are replayed through the ordinary per-edge gates. A repaired recipient
catches up while the runtime stays up, with no resend, no poller, and no
delivery queue.

**The registry snapshot guard.** The in-memory registry is reloaded only when
the descriptor tree actually changed: a metadata-only walk folds a fingerprint
over the descriptor kinds, and the heavy parse and validation run only when it
moves. If the fingerprint cannot be computed, the snapshot reloads
unconditionally.

Both are caches that rebuild from the log.

## Restart

A run is rebuilt from its record and the
movements recorded against it. A stopped process loses no state.
