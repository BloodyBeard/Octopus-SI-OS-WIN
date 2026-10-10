# review_rubric verifier

Deterministic structural check on a code-review artifact, standing on the edge of
a code-review checkpoint. It pairs with the `artifact-review-rubric` context
artifact: that artifact tells the reviewing worker how to review and what output
shape to emit; this verifier proves the emitted review has that shape before the
checkpoint's review mark is admitted.

## What it checks (shape, fail-closed)

The review payload must be a JSON object carrying:

- `findings`: an array. Each entry must be an object with a non-empty `title`, a
  non-empty `body`, and — when present — a `priority` inside the 0-3 ladder
  (P0-P3).
- `overall_correctness`: exactly `"patch is correct"` or `"patch is incorrect"`.

A missing, empty, unreadable, or non-JSON payload fails closed, as does a payload
missing the findings array or a valid verdict (VERIFIERS.md §3: no silent pass on
any malformed leg). With `params.require_findings: true`, an empty findings array
also fails closed; the default (`false`) admits a clean review that legitimately
found nothing.

## What it does NOT check (honest scope boundary)

It does not re-judge whether the findings are semantically right, whether the
reviewer applied the rubric's bug-flagging criteria correctly, or whether the
verdict matches the actual diff. That judgment lives in the reviewing model, not
in deterministic code. This is a real shape gate, not a hidden label-only pass —
but it is a shape gate, not a quality gate. A model-backed rubric-quality check
would be a separate, larger verifier package.

## Wiring

Referenced from a checkpoint's `required_verified_artifacts`:

```json
{
  "mark": "<a review mark id in registry/mark/>",
  "artifact_label": "<the reviewer's review artifact label>",
  "verifier": "review_rubric",
  "params": { "require_findings": false }
}
```

The `mark` is authored by whoever wires the review checkpoint and must exist in
`registry/mark/` and sit inside the run's granted `mark_outcomes`; this package
ships no mark of its own.
