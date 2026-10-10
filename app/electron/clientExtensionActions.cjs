const fs = require("node:fs");
const fsPromises = require("node:fs/promises");
const path = require("node:path");

const CLIENT_EXTENSION_ACTION_RESULT = "octopus.client-extension.action.v1";
const CLIENT_EXTENSION_ACTION_STATUS = "octopus.client-extension.action-status.v1";
const PROVIDER_CREDENTIAL_REPAIR_EXTENSION = "builtin.provider-credential-repair";
const PROVIDER_CREDENTIAL_REPAIR_ACTION = "provider.credentials.refresh";
const PROVIDER_AUTHENTICATION_INCIDENT = "provider.authentication.invalid";
const PROVIDER_SETUP_INCIDENT = "provider.authentication.setup_required";
const CODEX_PROVIDER_INTERFACE = "world_provider_codex";
const MACHINE_CODEX_AUTH_FILE_ENV = "OCTOPUS_MACHINE_CODEX_AUTH_FILE";
const MAX_AUTH_BYTES = 2 * 1024 * 1024;
const MAX_DESCRIPTOR_BYTES = 2 * 1024 * 1024;
let providerCredentialRepairRunning = false;

class ClientExtensionActionError extends Error {
  constructor(code, publicMessage, diagnosticMessage = publicMessage, retryable = true) {
    super(diagnosticMessage);
    this.name = "ClientExtensionActionError";
    this.code = code;
    this.publicMessage = publicMessage;
    this.retryable = retryable;
  }
}

function clientExtensionActionSuccess(value) {
  return {
    contract: CLIENT_EXTENSION_ACTION_RESULT,
    ok: true,
    value,
  };
}

function clientExtensionActionFailure(error) {
  return {
    contract: CLIENT_EXTENSION_ACTION_RESULT,
    ok: false,
    error: {
      code: error instanceof ClientExtensionActionError ? error.code : "repair_failed",
      message: error instanceof ClientExtensionActionError
        ? error.publicMessage
        : "Octopus couldn't repair the sign-in. Try again.",
      retryable: error instanceof ClientExtensionActionError ? error.retryable : true,
    },
  };
}

async function executeClientExtensionAction(options, request) {
  const extensionId = requiredId(request?.extensionId, "extension id");
  const actionId = requiredId(request?.actionId, "action id");
  const incident = normalizeIncident(request?.incident);
  if (
    extensionId !== PROVIDER_CREDENTIAL_REPAIR_EXTENSION
    || actionId !== PROVIDER_CREDENTIAL_REPAIR_ACTION
    || ![PROVIDER_AUTHENTICATION_INCIDENT, PROVIDER_SETUP_INCIDENT].includes(incident.code)
    || incident.interfaceId !== CODEX_PROVIDER_INTERFACE
  ) {
    throw new ClientExtensionActionError(
      "action_not_available",
      "This repair is not available for the detected problem.",
      `client extension action '${extensionId}/${actionId}' does not admit incident '${incident.code}' from '${incident.interfaceId}'`,
      false,
    );
  }
  if (!verifiedInstalledExtensionIds(options).has(extensionId)) {
    throw new ClientExtensionActionError(
      "extension_not_installed",
      "Install the Codex sign-in check extension before using this action.",
      `client extension '${extensionId}' is not installed`,
      false,
    );
  }
  if (providerCredentialRepairRunning) {
    throw new ClientExtensionActionError(
      "action_in_progress",
      "The sign-in repair is already running.",
      "provider credential repair already has an active transaction",
      true,
    );
  }
  providerCredentialRepairRunning = true;
  try {
    return await repairProviderCredential(options, incident);
  } finally {
    providerCredentialRepairRunning = false;
  }
}

async function readClientExtensionActionStatuses(options) {
  const installed = verifiedInstalledExtensionIds(options).has(PROVIDER_CREDENTIAL_REPAIR_EXTENSION);
  const credential = await inspectProviderCredential(options);
  return [{
    contract: CLIENT_EXTENSION_ACTION_STATUS,
    extensionId: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
    actionId: PROVIDER_CREDENTIAL_REPAIR_ACTION,
    interfaceId: CODEX_PROVIDER_INTERFACE,
    installed,
    actionAvailable: false,
    incidentCode: "",
    ...credential,
  }];
}

function verifiedInstalledExtensionIds(options) {
  const values = options?.installedExtensionIds;
  if (!Array.isArray(values)) return new Set();
  return new Set(values.map((value) => requiredId(value, "installed extension id")));
}

