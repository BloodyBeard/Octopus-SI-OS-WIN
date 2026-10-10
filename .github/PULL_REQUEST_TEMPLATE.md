## What this changes

<!-- Say what changes and why. For a bug fix, link the issue it was reported in.
For an idea, a discussion link helps but is not required. -->

Fixes #

## Evidence

<!-- Give the reproduction before the change and the exact result afterward. For a runtime change,
the ledger rows or refusal that show the behavior. For UI, a screenshot or short recording. -->

## Scope and invariants

- [ ] I read and followed `CONTRIBUTING.md`.
- [ ] No unrelated cleanup is mixed in.
- [ ] No branch on a worker pin, provider, model string, or artifact label was added to the runtime.
- [ ] New capability is descriptor data, or code inside its concrete owner folder under `registry/`.
- [ ] Failure stays explicit: no silent fallback, compatibility shim, or placeholder success.
- [ ] New side effects leave an artifact and a movement; no mark is claimed without a verifier.
- [ ] Client-only behavior is not routed through runtime policy.

## Verification

- [ ] I ran the checks in `docs/development.md` that cover this change.
- [ ] I added or updated one focused contract check for a behavior change.
- [ ] For user-visible changes, I tested the packaged app with a dedicated test Home.
- [ ] I did not commit a Home, credentials, generated binaries, reference clones, or private logs.

## What was not verified

<!-- Name the platform, provider, and flows you actually exercised. Write "not run" for anything not verified. -->
