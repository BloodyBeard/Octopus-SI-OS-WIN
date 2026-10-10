const fs = require("node:fs/promises");
const path = require("node:path");
const { spawn } = require("node:child_process");
const { createHash } = require("node:crypto");

const MAX_PATHS = 5_000;
const MAX_PATH_BYTES = 4_096;
const MAX_FILE_BYTES = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES = 16 * 1024 * 1024;
const MAX_GIT_OUTPUT_BYTES = 1024 * 1024;
const MAX_GIT_INDEX_BYTES = 64 * 1024 * 1024;
const GIT_TIMEOUT_MS = 20_000;
const MAX_SNAPSHOTS = 32;
const branchDiffCache = new Map();

function createWorkerEnvironmentOperatorActions(options) {
  if (typeof options?.resolveSurface !== "function") {
    throw new Error("Worker Environment operator actions require a projected surface resolver");
  }
  const locks = new Map();
  const snapshots = new Map();

  async function inspect(request) {
    const workerEnvironmentId = requiredId(request?.workerEnvironmentId, "workerEnvironmentId");
    const surfaceId = requiredId(request?.surfaceId, "surfaceId");
    const runId = requiredId(request?.runId, "runId");
    const descriptor = await options.resolveSurface(workerEnvironmentId, surfaceId);
    if (descriptor.runId !== runId) throw new Error("Worker Environment no longer belongs to the projected run");
    validateDirectGitSurface(descriptor);
    const root = await projectedGitRoot(descriptor);
    const snapshot = await captureGitSnapshot({ descriptor, root, runId, surfaceId, workerEnvironmentId });
    rememberSnapshot(snapshots, snapshot);
    return publicGitSnapshot(snapshot);
  }

  async function perform(request) {
    const workerEnvironmentId = requiredId(request?.workerEnvironmentId, "workerEnvironmentId");
    const previous = locks.get(workerEnvironmentId) ?? Promise.resolve();
    let release;
    const current = new Promise((resolve) => { release = resolve; });
    const queued = previous.then(() => current);
    locks.set(workerEnvironmentId, queued);
    await previous;
    try {
      return await performLocked(options, request, workerEnvironmentId, snapshots);
    } finally {
      release();
      if (locks.get(workerEnvironmentId) === queued) locks.delete(workerEnvironmentId);
    }
  }

  return { inspect, perform };
}

async function performLocked(options, request, workerEnvironmentId, snapshots) {
  const surfaceId = requiredId(request?.surfaceId, "surfaceId");
  const runId = requiredId(request?.runId, "runId");
  const projectionRevision = requiredId(request?.projectionRevision, "projectionRevision");
  const action = requiredAction(request?.action);
  const requestedPaths = requiredPaths(request?.paths);
  const descriptor = await options.resolveSurface(workerEnvironmentId, surfaceId);
  if (descriptor.runId !== runId) throw new Error("Worker Environment no longer belongs to the projected run");
  let root;
  let changes;
  let directSnapshot = null;
  if (isDirectGitSurface(descriptor)) {
    validateDirectGitSurface(descriptor);
    const key = snapshotKey(workerEnvironmentId, surfaceId, projectionRevision);
    let snapshot = snapshots.get(key);
    if (!snapshot) {
      root = await projectedGitRoot(descriptor);
      const current = await captureGitSnapshot({ descriptor, root, runId, surfaceId, workerEnvironmentId });
      if (current.revision !== projectionRevision) {
        throw new Error("Git worktree changed since this review was loaded; refresh before modifying files");
      }
      snapshot = current;
      rememberSnapshot(snapshots, snapshot);
    }
    if (snapshot.environmentRevision !== descriptor.formationRevision) {
      throw new Error("Worker Environment changed since this review was loaded; refresh before modifying files");
    }
    directSnapshot = snapshot;
    root = snapshot.root;
    changes = projectedChanges({ diffs: snapshot.diffs });
  } else {
    if (!descriptor.revision || descriptor.revision !== projectionRevision) {
      throw new Error("Worker Environment changed since this diff was projected; refresh before modifying files");
    }
    root = await projectedGitRoot(descriptor);
    changes = projectedChanges(descriptor.data);
  }
  const selected = requestedPaths.map((requestedPath) => {
    const change = changes.get(requestedPath);
    if (!change) throw new Error(`Changed path is no longer projected: ${requestedPath}`);
    if (change.binary && action !== "stage" && action !== "unstage") {
      throw new Error(`Binary projected changes cannot be reverted or reapplied from the client: ${requestedPath}`);
    }
    return change;
  });
  const totalBytes = selected.reduce((sum, change) => sum + Buffer.byteLength(change.before) + Buffer.byteLength(change.after), 0);
  if (totalBytes > MAX_TOTAL_BYTES || selected.some((change) => Buffer.byteLength(change.before) > MAX_FILE_BYTES || Buffer.byteLength(change.after) > MAX_FILE_BYTES)) {
    throw new Error("Projected change exceeds the bounded operator-action limit");
  }
  if (directSnapshot && action !== "apply") {
    await assertSelectedGitChangesCurrent({
      descriptor,
      root,
      runId,
      selectedPaths: requestedPaths,
      snapshot: directSnapshot,
      surfaceId,
      workerEnvironmentId,
    });
  }

  if (action === "stage" || action === "unstage") {
    const gitPaths = [...new Set(selected.flatMap((change) => [change.path, change.previousPath].filter(Boolean)))];
    await mutateGitIndex({
      action,
      gitPaths,
      revalidate: directSnapshot
        ? () => assertSelectedGitChangesCurrent({
            descriptor,
            root,
            runId,
            selectedPaths: requestedPaths,
            snapshot: directSnapshot,
            surfaceId,
            workerEnvironmentId,
          })
        : action === "stage"
          ? () => assertProjectedWorktreeCurrent(root, selected)
          : async () => {},
      root,
    });
  } else {
    const gitPaths = [...new Set(selected.flatMap((change) => [change.path, change.previousPath].filter(Boolean)))];
    const stagedPaths = action === "revert" ? await stagedGitPaths(root, gitPaths) : [];
    const indexMutation = stagedPaths.length
      ? await mutateGitIndex({
          action: "unstage",
          gitPaths: stagedPaths,
          revalidate: directSnapshot
            ? () => assertSelectedGitChangesCurrent({
                descriptor,
                root,
                runId,
                selectedPaths: requestedPaths,
                snapshot: directSnapshot,
                surfaceId,
                workerEnvironmentId,
              })
            : () => assertProjectedWorktreeCurrent(root, selected),
          root,
        })
      : null;
    try {
      await rewriteProjectedChanges(root, selected, action);
    } catch (error) {
      if (indexMutation) {
        try {
          await restoreGitIndexMutation(indexMutation);
        } catch (restoreError) {
          throw new Error(`${error instanceof Error ? error.message : String(error)}; restoring the prior Git index failed: ${restoreError instanceof Error ? restoreError.message : String(restoreError)}`);
        }
      }
      throw error;
    }
  }
  return {
    action,
    outsideOctopusEvidence: true,
    paths: requestedPaths,
    projectionRevision,
    runId,
    surfaceId,
    workerEnvironmentId,
  };
}

