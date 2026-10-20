module.exports = { clientPinnedContextItem, materializePinnedExcerpt, normalizePinnedContextItems, runJsonProcess, updateOperatorChatPins, validatePinnedExcerpt };

const { spawn } = require("node:child_process");
const path = require("node:path");
const { assertRunOwnsArtifact, chatIdentityFromRunRecord } = require("./conversation.cjs");
const { resolveRunRecordsRoot } = require("./operatorMessages.cjs");
const { commitClientWrites, fileExists, resolveRegistryRoot, uniqueArtifactBindings, withRunAuthoringLock } = require("./publication.cjs");
const { readConfig, resolveInstallationRoot, resolvePackageExecutable } = require("./roots.cjs");
const { absolutePath, isObject, nonNegativeInteger, readJsonDocumentIfPresent, requiredArray, requiredString, safeSegment, sha256Hex } = require("./values.cjs");

async function updateOperatorChatPins(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const installationRoot = resolveInstallationRoot(options);
  const artifactMaterializer = resolvePackageExecutable(
    config,
    installationRoot,
    resolveRegistryRoot(options),
    "artifact_materializer",
  );
  const runId = safeSegment(request?.runId, "runId");
  const sourceArtifactId = requiredString(request?.artifactId, "artifactId");
  if (typeof request?.pinned !== "boolean") throw new Error("pinned must be a boolean");
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const chatRunPath = path.join(runsDir, `${runId}.json`);

  return withRunAuthoringLock(runId, async () => {
    const document = await readJsonDocumentIfPresent(chatRunPath);
    if (!document) throw new Error(`operator chat run '${runId}' does not exist`);
    const identity = chatIdentityFromRunRecord(config, runId, document.value);
    const state = document.value.interface_state?.[identity.interfaceId];
    if (!isObject(state)) {
      throw new Error(`operator chat run '${runId}' has no state for '${identity.interfaceId}'`);
    }
    const current = state.pinned_artifact_bindings === undefined
      ? []
      : uniqueArtifactBindings(requiredArray(
        state.pinned_artifact_bindings,
        "interface_state pinned_artifact_bindings",
      ));
    const currentItems = normalizePinnedContextItems(state.pinned_context_items);
    let artifactId = sourceArtifactId;
    let nextItems = currentItems;
    if (request.pinned) {
      await assertRunOwnsArtifact({
        octopusHome,
        runId,
        interfaceId: identity.interfaceId,
        artifactId: sourceArtifactId,
      });
      if (request.excerpt !== undefined) {
        const excerpt = validatePinnedExcerpt(request.excerpt);
        const materialized = await materializePinnedExcerpt(
          octopusHome,
          artifactMaterializer,
          runId,
          sourceArtifactId,
          excerpt,
        );
        artifactId = materialized.artifactId;
        nextItems = [
          ...currentItems.filter((item) => item.artifact_id !== artifactId),
          materialized.item,
        ];
      }
    } else {
      nextItems = currentItems.filter((item) => item.artifact_id !== artifactId);
      if (!current.some((binding) => binding.id === artifactId)) {
        throw new Error(`artifact '${artifactId}' is not pinned in operator chat run '${runId}'`);
      }
    }
    const next = request.pinned
      ? uniqueArtifactBindings([...current, { id: artifactId }])
      : current.filter((binding) => binding.id !== artifactId);
    state.pinned_artifact_bindings = next;
    state.pinned_context_items = nextItems;
    const content = `${JSON.stringify(document.value, null, 2)}\n`;
    // notes2 2026-07-16: client administrative edits stay outside runtime evidence; the owner may revisit this boundary.
    const clientWrites = await commitClientWrites([{
      filePath: chatRunPath,
      content,
      tag: `run-pins:${runId}`,
      expectedBeforeHash: sha256Hex(Buffer.from(document.text)),
    }], options);
    return {
      runId,
      pinnedArtifactIds: next.map((binding) => binding.id),
      pinnedContextItems: nextItems.map(clientPinnedContextItem),
      clientWrites,
    };
  });
}

