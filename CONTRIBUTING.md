# Contributing

Octopus is a fully public initiative. There is no commercial plan behind it and
there will not be one: no paid tier, no hosted version, no company waiting at
the end. The motivation is simple: everyone should end up with a
superintelligence of their own.

So contributions are open. Any pull request is welcome, and so is any idea.

- **Ideas** go to
  [Discussions](https://github.com/miuuyy/Octopus-SI-OS/discussions/categories/ideas),
  or straight into a pull request if you would rather show than tell.
- **Bugs** normally go to an [issue](https://github.com/miuuyy/Octopus-SI-OS/issues/new/choose)
  first, then a pull request that references it. The issue is where the
  failure gets reproduced and agreed on before anyone fixes it. A very small,
  focused fix for an obvious bug can go straight to a PR: explain the defect,
  why it qualifies for this exception, and how you verified the fix.

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

Solve one underlying problem per PR. Include the client, runtime, descriptor,
test, and documentation changes needed for that problem, and explain their
relationship when it is not obvious. Submit unrelated fixes, features, or
cleanup separately. Scope is judged by purpose, not a line or file count.

Describe the problem, your change, and how you checked it. Run the relevant
[development checks](docs/development.md#checks), and add a regression check
when it can catch the reported failure. State what you did not verify; a local
test, a packaged build, and a real provider reply establish different things.

If an agent wrote any of the code, end the PR description with one line naming
the model or models and the harness used to write it. For example:
`Written with: <model> via <harness>.`

The PR template's **Architecture fit** section is required only when a proposal
changes architectural responsibilities, contracts, or mechanisms. Read
[Architecture](docs/architecture.md) and the relevant
[contracts](docs/contracts/README.md), then briefly explain why the approach
fits the whole system and why a simpler use of existing primitives is not enough.
An ordinary fix that restores an existing contract does not need this section.

For a new package, application, extension, or driver, complete **Use and need**:
explain who benefits and why existing options are insufficient, and show your own
practical use in Octopus. Include the setup, task, result, and anything untested;
for visual extensions, follow [Client UI evidence](#client-ui-evidence). A build or tool-list
check alone is not enough. Niche or hard-to-test integrations receive individual
maintainer review, and visual extensions require a visual review before acceptance.

A working implementation is not the only consideration: we also assess whether
the capability belongs in Octopus and whether its approach keeps the architecture
coherent. A useful report can lead to a different fix rather than merging its PR.

### Client UI evidence

Changes to the client's appearance or interaction require clear before/after
screenshots from the packaged app. Capture the original state before editing,
and show the same scenario at a comparable window size after the change. For
a new screen, show the existing entry point and the resulting screen.

Include a short recording when motion, timing, transitions, or interaction
details cannot be demonstrated with screenshots. Attach or link the evidence
in the PR; do not commit screenshots or recordings used only for PR review.
Redact private content. Client changes with no visible or interaction change
do not need visual evidence; explain their verification in the PR instead.

## Labels

Maintainers assign labels during the first review; contributors do not need
label permissions to submit. Each issue gets one priority (`P0`–`P3`), and each
PR gets either `urgent` or `non-urgent`. Add `feature` for new capabilities and
`architecture` for architectural changes or questions requiring an architectural
decision. Platform labels are used when the affected platform is known.
Labels describe the work; they do not imply approval or a released fix.
