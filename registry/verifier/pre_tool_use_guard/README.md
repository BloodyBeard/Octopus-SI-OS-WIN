# pre_tool_use_guard

This verifier checks a `tool.call` artifact before it reaches a worker-environment
door. Its role is similar to a `PreToolUse` hook: the checkpoint names the
verifier in `required_verified_artifacts`, and a passing verdict admits the
configured mark. A failing verdict records `VerificationFailed` and leaves that
checkpoint open.

The package-local `verify.py` reads the artifact payload and accepts these
checkpoint parameters:

- `denied_tools`: exact tool ids to reject.
- `denied_command_substrings`: case-insensitive substrings to reject in the
  payload's command or argument fields.

It rejects an unreadable payload, an invalid JSON object, an artifact whose label
is not `tool.call`, and a missing tool id. It does not execute the tool or provide
a general hook lifecycle. Non-blocking observation belongs to interface code.

`registry/mark/hook.gate.passed/mark.json` declares the success mark for this
check. Allow it in the worker pin's `ceiling.mark_outcomes` and the EE policy,
then name it in the checkpoint's requirement.
