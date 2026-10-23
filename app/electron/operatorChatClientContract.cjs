const OPERATOR_CHAT_PACKAGE_ID = "world_operator_chat";
const OPERATOR_CHAT_CLIENT_INTERFACE_PREFIX = `${OPERATOR_CHAT_PACKAGE_ID}_`;
const DIGEST = /^[a-f0-9]{64}$/;
const path = require("node:path");
const fs = require("node:fs/promises");
const { constants } = require("node:fs");
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
      for (const method of ["readOperatorChatArtifacts", "submitOperatorTurn"]) {
        if (typeof client?.[method] !== "function") throw operatorChatClientContractError(`The installed chat client does not export ${method}.`);
      }
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
  ) {
    throw operatorChatClientContractError(
      "The installed world_operator_chat interface owner is missing. Restore that interface owner before using chat.",
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
  const installedFiles = member?.installed_files ?? [];
  const installed = new Map(installedFiles.map(file => [file.relative_path, file]));
  if (installed.get("client.cjs")?.path !== clientPath || installed.size !== installedFiles.length) {
    throw operatorChatClientContractError("The installed chat client has no unique declared client.cjs entry.");
  }
  const stat = await fs.lstat(clientPath);
  if (!stat.isFile() || stat.isSymbolicLink() || await fs.realpath(clientPath) !== clientPath) {
    throw operatorChatClientContractError("The installed chat client must be a regular registry file.");
  }
  const life = await readClientConfiguration(installed.get("life.json"));
  if (!Array.isArray(life.processes) || !life.processes.length || life.processes.some(process =>
    !process || typeof process.bin !== "string" || !process.bin.trim()
    || process.authority?.sandbox_mode !== undefined && !["enforced", "disabled"].includes(process.authority.sandbox_mode))) {
    throw operatorChatClientContractError("Chat lifecycle requires valid declared processes.");
  }
  const actual = await readClientConfiguration(installed.get("client.json"));
  if (actual.schema_version !== 2 || actual.interface !== OPERATOR_CHAT_PACKAGE_ID
    || !actual.run || typeof actual.run !== "object" || Array.isArray(actual.run)) {
    throw operatorChatClientContractError("Chat client configuration requires schema version 2, its interface identity and a run object.");
  }
  const { ee, worker, context_artifact_bindings: bindings } = actual.run;
  if ([ee, worker].some(id => typeof id !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(id))) {
    throw operatorChatClientContractError("Chat run defaults require exact EE and worker identities.");
  }
  if (!Array.isArray(bindings) || bindings.some(binding => !binding || typeof binding !== "object" || Array.isArray(binding)
    || Object.keys(binding).length !== 1 || typeof binding.id !== "string" || !binding.id
    || binding.id.trim() !== binding.id || /[/\\\0]/.test(binding.id))
    || new Set(bindings.map(binding => binding.id)).size !== bindings.length) {
    throw operatorChatClientContractError("Chat run context defaults require unique explicit artifact bindings.");
  }
  return { schemaVersion: actual.schema_version, interface: actual.interface };
}

async function readClientConfiguration(file) {
  if (!file) throw operatorChatClientContractError("The installed chat client is missing a required configuration file.");
  const handle = await fs.open(file.path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > 2 * 1024 * 1024) {
      throw operatorChatClientContractError("Chat client configuration must be a bounded regular file.");
    }
    const value = JSON.parse(await handle.readFile("utf8"));
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw operatorChatClientContractError("Chat client configuration must be a JSON object.");
    }
    return value;
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
