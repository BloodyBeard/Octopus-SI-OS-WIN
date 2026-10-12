const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const {
  inspectLocalPackageCatalog,
  inspectLocalPackageInstallation,
} = require("./localPackages.cjs");
const { verifyOperatorChatClientInstallation } = require("./operatorChatClientContract.cjs");

const SAFE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const CHECKS = Object.freeze([
  Object.freeze({
    id: "operator_chat.client_owner",
    packageId: "world_operator_chat",
    validate: verifyOperatorChatClientInstallation,
  }),
]);

async function runClientDoctor(options) {
  const roots = packageRoots(options?.roots);
  const inspectInstallation = options?.inspectInstallation ?? inspectLocalPackageInstallation;
  const inspectCatalog = options?.inspectCatalog ?? inspectLocalPackageCatalog;
  if (typeof inspectInstallation !== "function" || typeof inspectCatalog !== "function") {
    throw new Error("client doctor requires read-only package inspectors");
  }
  const findings = [];
  let checks = 0;
  for (const check of CHECKS) {
    checks += 1;
    try {
      const installation = await inspectInstallation({ ...roots, packageId: check.packageId, includeFileSnapshot: true });
      await check.validate(installation, installation.file_snapshot, path.join(roots.activeRegistryRoot, "interface", check.packageId, "client.cjs"));
    } catch (error) {
      findings.push(finding(
        "client.package_owner_stale",
        check.id,
        errorMessage(error),
        `Inspect package '${check.packageId}', back up its exact active owners, then perform an owner-authorized package repair with pinned source and installed digests and retain the receipt.`,
      ));
    }
  }

  const runDiagnosis = await diagnoseZeroLedgerGhostRuns(roots);
  checks += runDiagnosis.checks;
  findings.push(...runDiagnosis.findings);

  const catalog = await inspectCatalog(roots);
  for (const error of catalog.errors ?? []) {
    checks += 1;
    findings.push(finding(
      "context.package_catalog_invalid",
      "local package catalog",
      errorMessage(error),
      "Correct the exact local package manifest in source before any production repair.",
    ));
  }
  for (const entry of catalog.packages ?? []) {
    if (!entry.materialized && !entry.partially_materialized && !["valid", "stale"].includes(entry.receipt_state)) continue;
    checks += 1;
    if (entry.installed === true && entry.receipt_state === "valid" && !entry.error) continue;
    findings.push(finding(
      "package.installation_inconsistent",
      entry.id,
      entry.error || `Installed package '${entry.id}' has receipt state '${entry.receipt_state}' and materialized=${entry.materialized === true}.`,
      `Inspect package '${entry.id}' and its exact source/installed differences. With the runtime stopped, the client update path adopts unchanged package owners; conflicting local edits require an explicit reviewed package repair.`,
      { package_id: entry.id, receipt_state: entry.receipt_state, materialized: entry.materialized === true,
        partially_materialized: entry.partially_materialized === true, source_digest: entry.source_digest,
        installed_digest: entry.installed_digest, receipt_digest: entry.receipt_digest },
    ));
  }
  const bound = await collectBoundContextArtifacts(roots);
  const owners = contextPackageOwners(catalog.packages ?? []);
  for (const [artifactId, bindings] of bound) {
    checks += 1;
    const diagnosis = await diagnoseBoundContextArtifact(roots, owners, artifactId, bindings);
    if (diagnosis) findings.push(diagnosis);
  }
  const errors = findings.map(renderFinding);
  return {
    schema_version: 2,
    checks,
    passed: checks - findings.length,
    findings,
    errors,
  };
}

