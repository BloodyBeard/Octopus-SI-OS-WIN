# Contributing

Octopus is a fully public initiative. There is no commercial plan behind it and
there will not be one: no paid tier, no hosted version, no company waiting at
the end. The motivation is simple: everyone should end up with a
superintelligence of their own.

So contributions are open. Any pull request is welcome, and so is any idea.

- **Ideas** go to
  [Discussions](https://github.com/miuuyy/Octopus-SI-OS/discussions/categories/ideas),
  or straight into a pull request if you would rather show than tell.
- **Bugs** go to an [issue](https://github.com/miuuyy/Octopus-SI-OS/issues/new/choose)
  first, then a pull request that references it. The issue is where the
  failure gets reproduced and agreed on before anyone fixes it.

Before filing a bug, work through [TROUBLESHOOTING.md](TROUBLESHOOTING.md).
Never upload a Home, a ledger, provider sign-in files, or unredacted logs.

Octopus was created and is maintained by [@miuuyy](https://github.com/miuuyy).

## Core invariants

- **The kernel stays small and blind.** It knows no provider, channel, tool,
  model, or artifact by name. A branch on a worker pin, a brand, a model
  string, or an artifact label is a bug.
- **Capabilities are descriptors.** If an existing driver or room can express
  it, it is JSON. New executable behavior lives in its owner folder under
  `registry/`.
- **Fail closed.** No silent fallback and no placeholder success. A clear error
  beats something that almost works.
- **Every effect leaves evidence.** It crosses a declared interface under
  policy and is recorded. Marks come from verifiers, never from a model's
  claim.
- Homes, credentials, provider sign-in state, private logs, and absolute user
  paths never enter the repository.

A change that needs a new primitive is still welcome. Open it as an idea first,
because it changes the architecture for everyone.

## Before opening a pull request

1. Run `npm ci --prefix app` and the checks that cover your change. See
   [Development](docs/development.md#checks). At minimum:

   ```sh
   cargo check --locked --workspace --all-targets
   (cd app && npx tsc --noEmit --incremental false)
   node app/scripts/check-public-source.cjs
   ```

2. Add a focused regression test for a behavior change.
3. Do not commit credentials, a Home, local absolute paths, or generated logs.
4. Preserve fail-closed behavior. A failure must not pick another path or claim
   success.
5. Keep claims factual. Say what you ran and what you could not verify.