async function assertSelectedGitChangesCurrent({ descriptor, root, runId, selectedPaths, snapshot, surfaceId, workerEnvironmentId }) {
  const current = await captureGitSnapshot({ descriptor, root, runId, surfaceId, workerEnvironmentId });
  const expectedByPath = new Map(snapshot.diffs.map((change) => [change.path, change]));
  const currentByPath = new Map(current.diffs.map((change) => [change.path, change]));
  for (const selectedPath of selectedPaths) {
    const expected = expectedByPath.get(selectedPath);
    const actual = currentByPath.get(selectedPath);
    if (!expected || !actual || !sameProjectedGitChange(expected, actual)) {
      throw new Error(`Git change changed since this review was loaded; refresh before modifying ${selectedPath}`);
    }
  }
}

async function assertProjectedWorktreeCurrent(root, changes) {
  for (const change of changes) {
    const current = await readFileState(root, change.path);
    const expected = isDeleted(change) ? missingState() : textState(change.after, current.mode);
    if (!statesEqual(current, expected)) {
      throw new Error(`Git change changed since this review was loaded; refresh before modifying ${change.path}`);
    }
    if (!change.previousPath || change.previousPath === change.path) continue;
    const previous = await readFileState(root, change.previousPath);
    if (previous.exists) {
      throw new Error(`Git rename changed since this review was loaded; refresh before modifying ${change.previousPath}`);
    }
  }
}

