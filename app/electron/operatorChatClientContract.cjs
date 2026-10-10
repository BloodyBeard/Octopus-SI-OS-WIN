const OPERATOR_CHAT_PACKAGE_ID = "world_operator_chat";
const OPERATOR_CHAT_CLIENT_INTERFACE_PREFIX = `${OPERATOR_CHAT_PACKAGE_ID}_`;
const DIGEST = /^[a-f0-9]{64}$/;
const path = require("node:path");
const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const { createHash } = require("node:crypto");
const { isDeepStrictEqual } = require("node:util");
const {
  registerLocalPackageMutationGuard,
  withLocalPackageInstallation,
} = require("./localPackages.cjs");

function createOperatorChatClientLoader(options) {
  if (typeof options?.packageRoots !== "function" || typeof options?.clientPath !== "function") {
    throw new Error("operator chat client loader requires packageRoots and clientPath resolvers");
  }
  let loaded = null;
  let pending = null;

  const assertMutationAllowed = (packageIds) => {
    const targets = new Set(Array.isArray(packageIds) ? packageIds : []);
    if (!targets.has(OPERATOR_CHAT_PACKAGE_ID) || !loaded) return;
    throw operatorChatClientRestartError();
  };
  registerLocalPackageMutationGuard({
    stateRoot: options.packageRoots().stateRoot,
    guard: assertMutationAllowed,
  });

  const load = () => {
    if (loaded) return Promise.resolve(loaded.client);
    if (pending) return pending;
    pending = withLocalPackageInstallation({
      ...options.packageRoots(),
      packageId: OPERATOR_CHAT_PACKAGE_ID,
    }, async (installation, snapshot) => {
      const clientPath = path.resolve(options.clientPath());
      const proof = await verifyOperatorChatClientInstallation(installation, snapshot, clientPath);
      const client = require(clientPath);
      loaded = { client, proof };
      return client;
    }).catch((error) => {
      loaded = null;
      throw error;
    }).finally(() => {
      pending = null;
    });
    return pending;
  };

  return Object.freeze({
    assertMutationAllowed,
    load,
    loadedProof() {
      return loaded?.proof ?? null;
    },
  });
}

async function verifyOperatorChatClientInstallation(installation, snapshot, clientPath) {
  const proof = requireMaterializedOperatorChatClientOwner({ packages: [installation] });
  const files = snapshot?.members?.find((member) => member.registry_kind === "interface" && member.owner === OPERATOR_CHAT_PACKAGE_ID);
  const configuration = await requireCurrentClientFiles(files, clientPath);
  return { ...proof, configuration };
}

function requireMaterializedOperatorChatClientOwner(catalog) {
  const packages = Array.isArray(catalog?.packages) ? catalog.packages : [];
  const candidate = packages.find((entry) => entry?.id === OPERATOR_CHAT_PACKAGE_ID);
  const memberStates = Array.isArray(candidate?.member_states) ? candidate.member_states : [];
  const clientOwner = memberStates.find((member) => (
    member?.registry_kind === "interface"
    && member?.owner === OPERATOR_CHAT_PACKAGE_ID
  ));
  const sourceDigest = stringValue(clientOwner?.source_digest);
  const installedDigest = stringValue(clientOwner?.installed_digest);
  if (
    candidate?.available !== true
    || clientOwner?.materialized !== true
    || !DIGEST.test(sourceDigest)
    || !DIGEST.test(installedDigest)
  ) {
    throw operatorChatClientContractError(
      "The installed world_operator_chat interface owner is missing or stale. Repair that exact interface owner before using chat.",
    );
  }
  return {
    installedDigest,
    owner: OPERATOR_CHAT_PACKAGE_ID,
    registryKind: "interface",
    sourceDigest,
  };
}

async function requireCurrentClientFiles(member, clientPath) {
  const sourceFiles = member?.source_files ?? [];
  const installedFiles = member?.installed_files ?? [];
  const installed = new Map(installedFiles.map((file) => [file.relative_path, file]));
  const source = new Map(sourceFiles.map((file) => [file.relative_path, file]));
  if (!source.has("client.cjs") || installed.get("client.cjs")?.path !== clientPath
    || source.size !== installed.size || source.size !== sourceFiles.length || installed.size !== installedFiles.length) {
    throw operatorChatClientContractError("The installed chat client file set differs from its package source.");
  }
  for (const [name, expected] of source) {
    const actual = installed.get(name);
    if (!actual || actual.mode !== expected.mode
      || (!["client.json", "life.json"].includes(name) && (actual.sha256 !== expected.sha256 || actual.size !== expected.size))) {
      throw operatorChatClientContractError(`The installed chat client file '${name}' differs from its package source. Repair that file before using chat.`);
    }
  }
  let life = null;
  if (source.has("life.json")) {
    const expectedFile = source.get("life.json"), actualFile = installed.get("life.json");
    const [expected, actual] = await Promise.all([readPinnedConfiguration(expectedFile), readPinnedConfiguration(actualFile)]);
    const executableConfiguration = (value) => {
      if (!Array.isArray(value?.processes)) throw operatorChatClientContractError("Chat lifecycle requires declared processes.");
      return { ...value, processes: value.processes.map((process) => {
        const { sandbox_mode, ...authority } = process.authority ?? {};
        if (sandbox_mode !== undefined && !["enforced", "disabled"].includes(sandbox_mode)) {
          throw operatorChatClientContractError("Chat lifecycle has an invalid sandbox mode.");
        }
        return { ...process, authority };
      }) };
    };
    if (!isDeepStrictEqual(executableConfiguration(actual), executableConfiguration(expected))) {
      throw operatorChatClientContractError("The installed chat lifecycle executable configuration differs from its package source.");
    }
    life = { sourceDigest: expectedFile.sha256, installedDigest: actualFile.sha256 };
  }
  if (!source.has("client.json")) return null;
  const sourceFile = source.get("client.json"), installedFile = installed.get("client.json");
  const [expected, actual] = await Promise.all([readPinnedConfiguration(sourceFile), readPinnedConfiguration(installedFile)]);
  const defaults = (value) => {
    if (!value || typeof value !== "object" || Array.isArray(value)
      || !value.run || typeof value.run !== "object" || Array.isArray(value.run)) {
      throw operatorChatClientContractError("Chat client configuration requires an explicit run object.");
    }
    const { ee, worker, context_artifact_bindings, ...protocol } = value.run;
    for (const id of [ee, worker]) {
      if (typeof id !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(id)) {
        throw operatorChatClientContractError("Chat run defaults require exact EE and worker identities.");
      }
    }
    if (!Array.isArray(context_artifact_bindings) || context_artifact_bindings.some((binding) => (
      !binding || typeof binding !== "object" || Array.isArray(binding)
      || Object.keys(binding).length !== 1 || typeof binding.id !== "string"
      || !binding.id || binding.id.trim() !== binding.id || /[/\\\0]/.test(binding.id)
    )) || new Set(context_artifact_bindings.map((binding) => binding.id)).size !== context_artifact_bindings.length) {
      throw operatorChatClientContractError("Chat run context defaults require unique explicit artifact bindings.");
    }
    return { ...value, run: protocol };
  };
  if (!isDeepStrictEqual(defaults(actual), defaults(expected))) {
    throw operatorChatClientContractError("The installed chat protocol or executable configuration differs from its package source.");
  }
  return { sourceDigest: sourceFile.sha256, installedDigest: installedFile.sha256, life };
}

