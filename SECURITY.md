# Security policy

Do not open public issues containing an Octopus Home, ledger rows, interface
state, provider sign-in files, API keys, prompts, tool results, or local
filesystem paths. Redact diagnostics before sharing them.

Octopus runs a model with tools on your machine. Tools, verifiers, and
interface processes are sandboxed to the authority their descriptors declare,
and every refusal is recorded - but a run that holds write or execute tools can
still be steered by untrusted content it reads. Read the complete
[security model](docs/security-model.md) before granting a project real
capabilities.

A Home and the provider session under `~/.codex` are readable by anything
running as your user. If another person or an untrusted process can act as that
user, treat both as compromised and sign in again.

Report a vulnerability through the repository's private
[Security Advisory](https://github.com/miuuyy/Octopus-SI-OS/security/advisories/new)
flow. Do not publish a proof of concept that exposes credentials or arbitrary
local tool execution before it has been addressed.
