const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const path = require("node:path");
const { inspectLocalPackageDiagnostics } = require("./localPackages.cjs");

const MAX_JSON_BYTES = 4 * 1024 * 1024;
const MAX_REPORT_BYTES = 1024 * 1024;
const DIGEST = /^[a-f0-9]{64}$/;
const SOURCE_ID = /^[A-Za-z0-9._-]+$/;
const REGISTRY_KINDS = new Set(["interface", "worker_environment_kind", "tool", "artifact", "mark", "verifier", "ee_template"]);

async function createClientDoctorReport(options) {
  const report = {
    schema: "octopus.configuration_diagnostic",
    schema_version: 1,
    scope: options.packageId ? "package" : "registry_file",
    disclosure: options.includePaths === true ? "relative_names_and_pointers" : "names_omitted",
    limitations: [
      "Read-only observation; concurrent changes are possible. No recovery or repair is performed.",
      "Differences describe state, not corruption or who changed it. Missing provenance is unknown provenance.",
      "JSON values, active content hashes, absolute roots, logs, credentials and environment variables are omitted.",
      "This report cannot replay model calls or external effects. Nothing is uploaded.",
    ],
  };
  try {
    if (Boolean(options.packageId) === Boolean(options.registryFile)) throw new Error("one scope required");
    report.observation = options.packageId
      ? await inspectLocalPackageDiagnostics({ ...options.roots, packageId: options.packageId, includePaths: options.includePaths })
      : await inspectRegistryFile(options);
    report.status = "observed";
  } catch {
    report.status = "inspection_failed";
    report.next_step = "Check the selected scope, source/library definitions and read access locally; raw errors are omitted.";
  }
  if (Buffer.byteLength(JSON.stringify(report, null, 2)) > MAX_REPORT_BYTES) {
    delete report.observation;
    report.status = "report_limit_exceeded";
    report.next_step = "Select one registry JSON file instead of the whole package.";
  }
  return report;
}

async function inspectRegistryFile(options) {
  const { sourceRegistryRoot, activeRegistryRoot } = options.roots;
  const relative = safeRelativePath(options.registryFile);
  const components = relative.split("/");
  if (components.length < 3 || !REGISTRY_KINDS.has(components[0]) || !relative.endsWith(".json")) {
    throw new Error("select one registry JSON file inside an owner folder");
  }
  for (const root of [sourceRegistryRoot, activeRegistryRoot]) {
    if (!path.isAbsolute(root) || !(await fs.lstat(root)).isDirectory()) throw new Error("invalid registry root");
  }
  const [source, active, manifest, identity] = await Promise.all([
    readJsonSnapshot(sourceRegistryRoot, relative),
    readJsonSnapshot(activeRegistryRoot, relative),
    readJsonSnapshot(activeRegistryRoot, ".octopus-source-manifest.json"),
    readJsonSnapshot(sourceRegistryRoot, ".octopus-source.json"),
  ]);
  let baselineState = manifest.state, baseline = null, identityMatches = null;
  if (manifest.state === "present") {
    const value = manifest.document;
    if (value.schema_version !== 2 || typeof value.source_id !== "string" || !SOURCE_ID.test(value.source_id)
      || typeof value.source_registry !== "string" || !record(value.files)
      || Object.keys(value).some((key) => !["schema_version", "source_id", "source_registry", "files"].includes(key))
      || Object.values(value.files).some((file) => !record(file) || !DIGEST.test(file.sha256)
        || file.mode !== null && (!Number.isInteger(file.mode) || file.mode < 0 || file.mode > 0o777)
        || Object.keys(file).some((key) => !["sha256", "mode"].includes(key)))) {
      baselineState = "invalid";
    } else {
      baseline = Object.hasOwn(value.files, relative) ? value.files[relative] : null;
      identityMatches = identity.state === "present" && identity.document.schema_version === 1
        && typeof identity.document.id === "string" && SOURCE_ID.test(identity.document.id)
        && Object.keys(identity.document).every((key) => ["schema_version", "id"].includes(key))
        ? identity.document.id === value.source_id : null;
    }
  }
  return {
    ...(options.includePaths === true ? { path: relative } : {}),
    origin: baseline ? "baseline_recorded" : baselineState === "missing" || baselineState === "present"
      ? source.state === "missing" && active.state === "present" ? "local_only" : "not_baseline_recorded"
      : "unknown",
    source: { state: source.state },
    active: { state: active.state },
    baseline: { state: baselineState, file_recorded: baseline !== null, source_identity_matches: identityMatches },
    active_matches_source: source.sha256 && active.sha256 ? sameFile(source, active) : null,
    source_matches_baseline: source.sha256 && baseline && identityMatches === true ? sameFile(source, baseline) : null,
    active_matches_baseline: active.sha256 && baseline ? sameFile(active, baseline) : null,
    comparison: "File bytes and permission bits; baseline is the last source sync, not configuration inheritance. Package receipts are inspected with --package.",
  };
}

function sameFile(left, right) {
  return left.sha256 === right.sha256 && (right.mode === null || left.mode === right.mode);
}

function safeRelativePath(value) {
  if (typeof value !== "string" || !value || value.length > 512
    || value.split("/").some((part) => !part || part === "." || part === ".." || /[\\\x00-\x1f\x7f]/.test(part))) {
    throw new Error("invalid relative registry path");
  }
  return value;
}

async function readJsonSnapshot(root, relative) {
  let handle;
  try {
    let directory = root;
    for (const part of ["", ...relative.split("/").slice(0, -1)]) {
      directory = path.join(directory, part);
      if (!(await fs.lstat(directory)).isDirectory()) return { state: "unsafe_path" };
    }
    const file = path.join(root, relative);
    const before = await fs.lstat(file);
    if (!before.isFile()) return { state: "unsafe_path" };
    if (before.size > MAX_JSON_BYTES) return { state: "size_limit_exceeded" };
    handle = await fs.open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = await handle.stat();
    if (!sameStat(before, opened)) return { state: "changed_during_read" };
    const buffer = Buffer.alloc(before.size + 1);
    let length = 0;
    while (length < buffer.length) {
      const { bytesRead } = await handle.read(buffer, length, buffer.length - length, null);
      if (!bytesRead) break;
      length += bytesRead;
    }
    if (length !== before.size || !sameStat(before, await handle.stat()) || !sameStat(before, await fs.lstat(file))) {
      return { state: "changed_during_read" };
    }
    const body = buffer.subarray(0, length);
    let document;
    try {
      document = JSON.parse(body.toString("utf8"));
      if (!record(document) || !finiteJson(document)) return { state: "invalid_json_object" };
    } catch {
      return { state: "invalid_json_object" };
    }
    return { state: "present", sha256: crypto.createHash("sha256").update(body).digest("hex"), mode: before.mode & 0o777, document };
  } catch (error) {
    return { state: error?.code === "ENOENT" ? "missing" : "unreadable" };
  } finally {
    await handle?.close();
  }
}

function sameStat(left, right) {
  return right.isFile() && left.ino === right.ino && left.dev === right.dev && left.size === right.size
    && left.mtimeMs === right.mtimeMs && left.ctimeMs === right.ctimeMs && left.mode === right.mode;
}

function record(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function finiteJson(value) {
  if (typeof value === "number") return Number.isFinite(value);
  return !value || typeof value !== "object" || Object.values(value).every(finiteJson);
}

module.exports = { createClientDoctorReport };