async function inspectProviderCredential(options) {
  let octopusHome;
  let codexHome;
  try {
    octopusHome = requiredAbsolutePath(options?.octopusHome, "Octopus home");
    codexHome = requiredAbsolutePath(options?.codexHome, "Codex home");
  } catch (error) {
    return credentialStatus("unavailable", error, "Provider sign-in status is unavailable.");
  }

  try {
    const target = await resolveProviderAuthTarget(octopusHome, CODEX_PROVIDER_INTERFACE, codexHome);
    await readAndValidateCodexAuth(target.path, "machine Codex sign-in");
    return {
      state: "configured",
      code: "provider_credentials_present",
      message: "Octopus uses the current machine-wide Codex sign-in.",
    };
  } catch (error) {
    if (!(error instanceof ClientExtensionActionError)) {
      return credentialStatus("unavailable", error, "Provider sign-in status is unavailable.");
    }
    if (error.code === "provider_auth_store_missing") {
      return credentialStatus("waiting_for_interface", error, error.publicMessage);
    }
    if (["codex_login_unavailable", "codex_login_invalid"].includes(error.code)) {
      return credentialStatus("sign_in_required", error, error.publicMessage);
    }
    return credentialStatus("unavailable", error, error.publicMessage);
  }
}

function credentialStatus(state, error, message) {
  return {
    state,
    code: error instanceof ClientExtensionActionError ? error.code : "provider_status_unavailable",
    message,
  };
}

async function repairProviderCredential(options, incident) {
  const octopusHome = requiredAbsolutePath(options?.octopusHome, "Octopus home");
  const codexHome = requiredAbsolutePath(options?.codexHome, "Codex home");
  const target = await resolveProviderAuthTarget(octopusHome, incident.interfaceId, codexHome);
  await readAndValidateCodexAuth(target.path, "machine Codex sign-in");
  return {
    actionId: PROVIDER_CREDENTIAL_REPAIR_ACTION,
    extensionId: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
    incidentId: incident.id,
    interfaceId: incident.interfaceId,
    credentialUpdated: false,
    runtimeRestarted: false,
    sharedCredential: true,
  };
}

async function resolveProviderAuthTarget(octopusHome, interfaceId, codexHome) {
  const descriptorPath = path.join(octopusHome, "registry", "interface", interfaceId, "interface.json");
  const descriptorBytes = await readStableRegularFile(descriptorPath, MAX_DESCRIPTOR_BYTES, "provider interface descriptor").catch((error) => {
    if (error?.code === "ENOENT") {
      throw new ClientExtensionActionError(
        "provider_auth_store_missing",
        "The Codex interface is not installed yet.",
        `provider descriptor '${descriptorPath}' does not exist`,
        true,
      );
    }
    throw error;
  });
  let descriptor;
  try {
    descriptor = JSON.parse(descriptorBytes.toString("utf8"));
  } catch (error) {
    throw new ClientExtensionActionError(
      "provider_descriptor_invalid",
      "The provider configuration is invalid, so Octopus left it unchanged.",
      `provider descriptor '${descriptorPath}' is not valid JSON: ${formatError(error)}`,
      false,
    );
  }
  if (!descriptor || typeof descriptor !== "object" || Array.isArray(descriptor) || descriptor.id !== interfaceId) {
    throw new ClientExtensionActionError(
      "provider_descriptor_invalid",
      "The provider configuration is invalid, so Octopus left it unchanged.",
      `provider descriptor '${descriptorPath}' does not declare exact interface '${interfaceId}'`,
      false,
    );
  }
  const privateStore = descriptor.interface_driver_config?.residual_ambient?.private_auth_store;
  if (
    !privateStore
    || typeof privateStore !== "object"
    || Array.isArray(privateStore)
    || Object.keys(privateStore).length !== 1
    || privateStore.auth_file_env !== MACHINE_CODEX_AUTH_FILE_ENV
  ) {
    throw new ClientExtensionActionError(
      "provider_auth_store_unsupported",
      "This Codex interface doesn't declare the machine-wide sign-in contract.",
      `provider interface '${interfaceId}' must declare only private_auth_store.auth_file_env='${MACHINE_CODEX_AUTH_FILE_ENV}'`,
      false,
    );
  }
  const homeStat = await fsPromises.lstat(codexHome).catch((error) => {
    throw new ClientExtensionActionError(
      "codex_login_unavailable",
      "Sign in to Codex on this Mac, then try again.",
      `machine Codex home '${codexHome}' cannot be inspected: ${formatError(error)}`,
      true,
    );
  });
  if (!homeStat.isDirectory() || homeStat.isSymbolicLink()) {
    throw new ClientExtensionActionError(
      "provider_auth_store_invalid",
      "The machine Codex home is unsafe, so Octopus left it unchanged.",
      `machine Codex home '${codexHome}' is not a real directory`,
      false,
    );
  }
  return { path: path.join(codexHome, "auth.json") };
}