async function diagnoseZeroLedgerGhostRuns(roots) {
  const octopusHome = path.dirname(roots.activeRegistryRoot);
  const host = await readJsonIfPresent(path.join(octopusHome, "host.json"));
  if (!host) return { checks: 0, findings: [] };
  const runsRoot = host.host_manifest_version === 1 && typeof host.root_refs?.run_records_root === "string"
    ? host.root_refs.run_records_root
    : "";
  if (!path.isAbsolute(runsRoot)) {
    return {
      checks: 1,
      findings: [finding(
        "run.records_root_unavailable",
        "host.json",
        "Doctor cannot resolve one absolute run_records_root from host.json.",
        "Correct the explicit host.json run_records_root before diagnosing or repairing run records.",
      )],
    };
  }
  const running = [];
  let checks = 0;
  for (const entry of await directoryEntries(runsRoot)) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    checks += 1;
    const runPath = path.join(runsRoot, entry.name);
    const run = await readJsonIfPresent(runPath);
    if (run?.status === "running" && typeof run.id === "string" && SAFE_ID.test(run.id)) {
      running.push({ id: run.id, path: runPath });
    }
  }
  if (running.length === 0) return { checks, findings: [] };
  const ledgerPath = path.join(octopusHome, "ledger", "movements.jsonl");
  let ledger;
  try {
    ledger = await fs.readFile(ledgerPath, "utf8");
  } catch (error) {
    return {
      checks,
      findings: [finding(
        "run.ledger_unavailable",
        ledgerPath,
        `Doctor cannot prove whether ${running.length} running run record(s) have movement evidence: ${errorMessage(error)}`,
        "Restore read access to the authoritative movements.jsonl before changing any run status.",
      )],
    };
  }
  const observed = new Set();
  for (const [index, line] of ledger.split("\n").entries()) {
    if (!line) continue;
    let movement;
    try {
      movement = JSON.parse(line);
    } catch (error) {
      throw new Error(`doctor ledger line ${index + 1} is malformed: ${errorMessage(error)}`);
    }
    for (const runId of movementRunIds(movement)) observed.add(runId);
  }
  return {
    checks,
    findings: running.filter((run) => !observed.has(run.id)).map((run) => finding(
      "run.running_without_ledger_movement",
      run.id,
      `Run '${run.id}' is authored with status='running', but no durable ledger movement names that run.`,
      `Create a new task and leave '${run.id}' untouched. If the owner later requests cleanup, first back up that exact record, confirm the turn was never admitted, then change only status to 'idle' and retain a repair receipt.`,
      { run_record_path: run.path, ledger_path: ledgerPath, movement_count: 0 },
    )),
  };
}

function movementRunIds(movement) {
  const ids = new Set();
  const metadata = movement?.invocation_metadata;
  const run = movement?.kind && typeof movement.kind === "object" && movement.kind.MarkOutcomeAdmitted
    ? metadata?.run : metadata?.runtime?.run;
  if (SAFE_ID.test(run ?? "")) ids.add(run);
  return ids;
}

async function collectBoundContextArtifacts(roots) {
  const bound = new Map();
  const interfaceRoot = path.join(roots.activeRegistryRoot, "interface");
  for (const entry of await directoryEntries(interfaceRoot)) {
    if (!entry.isDirectory() || !SAFE_ID.test(entry.name)) continue;
    const policy = await readJsonIfPresent(path.join(interfaceRoot, entry.name, "policy.json"));
    if (!policy) continue;
    for (const artifactId of Array.isArray(policy.role_artifacts) ? policy.role_artifacts : []) {
      addBinding(bound, artifactId, `worker policy ${entry.name}`);
    }
  }

  const octopusHome = path.dirname(roots.activeRegistryRoot);
  const host = await readJsonIfPresent(path.join(octopusHome, "host.json"));
  const runsRoot = typeof host?.root_refs?.run_records_root === "string" && path.isAbsolute(host.root_refs.run_records_root)
    ? host.root_refs.run_records_root
    : path.join(octopusHome, "state", "runs");
  for (const entry of await directoryEntries(runsRoot)) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const run = await readJsonIfPresent(path.join(runsRoot, entry.name));
    if (!run || !Array.isArray(run.checkpoints) || !Array.isArray(run.active_checkpoints)) continue;
    const active = new Set(run.active_checkpoints.filter((value) => typeof value === "string"));
    for (const checkpoint of run.checkpoints) {
      if (!checkpoint || typeof checkpoint !== "object" || !active.has(checkpoint.id)) continue;
      for (const binding of Array.isArray(checkpoint.artifact_bindings) ? checkpoint.artifact_bindings : []) {
        const artifactId = binding && typeof binding === "object" ? binding.id : "";
        if (await isContextArtifact(roots, artifactId)) {
          addBinding(bound, artifactId, `run ${String(run.id || entry.name)} checkpoint ${String(checkpoint.id)}`);
        }
      }
    }
  }
  return [...bound.entries()].sort(([left], [right]) => left.localeCompare(right));
}