function validatePinnedExcerpt(value) {
  if (!isObject(value)) throw new Error("excerpt must be an object");
  const unknown = Object.keys(value).filter((key) => !["sourceMessageId", "text", "start", "end", "role"].includes(key));
  if (unknown.length > 0) throw new Error(`excerpt contains unknown fields: ${unknown.join(", ")}`);
  const text = requiredString(value.text, "excerpt.text");
  const start = nonNegativeInteger(value.start, "excerpt.start");
  const end = nonNegativeInteger(value.end, "excerpt.end");
  if (end <= start) throw new Error("excerpt.end must be greater than excerpt.start");
  const role = requiredString(value.role, "excerpt.role");
  if (!["user", "assistant", "system"].includes(role)) throw new Error("excerpt.role is unsupported");
  return {
    sourceMessageId: requiredString(value.sourceMessageId, "excerpt.sourceMessageId"),
    text,
    start,
    end,
    role,
  };
}

async function materializePinnedExcerpt(octopusHome, materializer, runId, sourceArtifactId, excerpt) {
  const blobsDir = path.join(octopusHome, "ledger", "blobs");
  if (!(await fileExists(materializer))) {
    throw new Error(
      `operator chat artifact materializer is absent at its declared package path '${materializer}'`,
    );
  }
  const output = await runJsonProcess(materializer, {
    blobs_root: blobsDir,
    label: "operator.pinned_excerpt",
    payload: {
      text: excerpt.text,
      role: excerpt.role,
      source_run_id: runId,
      source_artifact_id: sourceArtifactId,
      source_message_id: excerpt.sourceMessageId,
      source_range: { start: excerpt.start, end: excerpt.end },
    },
  });
  const artifactId = requiredString(output?.artifact_id, "artifact materializer artifact_id");
  return {
    artifactId,
    item: {
      artifact_id: artifactId,
      source_artifact_id: sourceArtifactId,
      source_message_id: excerpt.sourceMessageId,
      text: excerpt.text,
      start: excerpt.start,
      end: excerpt.end,
      role: excerpt.role,
    },
  };
}

async function runJsonProcess(binary, request) {
  const stdout = await new Promise((resolve, reject) => {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"] });
    const output = [];
    const errors = [];
    child.stdout.on("data", (chunk) => output.push(chunk));
    child.stderr.on("data", (chunk) => errors.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve(Buffer.concat(output).toString("utf8"));
        return;
      }
      reject(new Error(Buffer.concat(errors).toString("utf8").trim() || `process exited with code ${code}`));
    });
    child.stdin.on("error", reject);
    child.stdin.end(JSON.stringify(request));
  });
  const response = JSON.parse(stdout);
  if (!isObject(response)) throw new Error("process returned a non-object response");
  return response;
}

function normalizePinnedContextItems(value) {
  if (value === undefined) return [];
  return requiredArray(value, "interface_state pinned_context_items").map((item, index) => {
    if (!isObject(item)) throw new Error(`pinned_context_items[${index}] must be an object`);
    const role = requiredString(item.role, `pinned_context_items[${index}].role`);
    if (!["user", "assistant", "system"].includes(role)) {
      throw new Error(`pinned_context_items[${index}].role is unsupported`);
    }
    return {
      artifact_id: requiredString(item.artifact_id, `pinned_context_items[${index}].artifact_id`),
      source_artifact_id: requiredString(item.source_artifact_id, `pinned_context_items[${index}].source_artifact_id`),
      source_message_id: requiredString(item.source_message_id, `pinned_context_items[${index}].source_message_id`),
      text: requiredString(item.text, `pinned_context_items[${index}].text`),
      start: nonNegativeInteger(item.start, `pinned_context_items[${index}].start`),
      end: nonNegativeInteger(item.end, `pinned_context_items[${index}].end`),
      role,
    };
  });
}

function clientPinnedContextItem(item) {
  return {
    artifactId: item.artifact_id,
    sourceArtifactId: item.source_artifact_id,
    sourceMessageId: item.source_message_id,
    text: item.text,
    start: item.start,
    end: item.end,
    role: item.role,
  };
}