async function mutateGitIndex({ action, gitPaths, revalidate, root }) {
  const indexPath = await gitIndexPath(root);
  const indexDirectory = path.dirname(indexPath);
  const nonce = `${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  const temporaryIndex = path.join(indexDirectory, `.octopus-index-${nonce}`);
  const temporaryLock = `${temporaryIndex}.lock`;
  const initial = await readGitIndexState(indexPath);
  try {
    if (initial.exists) await fs.writeFile(temporaryIndex, initial.bytes, { flag: "wx", mode: initial.mode || 0o644 });
    const env = { GIT_INDEX_FILE: temporaryIndex };
    await runGitWithEnv(root, action === "stage"
      ? ["add", "--all", "--", ...gitPaths]
      : ["restore", "--staged", "--", ...gitPaths], env);
    await revalidate();
    const prepared = await readGitIndexState(temporaryIndex);
    if (!prepared.exists) throw new Error("Git did not produce a replacement index");
    await installGitIndex(indexPath, initial, prepared);
    return { after: prepared, before: initial, indexPath };
  } finally {
    await fs.rm(temporaryIndex, { force: true }).catch(() => {});
    await fs.rm(temporaryLock, { force: true }).catch(() => {});
  }
}

async function restoreGitIndexMutation(mutation) {
  if (!mutation.before.exists) {
    throw new Error("The prior Git index did not exist and cannot be restored after a partial operator action");
  }
  await installGitIndex(mutation.indexPath, mutation.after, mutation.before);
}

async function gitIndexPath(root) {
  const value = (await runGit(root, ["rev-parse", "--git-path", "index"])).stdout.trim();
  if (!value) throw new Error("Git did not resolve its index path");
  return path.isAbsolute(value) ? path.normalize(value) : path.resolve(root, value);
}

async function readGitIndexState(indexPath) {
  const stat = await fs.lstat(indexPath).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!stat) return { bytes: Buffer.alloc(0), exists: false, mode: 0 };
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("Git index is not a regular file");
  if (stat.size > MAX_GIT_INDEX_BYTES) throw new Error(`Git index exceeds ${MAX_GIT_INDEX_BYTES / (1024 * 1024)} MB`);
  const bytes = await fs.readFile(indexPath);
  const after = await fs.lstat(indexPath);
  if (
    after.isSymbolicLink()
    || !after.isFile()
    || after.dev !== stat.dev
    || after.ino !== stat.ino
    || after.size !== stat.size
    || after.mtimeMs !== stat.mtimeMs
    || after.ctimeMs !== stat.ctimeMs
  ) {
    throw new Error("Git index changed while it was read");
  }
  return { bytes, exists: true, mode: stat.mode & 0o777 };
}

async function installGitIndex(indexPath, initial, prepared) {
  const lockPath = `${indexPath}.lock`;
  let lock;
  try {
    lock = await fs.open(lockPath, "wx", initial.mode || prepared.mode || 0o644);
  } catch (error) {
    if (error?.code === "EEXIST") throw new Error("Git index is busy; refresh after the other Git operation finishes");
    throw error;
  }
  let committed = false;
  try {
    const current = await readGitIndexState(indexPath);
    if (!gitIndexStatesEqual(current, initial)) {
      throw new Error("Git index changed while this action was being prepared; refresh before modifying files");
    }
    await lock.truncate(0);
    await lock.writeFile(prepared.bytes);
    await lock.chmod(initial.mode || prepared.mode || 0o644);
    await lock.sync();
    await lock.close();
    lock = null;
    await fs.rename(lockPath, indexPath);
    committed = true;
  } finally {
    await lock?.close().catch(() => {});
    if (!committed) await fs.rm(lockPath, { force: true }).catch(() => {});
  }
}

function gitIndexStatesEqual(left, right) {
  return left.exists === right.exists
    && (!left.exists || left.mode === right.mode && left.bytes.equals(right.bytes));
}

function sameProjectedGitChange(left, right) {
  return left.path === right.path
    && left.previous_path === right.previous_path
    && left.before === right.before
    && left.after === right.after
    && left.binary === right.binary
    && left.previewOmitted === right.previewOmitted
    && left.stage === right.stage
    && left.staged === right.staged
    && left.unstaged === right.unstaged
    && left.status === right.status
    && left.beforeFingerprint === right.beforeFingerprint
    && left.afterFingerprint === right.afterFingerprint;
}

function isDirectGitSurface(descriptor) {
  return descriptor?.kind === "changes"
    && descriptor?.mode === "direct"
    && stringValue(descriptor?.protocol).toLowerCase() === "git.worktree";
}

function validateDirectGitSurface(descriptor) {
  if (!isDirectGitSurface(descriptor) || descriptor.readOnly === true) {
    throw new Error("Worker Environment did not project a direct Git worktree surface");
  }
  if (!descriptor.formationRevision) throw new Error("Worker Environment Git surface has no formation revision");
}

async function captureGitSnapshot({ descriptor, root, runId, surfaceId, workerEnvironmentId }) {
  const [stagedResult, unstagedResult, untrackedResult, branchResult, upstreamResult] = await Promise.all([
    runGit(root, ["diff", "--cached", "--name-status", "-z", "--find-renames"]),
    runGit(root, ["diff", "--name-status", "-z", "--find-renames"]),
    runGit(root, ["ls-files", "--others", "--exclude-standard", "-z"]),
    runGit(root, ["branch", "--show-current"]),
    runGitOptional(root, ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]),
  ]);
  const indexed = new Map();
  const omittedPaths = [];
  mergeNameStatus(indexed, stagedResult.stdout, "staged");
  mergeNameStatus(indexed, unstagedResult.stdout, "unstaged");
  for (const rawPath of untrackedResult.stdout.split("\0").filter(Boolean)) {
    if (rawPath.endsWith("/")) {
      omittedPaths.push({ path: rawPath.slice(0, -1), reason: "nested-worktree" });
      continue;
    }
    const filePath = safeRelativePath(rawPath);
    const current = indexed.get(filePath) ?? emptyGitChange(filePath);
    current.unstaged = true;
    current.statuses.add("A");
    indexed.set(filePath, current);
  }
  if (indexed.size > MAX_PATHS) {
    throw new Error(`Git review contains ${indexed.size} paths; client limit is ${MAX_PATHS}`);
  }

  const hasHead = Boolean((await runGitOptional(root, ["rev-parse", "--verify", "HEAD"])).stdout.trim());
  const branch = branchResult.stdout.trim() || "HEAD";
  const base = hasHead ? await resolveComparisonBase(root, branch, upstreamResult.stdout.trim()) : "";
  const mergeBase = base ? (await runGitOptional(root, ["merge-base", base, "HEAD"])).stdout.trim() : "";
  const headRevision = hasHead ? (await runGit(root, ["rev-parse", "HEAD"])).stdout.trim() : "";
  let branchDiffs = [];
  let branchError = "";
  if (mergeBase && headRevision) {
    try {
      branchDiffs = await cachedGitRevisionDiffs(root, mergeBase, headRevision);
    } catch (error) {
      branchError = error instanceof Error ? error.message : String(error);
    }
  }
  const rows = [...indexed.values()].sort((left, right) => left.path.localeCompare(right.path));
  let totalBytes = 0;
  const diffs = [];
  for (const row of rows) {
    const headPath = row.previousPath || row.path;
    const added = row.statuses.has("A") || (!hasHead && !row.previousPath);
    const deleted = row.statuses.has("D");
    const head = hasHead ? await readGitObject(root, `HEAD:${headPath}`, added) : missingContent();
    const index = row.staged ? await readGitObject(root, `:${row.path}`, deleted) : head;
    const worktree = row.unstaged ? await readWorktreeContent(root, row.path, deleted) : index;
    const beforeContent = head;
    const afterContent = row.unstaged ? worktree : index;
    const rowBytes = beforeContent.bytes.length + afterContent.bytes.length;
    const previewOmitted = totalBytes + rowBytes > MAX_TOTAL_BYTES;
    if (!previewOmitted) totalBytes += rowBytes;
    const before = previewOmitted ? null : decodedContent(beforeContent);
    const after = previewOmitted ? null : decodedContent(afterContent);
    const binary = previewOmitted || before === null || after === null;
    diffs.push({
      path: row.path,
      previous_path: row.previousPath,
      before: binary ? "" : before,
      after: binary ? "" : after,
      binary,
      previewOmitted,
      stage: row.unstaged ? "unstaged" : "staged",
      staged: row.staged,
      unstaged: row.unstaged,
      status: gitChangeStatus(row, beforeContent.exists, afterContent.exists),
      beforeFingerprint: beforeContent.fingerprint,
      afterFingerprint: afterContent.fingerprint,
    });
  }
  const digest = createHash("sha256");
  for (const row of diffs) {
    digest.update(JSON.stringify({
      path: row.path,
      previous_path: row.previous_path,
      binary: row.binary,
      preview_omitted: row.previewOmitted,
      stage: row.stage,
      staged: row.staged,
      unstaged: row.unstaged,
      status: row.status,
      before_fingerprint: row.beforeFingerprint,
      after_fingerprint: row.afterFingerprint,
    }));
    digest.update("\0");
    digest.update(row.before);
    digest.update("\0");
    digest.update(row.after);
    digest.update("\0");
  }
  digest.update("branch\0");
  digest.update(base);
  digest.update("\0");
  digest.update(branchError);
  digest.update("\0");
  for (const row of branchDiffs) {
    digest.update(JSON.stringify({
      path: row.path,
      previous_path: row.previous_path,
      binary: row.binary,
      status: row.status,
      before_fingerprint: row.beforeFingerprint,
      after_fingerprint: row.afterFingerprint,
    }));
    digest.update("\0");
    digest.update(row.before);
    digest.update("\0");
    digest.update(row.after);
    digest.update("\0");
  }
  digest.update(JSON.stringify(omittedPaths));
  return {
    base,
    branch,
    branchDiffs,
    branchError,
    diffs,
    environmentRevision: descriptor.formationRevision,
    omittedPaths,
    revision: digest.digest("hex"),
    root,
    runId,
    stagedFiles: rows.filter((row) => row.staged).map((row) => row.path),
    surfaceId,
    unstagedFiles: rows.filter((row) => row.unstaged).map((row) => row.path),
    workerEnvironmentId,
  };
}

function publicGitSnapshot(snapshot) {
  return {
    base: snapshot.base,
    branch: snapshot.branch,
    branchDiffs: snapshot.branchDiffs.map(publicGitDiff),
    branchError: snapshot.branchError,
    diffs: snapshot.diffs.map(publicGitDiff),
    omittedPaths: snapshot.omittedPaths,
    projectionRevision: snapshot.environmentRevision,
    revision: snapshot.revision,
    runId: snapshot.runId,
    stagedFiles: snapshot.stagedFiles,
    surfaceId: snapshot.surfaceId,
    unstagedFiles: snapshot.unstagedFiles,
    workerEnvironmentId: snapshot.workerEnvironmentId,
  };
}

function publicGitDiff(row) {
  return {
    path: row.path,
    previous_path: row.previous_path,
    before: row.before,
    after: row.after,
    binary: row.binary,
    preview_omitted: row.previewOmitted,
    stage: row.stage,
    staged: row.staged,
    unstaged: row.unstaged,
    status: row.status,
  };
}

async function resolveComparisonBase(root, branch, upstream) {
  const remoteHead = (await runGitOptional(root, ["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])).stdout.trim();
  const candidates = [remoteHead, upstream].filter(Boolean);
  for (const candidate of [...new Set(candidates)]) {
    if (candidate === branch) continue;
    const commit = (await runGitOptional(root, ["rev-parse", "--verify", `${candidate}^{commit}`])).stdout.trim();
    if (commit) return candidate;
  }
  return "";
}

async function captureGitRevisionDiffs(root, beforeRevision, afterRevision) {
  const result = await runGit(root, ["diff", "--name-status", "-z", "--find-renames", beforeRevision, afterRevision]);
  const indexed = new Map();
  mergeNameStatus(indexed, result.stdout, "branch");
  if (indexed.size > MAX_PATHS) {
    throw new Error(`Branch review contains ${indexed.size} paths; client limit is ${MAX_PATHS}`);
  }
  const rows = [...indexed.values()].sort((left, right) => left.path.localeCompare(right.path));
  let totalBytes = 0;
  const diffs = [];
  for (const row of rows) {
    const beforePath = row.previousPath || row.path;
    const added = row.statuses.has("A");
    const deleted = row.statuses.has("D");
    const beforeContent = await readGitObject(root, `${beforeRevision}:${beforePath}`, added);
    const afterContent = await readGitObject(root, `${afterRevision}:${row.path}`, deleted);
    const rowBytes = beforeContent.bytes.length + afterContent.bytes.length;
    const previewOmitted = totalBytes + rowBytes > MAX_TOTAL_BYTES;
    if (!previewOmitted) totalBytes += rowBytes;
    const before = previewOmitted ? null : decodedContent(beforeContent);
    const after = previewOmitted ? null : decodedContent(afterContent);
    const binary = previewOmitted || before === null || after === null;
    diffs.push({
      path: row.path,
      previous_path: row.previousPath,
      before: binary ? "" : before,
      after: binary ? "" : after,
      binary,
      previewOmitted,
      stage: "branch",
      staged: false,
      unstaged: false,
      status: gitChangeStatus(row, beforeContent.exists, afterContent.exists),
      beforeFingerprint: beforeContent.fingerprint,
      afterFingerprint: afterContent.fingerprint,
    });
  }
  return diffs;
}

async function cachedGitRevisionDiffs(root, beforeRevision, afterRevision) {
  const key = `${root}\0${beforeRevision}\0${afterRevision}`;
  const cached = branchDiffCache.get(key);
  if (cached) {
    branchDiffCache.delete(key);
    branchDiffCache.set(key, cached);
    return cached;
  }
  const diffs = await captureGitRevisionDiffs(root, beforeRevision, afterRevision);
  branchDiffCache.set(key, diffs);
  while (branchDiffCache.size > MAX_SNAPSHOTS) branchDiffCache.delete(branchDiffCache.keys().next().value);
  return diffs;
}

function rememberSnapshot(snapshots, snapshot) {
  const key = snapshotKey(snapshot.workerEnvironmentId, snapshot.surfaceId, snapshot.revision);
  snapshots.delete(key);
  snapshots.set(key, snapshot);
  while (snapshots.size > MAX_SNAPSHOTS) snapshots.delete(snapshots.keys().next().value);
}

function snapshotKey(workerEnvironmentId, surfaceId, revision) {
  return `${workerEnvironmentId}\0${surfaceId}\0${revision}`;
}

function emptyGitChange(filePath) {
  return { path: filePath, previousPath: "", staged: false, unstaged: false, statuses: new Set() };
}

function mergeNameStatus(target, text, stage) {
  const fields = text.split("\0");
  for (let index = 0; index < fields.length;) {
    const status = fields[index++];
    if (!status) continue;
    const code = status[0]?.toUpperCase();
    if (!code) throw new Error("Git returned an invalid name-status record");
    let previousPath = "";
    let filePath;
    if (code === "R" || code === "C") {
      previousPath = safeRelativePath(fields[index++]);
      filePath = safeRelativePath(fields[index++]);
    } else {
      filePath = safeRelativePath(fields[index++]);
    }
    const row = target.get(filePath) ?? emptyGitChange(filePath);
    if (previousPath) row.previousPath = previousPath;
    row[stage] = true;
    row.statuses.add(code);
    target.set(filePath, row);
  }
}

function gitChangeStatus(row, beforeExists, afterExists) {
  if (row.previousPath || row.statuses.has("R") || row.statuses.has("C")) return "renamed";
  if (!beforeExists && afterExists) return "added";
  if (beforeExists && !afterExists) return "deleted";
  return "modified";
}

async function readGitObject(root, objectName, missingAllowed) {
  let sizeResult;
  try {
    sizeResult = await runGit(root, ["cat-file", "-s", objectName]);
  } catch (error) {
    if (missingAllowed) return missingContent();
    throw error;
  }
  const size = Number.parseInt(sizeResult.stdout.trim(), 10);
  if (!Number.isSafeInteger(size) || size < 0) throw new Error(`Git object has an invalid size: ${objectName}`);
  const objectId = (await runGit(root, ["rev-parse", objectName])).stdout.trim();
  if (!/^[a-f0-9]{40,64}$/i.test(objectId)) throw new Error(`Git object has an invalid identity: ${objectName}`);
  if (size > MAX_FILE_BYTES) return binaryContent(Buffer.alloc(0), true, `git:${objectId}`);
  const result = await runGitRaw(root, ["show", "--no-textconv", objectName], MAX_FILE_BYTES + 64 * 1024);
  if (result.stdout.length !== size) throw new Error(`Git object changed while it was read: ${objectName}`);
  return contentState(result.stdout, true, false, `git:${objectId}`);
}

async function readWorktreeContent(root, relativePath, missingAllowed) {
  const target = targetPath(root, relativePath);
  const stat = await fs.lstat(target).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!stat) {
    if (missingAllowed) return missingContent();
    throw new Error(`Git review target disappeared while the snapshot was formed: ${relativePath}`);
  }
  if (stat.isSymbolicLink()) return binaryContent(Buffer.from(await fs.readlink(target)), true);
  if (!stat.isFile()) throw new Error(`Git review target is not a regular file: ${relativePath}`);
  if (stat.size > MAX_FILE_BYTES) {
    const fingerprint = (await runGit(root, ["hash-object", "--no-filters", "--", relativePath])).stdout.trim();
    const after = await fs.lstat(target);
    if (!after.isFile() || after.isSymbolicLink() || after.size !== stat.size || after.mtimeMs !== stat.mtimeMs) {
      throw new Error(`Git review target changed while it was fingerprinted: ${relativePath}`);
    }
    return binaryContent(Buffer.alloc(0), true, `git:${fingerprint}`);
  }
  const bytes = await fs.readFile(target);
  const after = await fs.lstat(target);
  if (!after.isFile() || after.isSymbolicLink() || after.size !== stat.size || after.mtimeMs !== stat.mtimeMs) {
    throw new Error(`Git review target changed while it was read: ${relativePath}`);
  }
  return contentState(bytes, true, false);
}

function missingContent() {
  return contentState(Buffer.alloc(0), false, false, "missing");
}

function binaryContent(bytes, oversized, fingerprint) {
  return contentState(bytes, true, oversized, fingerprint);
}

function contentState(bytes, exists, oversized, fingerprint) {
  return {
    bytes,
    exists,
    oversized,
    fingerprint: fingerprint || `sha256:${createHash("sha256").update(bytes).digest("hex")}`,
  };
}

function decodedContent(content) {
  if (content.oversized || content.bytes.includes(0)) return null;
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(content.bytes);
  } catch {
    return null;
  }
}

async function runGitOptional(cwd, args) {
  const result = await runGitRawOptional(cwd, args);
  return {
    stdout: result.stdout.toString("utf8"),
    stderr: result.stderr.toString("utf8"),
  };
}

async function stagedGitPaths(root, paths) {
  const result = await runGit(root, ["diff", "--cached", "--name-only", "-z", "--", ...paths]);
  const selected = new Set(paths);
  return result.stdout.split("\0").filter((entry) => entry && selected.has(entry));
}

async function projectedGitRoot(descriptor) {
  const rootValue = requiredString(descriptor.root || descriptor.environmentRoot, "projected Worker Environment root");
  if (!path.isAbsolute(rootValue)) throw new Error("Projected Worker Environment root must be absolute");
  const stat = await fs.lstat(rootValue);
  if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error("Projected Worker Environment root must be a regular directory");
  const root = await fs.realpath(rootValue);
  const gitTop = (await runGit(root, ["rev-parse", "--show-toplevel"])).stdout.trim();
  const resolvedGitTop = await fs.realpath(gitTop);
  if (resolvedGitTop !== root) throw new Error("Projected Worker Environment root is not the exact Git worktree root");
  return root;
}

function projectedChanges(data) {
  const rows = Array.isArray(data?.diffs) ? data.diffs : Array.isArray(data?.changes) ? data.changes : [];
  const changes = new Map();
  for (const row of rows) {
    if (!row || typeof row !== "object" || Array.isArray(row)) continue;
    const changedPath = safeRelativePath(row.path ?? row.file ?? row.name);
    if (!changedPath || changes.has(changedPath)) throw new Error("Projected changes contain a missing or duplicate path");
    const previousPathValue = stringValue(row.previous_path ?? row.previousPath ?? row.old_path ?? row.oldPath);
    const previousPath = previousPathValue ? safeRelativePath(previousPathValue) : "";
    const before = typeof row.before === "string" ? row.before : typeof row.old === "string" ? row.old : null;
    const after = typeof row.after === "string" ? row.after : typeof row.new === "string" ? row.new : null;
    if (before === null || after === null) throw new Error(`Projected change has no complete before/after content: ${changedPath}`);
    const status = stringValue(row.status ?? row.state ?? row.change_type ?? row.changeType).toLowerCase();
    changes.set(changedPath, {
      after,
      before,
      binary: row.binary === true,
      path: changedPath,
      previousPath,
      status,
    });
  }
  if (!changes.size) throw new Error("Worker Environment surface has no projected changes");
  return changes;
}

async function rewriteProjectedChanges(root, changes, action) {
  const plans = [];
  for (const change of changes) plans.push(await changePlan(root, change, action));
  const touched = new Map();
  for (const plan of plans) {
    for (const operation of plan) {
      if (touched.has(operation.path)) throw new Error(`Projected changes overlap on one operator-action target: ${operation.path}`);
      touched.set(operation.path, operation.observed);
    }
  }
  for (const plan of plans) {
    for (const operation of plan) await assertExpectedState(root, operation.path, operation.expected, operation.observed);
  }
  const completed = [];
  try {
    for (const plan of plans) {
      for (const operation of plan) {
        const written = await writeFileState(root, operation.path, operation.next, operation.observed);
        completed.push({ operation, written });
      }
    }
  } catch (error) {
    const rollbackErrors = [];
    for (const completedOperation of [...completed].reverse()) {
      const target = completedOperation.operation.path;
      try {
        await writeFileState(root, target, touched.get(target), completedOperation.written);
      } catch (rollbackError) {
        rollbackErrors.push(`${target}: ${rollbackError instanceof Error ? rollbackError.message : String(rollbackError)}`);
      }
    }
    if (rollbackErrors.length) {
      throw new Error(`${error instanceof Error ? error.message : String(error)}; rollback failed: ${rollbackErrors.join("; ")}`);
    }
    throw error;
  }
}

async function changePlan(root, change, action) {
  const applying = action === "apply";
  if (change.previousPath && change.previousPath !== change.path) {
    const sourcePath = applying ? change.previousPath : change.path;
    const targetPath = applying ? change.path : change.previousPath;
    const sourceText = applying ? change.before : change.after;
    const targetText = applying ? change.after : change.before;
    const source = await readFileState(root, sourcePath);
    const target = await readFileState(root, targetPath);
    if (!matchesText(source, sourceText) || target.exists) {
      if (!source.exists && matchesText(target, targetText)) return [];
      throw new Error(`Projected rename no longer matches the worktree: ${change.previousPath} → ${change.path}`);
    }
    return [
      { path: targetPath, expected: target, observed: target, next: textState(targetText, source.mode) },
      { path: sourcePath, expected: source, observed: source, next: missingState() },
    ];
  }

  const current = await readFileState(root, change.path);
  const before = textState(change.before, current.mode);
  const after = textState(change.after, current.mode);
  const addition = isAdded(change);
  const deletion = isDeleted(change);
  const expected = applying ? (addition ? missingState() : before) : (deletion ? missingState() : after);
  const next = applying ? (deletion ? missingState() : after) : (addition ? missingState() : before);
  if (statesEqual(current, next)) return [];
  if (!statesEqual(current, expected)) throw new Error(`Projected change no longer matches the worktree: ${change.path}`);
  return [{ path: change.path, expected, observed: current, next }];
}

function isAdded(change) {
  return ["added", "add", "new", "created", "a"].includes(change.status) || (!change.before && Boolean(change.after));
}

function isDeleted(change) {
  return ["deleted", "delete", "removed", "remove", "d"].includes(change.status) || (Boolean(change.before) && !change.after);
}

async function readFileState(root, relativePath) {
  const target = targetPath(root, relativePath);
  const stat = await fs.lstat(target).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!stat) return missingState();
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`Operator action target is not a regular file: ${relativePath}`);
  if (stat.size > MAX_FILE_BYTES) throw new Error(`Operator action target exceeds ${MAX_FILE_BYTES / (1024 * 1024)} MB: ${relativePath}`);
  const bytes = await fs.readFile(target);
  const after = await fs.lstat(target);
  if (
    after.isSymbolicLink()
    || !after.isFile()
    || after.dev !== stat.dev
    || after.ino !== stat.ino
    || after.size !== stat.size
    || after.mtimeMs !== stat.mtimeMs
    || after.ctimeMs !== stat.ctimeMs
  ) {
    throw new Error(`Operator action target changed while it was read: ${relativePath}`);
  }
  return {
    bytes,
    exists: true,
    identity: fileIdentity(after),
    mode: after.mode & 0o777,
  };
}

async function assertExpectedState(root, relativePath, expected, observed) {
  const current = await readFileState(root, relativePath);
  if (!statesEqual(current, expected) || !observedStatesEqual(current, observed)) {
    throw new Error(`Operator action target changed before mutation: ${relativePath}`);
  }
}

async function writeFileState(root, relativePath, state, observed) {
  const target = targetPath(root, relativePath);
  const parent = path.dirname(target);
  const parentStat = await fs.lstat(parent);
  if (parentStat.isSymbolicLink() || !parentStat.isDirectory()) throw new Error(`Operator action parent is not a regular directory: ${relativePath}`);
  const resolvedParent = await fs.realpath(parent);
  assertInside(root, resolvedParent);
  if (!state.exists) {
    await assertExpectedState(root, relativePath, observed, observed);
    await fs.unlink(target);
    const written = await readFileState(root, relativePath);
    if (written.exists) throw new Error(`Operator action failed to remove its target: ${relativePath}`);
    return written;
  }
  const temporary = path.join(parent, `.${path.basename(target)}.octopus-operator-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`);
  try {
    await fs.writeFile(temporary, state.bytes, { flag: "wx", mode: state.mode || 0o644 });
    await assertExpectedState(root, relativePath, observed, observed);
    await fs.rename(temporary, target);
  } finally {
    await fs.rm(temporary, { force: true }).catch(() => {});
  }
  const written = await readFileState(root, relativePath);
  if (!statesEqual(written, state)) throw new Error(`Operator action replacement did not match its prepared state: ${relativePath}`);
  return written;
}

function targetPath(root, relativePath) {
  const target = path.resolve(root, relativePath);
  assertInside(root, target);
  return target;
}

function assertInside(root, target) {
  const relative = path.relative(root, target);
  if (relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative))) return;
  throw new Error("Operator action target must be a file inside the projected worktree root");
}

function textState(text, mode) {
  return { bytes: Buffer.from(text, "utf8"), exists: true, identity: null, mode: mode || 0o644 };
}

function missingState() {
  return { bytes: Buffer.alloc(0), exists: false, identity: null, mode: 0 };
}

function matchesText(state, text) {
  return state.exists && state.bytes.equals(Buffer.from(text, "utf8"));
}

function statesEqual(left, right) {
  return left.exists === right.exists && (!left.exists || left.bytes.equals(right.bytes));
}

function observedStatesEqual(left, right) {
  if (left.exists !== right.exists) return false;
  if (!left.exists) return true;
  return left.mode === right.mode
    && left.bytes.equals(right.bytes)
    && left.identity != null
    && right.identity != null
    && left.identity.dev === right.identity.dev
    && left.identity.ino === right.identity.ino
    && left.identity.size === right.identity.size
    && left.identity.mtimeMs === right.identity.mtimeMs
    && left.identity.ctimeMs === right.identity.ctimeMs;
}

function fileIdentity(stat) {
  return {
    dev: stat.dev,
    ino: stat.ino,
    size: stat.size,
    mtimeMs: stat.mtimeMs,
    ctimeMs: stat.ctimeMs,
  };
}

async function runGit(cwd, args) {
  const result = await runGitRaw(cwd, args);
  return {
    stdout: result.stdout.toString("utf8"),
    stderr: result.stderr.toString("utf8"),
  };
}

async function runGitWithEnv(cwd, args, env) {
  const result = await runGitRaw(cwd, args, MAX_GIT_OUTPUT_BYTES, env);
  return {
    stdout: result.stdout.toString("utf8"),
    stderr: result.stderr.toString("utf8"),
  };
}

async function runGitRawOptional(cwd, args) {
  try {
    const result = await runGitRaw(cwd, args);
    return { ...result, ok: true };
  } catch (error) {
    return { error, ok: false, stderr: Buffer.alloc(0), stdout: Buffer.alloc(0) };
  }
}

function runGitRaw(cwd, args, maxOutputBytes = MAX_GIT_OUTPUT_BYTES, extraEnv = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn("git", args, { cwd, env: { ...process.env, GIT_OPTIONAL_LOCKS: "0", ...extraEnv }, stdio: ["ignore", "pipe", "pipe"] });
    const stdout = [];
    const stderr = [];
    let outputBytes = 0;
    let settled = false;
    const timeout = setTimeout(() => {
      child.kill("SIGKILL");
      finish(new Error(`Git operator action exceeded ${GIT_TIMEOUT_MS / 1000} seconds`));
    }, GIT_TIMEOUT_MS);
    function collect(target, chunk) {
      outputBytes += chunk.length;
      if (outputBytes > maxOutputBytes) {
        child.kill("SIGKILL");
        finish(new Error("Git operator action exceeded the bounded output limit"));
        return;
      }
      target.push(chunk);
    }
    function finish(error, result) {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (error) reject(error);
      else resolve(result);
    }
    child.stdout.on("data", (chunk) => collect(stdout, chunk));
    child.stderr.on("data", (chunk) => collect(stderr, chunk));
    child.on("error", (error) => finish(error));
    child.on("close", (code, signal) => {
      const out = Buffer.concat(stdout);
      const err = Buffer.concat(stderr);
      if (code === 0) finish(null, { stdout: out, stderr: err });
      else finish(new Error(`Git operator action failed (${signal || code}): ${(err.length ? err : out).toString("utf8").trim() || "no diagnostic output"}`));
    });
  });
}

function requiredPaths(value) {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_PATHS) {
    throw new Error(`Operator action requires between 1 and ${MAX_PATHS} projected paths`);
  }
  const paths = value.map(safeRelativePath);
  if (new Set(paths).size !== paths.length) throw new Error("Operator action paths must be unique");
  return paths;
}

function safeRelativePath(value) {
  const text = stringValue(value).replaceAll("\\", "/");
  if (!text || Buffer.byteLength(text) > MAX_PATH_BYTES || text.includes("\0") || path.isAbsolute(text)) {
    throw new Error("Operator action path must be a bounded relative path");
  }
  const parts = text.split("/");
  if (parts.some((part) => !part || part === "." || part === "..")) {
    throw new Error("Operator action path cannot contain empty or traversing segments");
  }
  return parts.join("/");
}

function requiredAction(value) {
  if (!["apply", "revert", "stage", "unstage"].includes(value)) throw new Error("Unsupported Worker Environment operator action");
  return value;
}

function requiredId(value, label) {
  const text = requiredString(value, label);
  if (text.length > 512 || /[\u0000-\u001f]/.test(text)) throw new Error(`${label} is invalid`);
  return text;
}

function requiredString(value, label) {
  const text = stringValue(value);
  if (!text) throw new Error(`${label} is required`);
  return text;
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

module.exports = { createWorkerEnvironmentOperatorActions };
