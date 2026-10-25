const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { collectSupportReport, supportReportText, supportIssueUrl } = require("../electron/clientSupportReport.cjs");

test("reports disclose only selected technical evidence; error text requires explicit inclusion", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-support-"));
  try {
    await fs.mkdir(path.join(root, "logs"));
    await fs.mkdir(path.join(root, "setup-state/operations"), { recursive: true });
    const operation_id = "workspace_setup_test";
    await fs.writeFile(path.join(root, "setup-state/current.json"), JSON.stringify({ operation_id, plan_digest: "same" }));
    await fs.writeFile(path.join(root, `setup-state/operations/${operation_id}.json`), JSON.stringify({ operation_id, plan_digest: "same", plan: { secret: "PRIVATE_PLAN" }, steps: [{ step: "provider", state: "pending" }], last_error: { step: "provider", message: "PRIVATE_ERROR" } }));
    await fs.writeFile(path.join(root, "logs/octopus-client.jsonl"), [
      JSON.stringify({ at: "2026-10-26T00:00:00.000Z", event: "setup.progress", detail: { step: "provider", phase: "verifying", percent: 96, operationId: "PRIVATE_ID" } }),
      "broken line",
      JSON.stringify({ event: "PRIVATE_EVENT", level: "error", detail: { message: "PRIVATE_ERROR", profile: "PRIVATE_PROFILE", stack: "PRIVATE_STACK" } }),
    ].join("\n"));
    const input = { clientRoot: root, profile: "live", application: { version: "0.1.0", revision: null, platform: "darwin", arch: "arm64" }, runtime: { phase: "ready", ready: true, home: "PRIVATE_HOME", launchEnv: { secret: "PRIVATE_ENV" } } };
    const safe = await collectSupportReport(input);
    assert.doesNotMatch(JSON.stringify(safe), /PRIVATE_/);
    assert.equal(safe.setup.journal.steps[0].step, "provider");
    assert.equal(safe.client_logs.sources[1].malformed_lines, 1);
    const detailed = await collectSupportReport({ ...input, includeDetails: true });
    assert.match(supportReportText(detailed, "My description"), /PRIVATE_ERROR/);
    assert.doesNotMatch(JSON.stringify(detailed), /PRIVATE_(PLAN|ENV|HOME|STACK|ID|PROFILE)/);
    assert.doesNotMatch(supportIssueUrl(detailed, "My description"), /PRIVATE_ERROR/);
    for (const profile of [null, "lab"]) {
      const report = await collectSupportReport({ ...input, profile, clientRoot: "/does-not-exist", includeDetails: false });
      assert.equal(report.setup, undefined);
      assert.equal(report.client_logs, undefined);
      assert.equal(report.runtime, undefined);
    }
    await fs.unlink(path.join(root, "logs/octopus-client.jsonl"));
    await fs.symlink(path.join(root, "setup-state/current.json"), path.join(root, "logs/octopus-client.jsonl"));
    assert.equal((await collectSupportReport(input)).client_logs.sources[1].state, "unreadable");
    await fs.unlink(path.join(root, "logs/octopus-client.jsonl"));
    await fs.writeFile(path.join(root, "logs/octopus-client.jsonl"), "x".repeat(300_000) + "\n" + JSON.stringify({ event: "setup.failed", level: "error" }));
    assert.equal((await collectSupportReport(input)).client_logs.sources[1].truncated, true);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});