function contextPackageOwners(packages) {
  const owners = new Map();
  for (const packageRecord of packages) {
    const members = Array.isArray(packageRecord.member_states) && packageRecord.member_states.length > 0
      ? packageRecord.member_states
      : Array.isArray(packageRecord.members) ? packageRecord.members : [];
    for (const member of members) {
      if (member.registry_kind !== "artifact") continue;
      const entries = owners.get(member.owner) ?? [];
      entries.push({ packageRecord, member });
      owners.set(member.owner, entries);
    }
  }
  return owners;
}

async function diagnoseBoundContextArtifact(roots, owners, artifactId, bindings) {
  const source = await artifactDescriptorState(roots.sourceRegistryRoot, artifactId);
  const active = await artifactDescriptorState(roots.activeRegistryRoot, artifactId);
  const evidence = { bindings: [...bindings].sort(), source, active };
  const ownerEntries = owners.get(artifactId) ?? [];
  const nextStep = `Back up active artifact '${artifactId}', establish one exact source package owner, then perform a separately owner-authorized production repair with pinned digests and retain its receipt.`;
  if (ownerEntries.length === 0) {
    if (!active.present) {
      return finding(
        "context.bound_artifact_active_missing",
        artifactId,
        `Bound context artifact '${artifactId}' has no active descriptor.`,
        "Inspect the authored binding and restore its intended descriptor or correct that exact binding; source ownership is not required for a local artifact.",
        evidence,
      );
    }
    return null;
  }
  if (ownerEntries.length > 1) {
    return finding(
      "context.bound_artifact_ambiguous_owner",
      artifactId,
      `Bound context artifact '${artifactId}' is claimed by multiple local packages: ${ownerEntries.map(({ packageRecord }) => packageRecord.id).join(", ")}.`,
      "Choose one canonical source package owner before any production repair.",
      evidence,
    );
  }
  const [{ packageRecord, member }] = ownerEntries;
  evidence.package_id = packageRecord.id;
  evidence.package_receipt_state = packageRecord.receipt_state ?? "missing";
  evidence.member_source_digest = member.source_digest ?? "";
  evidence.member_installed_digest = member.installed_digest ?? "";
  if (!source.present || !member.source_digest) {
    return finding(
      "context.bound_artifact_source_missing",
      artifactId,
      `Bound context artifact '${artifactId}' is owned by package '${packageRecord.id}', but its canonical source owner is missing or unreadable.`,
      nextStep,
      evidence,
    );
  }
  if (!active.present || member.materialized !== true || !member.installed_digest) {
    return finding(
      "context.bound_artifact_active_missing",
      artifactId,
      `Bound context artifact '${artifactId}' is owned by package '${packageRecord.id}', but the active owner is missing.`,
      nextStep,
      evidence,
    );
  }
  if (member.source_digest !== member.installed_digest) {
    return finding(
      "context.bound_artifact_digest_stale",
      artifactId,
      `Bound context artifact '${artifactId}' is stale: package '${packageRecord.id}' source digest ${member.source_digest} differs from active digest ${member.installed_digest}.`,
      nextStep,
      evidence,
    );
  }
  if (packageRecord.receipt_state !== "valid") {
    return finding(
      "context.bound_artifact_receipt_invalid",
      artifactId,
      `Bound context artifact '${artifactId}' matches source bytes, but package '${packageRecord.id}' has receipt state '${String(packageRecord.receipt_state || "missing")}'.`,
      nextStep,
      evidence,
    );
  }
  return null;
}