async function readPinnedConfiguration(file) {
  const handle = await fs.open(file.path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size !== file.size || stat.size > 2 * 1024 * 1024) {
      throw operatorChatClientContractError("Chat client configuration changed during verification.");
    }
    const bytes = await handle.readFile();
    if (createHash("sha256").update(bytes).digest("hex") !== file.sha256) {
      throw operatorChatClientContractError("Chat client configuration changed during verification.");
    }
    return JSON.parse(bytes.toString("utf8"));
  } finally {
    await handle.close();
  }
}

function isOperatorChatClientInterfaceCandidate(value) {
  return typeof value === "string" && value.startsWith(OPERATOR_CHAT_CLIENT_INTERFACE_PREFIX);
}

function assertOperatorChatArtifactBatch(value) {
  if (!value || typeof value !== "object" || Array.isArray(value) || !Array.isArray(value.artifacts)) {
    throw operatorChatClientContractError("world_operator_chat returned an invalid live artifact batch.");
  }
  for (const artifact of value.artifacts) {
    if (!artifact || typeof artifact !== "object" || Array.isArray(artifact)) {
      throw operatorChatClientContractError("world_operator_chat returned an invalid live artifact.");
    }
    for (const field of ["from_interface_id", "from_interface_role"]) {
      const fieldValue = artifact[field];
      if (typeof fieldValue !== "string" || !fieldValue || fieldValue.trim() !== fieldValue) {
        throw operatorChatClientContractError(`world_operator_chat live artifact is missing exact ${field}.`);
      }
    }
    if (artifact.source === "movement") {
      for (const field of ["movement_seq", "recorded_at_unix_ms"]) {
        if (!Number.isSafeInteger(artifact[field]) || artifact[field] < 0) {
          throw operatorChatClientContractError(`world_operator_chat movement artifact has invalid ${field}.`);
        }
      }
      for (const field of ["artifact_id", "content_hash", "checkpoint_id"]) {
        if (typeof artifact[field] !== "string") {
          throw operatorChatClientContractError(`world_operator_chat movement artifact has invalid ${field}.`);
        }
      }
      continue;
    }
    if (artifact.source !== "stream") {
      throw operatorChatClientContractError("world_operator_chat live artifact has an invalid source.");
    }
    for (const field of [
      "stream_seq",
      "byte_start_offset",
      "byte_end_offset",
      "source_movement_seq",
      "recorded_at_unix_ms",
    ]) {
      if (!Number.isSafeInteger(artifact[field]) || artifact[field] < 0) {
        throw operatorChatClientContractError(`world_operator_chat stream artifact has invalid ${field}.`);
      }
    }
    if (!DIGEST.test(stringValue(artifact.stream_id))) {
      throw operatorChatClientContractError("world_operator_chat stream artifact has invalid stream_id.");
    }
    if (artifact.byte_end_offset <= artifact.byte_start_offset) {
      throw operatorChatClientContractError("world_operator_chat stream artifact has invalid byte offsets.");
    }
    for (const field of ["artifact_id", "content_hash", "checkpoint_id", "movement_seq"]) {
      if (Object.hasOwn(artifact, field)) {
        throw operatorChatClientContractError(`world_operator_chat stream artifact must not carry ${field}.`);
      }
    }
  }
  return value;
}

function operatorChatClientContractError(message) {
  return Object.assign(new Error(message), {
    code: "operator_chat_client_contract_mismatch",
  });
}

function operatorChatClientRestartError() {
  return Object.assign(new Error(
    "world_operator_chat cannot be changed after its client has loaded. Restart Octopus before changing this package.",
  ), {
    code: "operator_chat_client_restart_required",
  });
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

module.exports = {
  assertOperatorChatArtifactBatch,
  createOperatorChatClientLoader,
  isOperatorChatClientInterfaceCandidate,
  requireMaterializedOperatorChatClientOwner,
  verifyOperatorChatClientInstallation,
};
