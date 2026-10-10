# Writing a mark package

A mark is a **tick**: the ledger fact "required verified artifact X was closed
by artifact Y through verifier Z". Neither the model nor an interface emits a
mark - the runtime records it after a verifier passes an artifact at a
checkpoint. A failed check creates **no** mark; the ledger gets a
`VerificationFailed` movement instead.

`registry/mark/*` is vocabulary only: a dictionary of known outcomes, with
nothing executable.

Background: [Marks and verifiers](../../docs/contracts/VERIFIERS.md).

## Where it goes

```text
registry/mark/<id>/mark.json
```

## mark.json

```json
{
  "id": "coder.build.ok",
  "package": "coder.build.ok",
  "class": "success"
}
```

`id` equals the folder name, or loading fails closed. `package` equals `id`.
`class` is the outcome's vocabulary class.

There are **no** `verifier`, `evidence_required`, or `emit` fields here. The
verifier is chosen in the checkpoint contract in run JSON (see
[../verifier/GUIDE.md](../verifier/GUIDE.md)), and the evidence is the artifact
that closed the requirement.

## How a mark enters the system

1. Declare the id here - the vocabulary.
2. Allow it in `ceiling.mark_outcomes` of the System worker pin that needs it
   (`registry/interface/system_worker_<name>/policy.json`). Boot fails closed
   if a worker interface references a mark id that does not exist.
3. Reference it from a checkpoint contract in run JSON, in
   `required_verified_artifacts[].mark`.

**Grant.** Only a run whose formed grant contains this mark id in
`mark_outcomes` can close it. Narrowing follows the EE chain inward.

**Coordination.** Another run may wait on this mark: a parked checkpoint wakes
when the mark fact lands in the ledger.