async function isContextArtifact(roots, artifactId) {
  if (!SAFE_ID.test(artifactId)) return false;
  const source = await artifactDescriptorState(roots.sourceRegistryRoot, artifactId);
  const active = await artifactDescriptorState(roots.activeRegistryRoot, artifactId);
  return source.label === "context.reference"
    || active.label === "context.reference"
    || !source.present && !active.present;
}

async function artifactDescriptorState(registryRoot, artifactId) {
  if (!SAFE_ID.test(artifactId)) return { present: false, digest: "", label: "" };
  const file = path.join(registryRoot, "artifact", artifactId, "artifact.json");
  try {
    const bytes = await fs.readFile(file);
    const descriptor = JSON.parse(bytes.toString("utf8"));
    return {
      present: true,
      digest: crypto.createHash("sha256").update(bytes).digest("hex"),
      label: typeof descriptor.label === "string" ? descriptor.label : "",
    };
  } catch (error) {
    if (error?.code === "ENOENT") return { present: false, digest: "", label: "" };
    throw new Error(`context artifact '${artifactId}' descriptor '${file}' cannot be inspected: ${errorMessage(error)}`);
  }
}

function addBinding(bound, artifactId, owner) {
  if (typeof artifactId !== "string" || !SAFE_ID.test(artifactId)) return;
  const bindings = bound.get(artifactId) ?? new Set();
  bindings.add(owner);
  bound.set(artifactId, bindings);
}

async function directoryEntries(root) {
  try {
    return await fs.readdir(root, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
}

async function readJsonIfPresent(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw new Error(`doctor input '${file}' cannot be read as JSON: ${errorMessage(error)}`);
  }
}

function finding(code, subject, reason, nextStep, evidence = {}) {
  return { code, subject, reason, next_step: nextStep, evidence };
}

function renderFinding(value) {
  return `${value.code} [${value.subject}]: ${value.reason} Next manual step: ${value.next_step}`;
}

function packageRoots(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("client doctor package roots must be an object");
  }
  return Object.fromEntries([
    "sourceRegistryRoot",
    "activeRegistryRoot",
    "libraryRoot",
    "stateRoot",
  ].map((field) => {
    const candidate = value[field];
    if (typeof candidate !== "string" || !path.isAbsolute(candidate)) {
      throw new Error(`client doctor ${field} must be an absolute path`);
    }
    return [field, path.resolve(candidate)];
  }));
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

async function main() {
  const args = process.argv.slice(2);
  const usage = "usage: clientDoctor.cjs <source-registry> <active-registry> <library> <state> [--package <id> | --registry-file <relative.json>] [--include-paths]";
  if (args.length === 1 && args[0] === "--help") {
    process.stdout.write(`${usage}\nScoped reports omit names and values by default. --include-paths reveals relative names and configuration pointers, never values.\nExit 0 for a completed observation (not a health verdict), 2 if inspection fails. Without a scope, the existing local doctor runs; its output is not a redacted export.\n`);
    return;
  }
  if (args.length !== 4 && !([6, 7].includes(args.length) && ["--package", "--registry-file"].includes(args[4])
    && (args.length === 6 || args[6] === "--include-paths"))) {
    throw new Error(usage);
  }
  const roots = {
    sourceRegistryRoot: args[0],
    activeRegistryRoot: args[1],
    libraryRoot: args[2],
    stateRoot: args[3],
  };
  if (args.length > 4) {
    const { createClientDoctorReport } = require("./clientDoctorReport.cjs");
    const report = await createClientDoctorReport({
      roots,
      ...(args[4] === "--package" ? { packageId: args[5] } : { registryFile: args[5] }),
      includePaths: args[6] === "--include-paths",
    });
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
    if (report.status !== "observed") process.exitCode = 2;
    return;
  }
  const report = await runClientDoctor({ roots });
  process.stdout.write(`${JSON.stringify(report)}\n`);
  if (report.errors.length > 0) process.exitCode = 1;
}

if (require.main === module) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack : String(error)}\n`);
    process.exitCode = 2;
  });
}

module.exports = {
  runClientDoctor,
};