async function readAndValidateCodexAuth(filePath, label) {
  const bytes = await readStableRegularFile(filePath, MAX_AUTH_BYTES, label).catch((error) => {
    if (error instanceof ClientExtensionActionError) throw error;
    throw new ClientExtensionActionError(
      "codex_login_unavailable",
      "Sign in to Codex on this Mac, then try the repair again.",
      `${label} '${filePath}' cannot be read: ${formatError(error)}`,
      true,
    );
  });
  let value;
  try {
    value = JSON.parse(bytes.toString("utf8"));
  } catch (error) {
    throw new ClientExtensionActionError(
      "codex_login_invalid",
      "The current Codex sign-in is invalid. Sign in to Codex again, then retry.",
      `${label} '${filePath}' is not valid JSON: ${formatError(error)}`,
      true,
    );
  }
  const tokens = value && typeof value === "object" && !Array.isArray(value) ? value.tokens : null;
  const requiredTokens = [tokens?.access_token, tokens?.refresh_token, tokens?.id_token];
  if (
    value?.auth_mode !== "chatgpt"
    || !tokens
    || typeof tokens !== "object"
    || Array.isArray(tokens)
    || requiredTokens.some((token) => typeof token !== "string" || !token.trim() || Buffer.byteLength(token, "utf8") > MAX_AUTH_BYTES)
  ) {
    throw new ClientExtensionActionError(
      "codex_login_invalid",
      "The current Codex sign-in is incomplete. Sign in to Codex again, then retry.",
      `${label} '${filePath}' does not contain complete ChatGPT token data`,
      true,
    );
  }
  return { bytes, realPath: await fsPromises.realpath(filePath) };
}

async function readStableRegularFile(filePath, maximumBytes, label, allowEmpty = false) {
  const noFollow = typeof fs.constants.O_NOFOLLOW === "number" ? fs.constants.O_NOFOLLOW : 0;
  const handle = await fsPromises.open(filePath, fs.constants.O_RDONLY | noFollow);
  try {
    const before = await handle.stat();
    if (!before.isFile() || (!allowEmpty && before.size <= 0) || before.size > maximumBytes) {
      throw new Error(`${label} must be a ${allowEmpty ? "regular file" : "non-empty regular file"} no larger than ${maximumBytes} bytes`);
    }
    const bytes = await handle.readFile();
    const after = await handle.stat();
    if (bytes.byteLength !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
      throw new Error(`${label} changed while it was being read`);
    }
    return bytes;
  } finally {
    await handle.close();
  }
}

function normalizeIncident(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ClientExtensionActionError("incident_invalid", "The detected problem is no longer available.", "client extension incident must be an object", false);
  }
  const id = requiredText(value.id, "incident id", 256);
  const code = requiredId(value.code, "incident code");
  const interfaceId = requiredId(value.interfaceId, "incident interface id");
  return { id, code, interfaceId };
}

function requiredId(value, label) {
  const text = requiredText(value, label, 96);
  if (!/^[a-z0-9][a-z0-9._-]+$/.test(text)) {
    throw new ClientExtensionActionError("request_invalid", "The repair request is invalid.", `${label} is invalid`, false);
  }
  return text;
}

function requiredText(value, label, maximumBytes) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text || Buffer.byteLength(text, "utf8") > maximumBytes) {
    throw new ClientExtensionActionError("request_invalid", "The repair request is invalid.", `${label} is invalid`, false);
  }
  return text;
}

function requiredAbsolutePath(value, label) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text || !path.isAbsolute(text)) {
    throw new ClientExtensionActionError("host_path_invalid", "The repair host is not configured correctly.", `${label} must be an absolute path`, false);
  }
  return path.resolve(text);
}

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = {
  CLIENT_EXTENSION_ACTION_RESULT,
  CLIENT_EXTENSION_ACTION_STATUS,
  PROVIDER_AUTHENTICATION_INCIDENT,
  PROVIDER_CREDENTIAL_REPAIR_ACTION,
  PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
  ClientExtensionActionError,
  clientExtensionActionFailure,
  clientExtensionActionSuccess,
  executeClientExtensionAction,
  readClientExtensionActionStatuses,
};
