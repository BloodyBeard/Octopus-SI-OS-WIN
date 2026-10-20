module.exports = { absolutePath, arrayValue, artifactIdHasLabel, artifactLabel, assertAdmitted, boundedPositiveInteger, contentHashHex, databaseInteger, decimalSafeInteger, endpointId, endpointInterfaceId, exactArrivalRuntime, exactOutboundRuntime, integerAtLeast, invocationRuntime, isObject, nonNegativeInteger, numberValue, optionalServiceTier, positiveInteger, randomToken, readJsonDocumentIfPresent, readJsonIfPresent, requiredArray, requiredFunction, requiredObject, requiredString, requiredTrimmedString, safeFilename, safeSegment, sha256Hex, streamFragmentValue, stringValue };

const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");

function artifactLabel(artifactId) {
  const separator = artifactId.lastIndexOf(":");
  const label = separator > 0 ? artifactId.slice(0, separator) : artifactId;
  return requiredString(label, "artifact label");
}

function contentHashHex(value, field) {
  const hash = typeof value === "string"
    ? value.trim().toLowerCase()
    : Array.isArray(value)
      ? Buffer.from(value).toString("hex")
      : "";
  if (!/^[0-9a-f]{64}$/.test(hash)) throw new Error(`${field} must be a 32-byte content digest`);
  return hash;
}

function databaseInteger(value, field) {
  const number = typeof value === "bigint" ? Number(value) : value;
  return nonNegativeInteger(number, field);
}

function decimalSafeInteger(value, field) {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) {
    throw new Error(`${field} must be an unsigned decimal integer`);
  }
  const number = Number(value);
  return nonNegativeInteger(number, field);
}

function integerAtLeast(value, minimum, field) {
  if (!Number.isSafeInteger(value) || value < minimum) {
    throw new Error(`${field} must be a safe integer at or above ${minimum}`);
  }
  return value;
}

function boundedPositiveInteger(value, fallback, maximum, field) {
  const selected = value === undefined ? fallback : value;
  if (!Number.isSafeInteger(selected) || selected <= 0 || selected > maximum) {
    throw new Error(`${field} must be a positive safe integer at or below ${maximum}`);
  }
  return selected;
}

function requiredFunction(value, field) {
  if (typeof value !== "function") throw new Error(`${field} must be a function`);
  return value;
}

async function readJsonIfPresent(file) {
  return (await readJsonDocumentIfPresent(file))?.value ?? null;
}

async function readJsonDocumentIfPresent(file) {
  try {
    const text = await fs.readFile(file, "utf8");
    const value = JSON.parse(text);
    if (!isObject(value)) throw new Error(`${file} must contain a JSON object`);
    return { value, text };
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function assertAdmitted(verdict, operation) {
  if (isObject(verdict?.admitted) && Number.isFinite(verdict.admitted.seq)) return;
  if (isObject(verdict?.refused)) {
    throw new Error(
      `${operation} refused (${stringValue(verdict.refused.reason) || "unknown"}): ${JSON.stringify(verdict.refused.detail)}`,
    );
  }
  throw new Error(`${operation} returned a malformed delivery verdict`);
}

function endpointInterfaceId(endpoint) {
  return endpointId(endpoint, "Interface");
}

function exactArrivalRuntime(movement, interfaceId) {
  if (movement?.kind !== "InterfaceArrival") return null;
  if (endpointInterfaceId(movement.from) !== interfaceId) return null;
  const runtime = invocationRuntime(movement);
  if (
    runtime.interface !== interfaceId
    || endpointInterfaceId(movement.to) !== runtime.worker
  ) {
    throw new Error("arrival movement interface endpoints do not match its exact runtime provenance");
  }
  return runtime;
}

function exactOutboundRuntime(movement, interfaceId) {
  if (movement?.kind !== "InterfaceOutbound") return null;
  if (!endpointInterfaceId(movement.from)) return null;
  if (endpointInterfaceId(movement.to) !== interfaceId) return null;
  const runtime = invocationRuntime(movement);
  if (runtime.interface !== interfaceId) {
    throw new Error("outbound movement runtime interface does not match its exact target");
  }
  return runtime;
}

function endpointId(endpoint, kind) {
  if (!isObject(endpoint) || Object.keys(endpoint).length !== 1 || !(kind in endpoint)) return "";
  const identifier = endpoint[kind];
  return typeof identifier === "string" && identifier && identifier.trim() === identifier
    ? identifier
    : "";
}

function invocationRuntime(movement) {
  const metadata = requiredObject(
    movement?.invocation_metadata,
    "movement invocation_metadata",
  );
  const runtime = requiredObject(metadata.runtime, "movement invocation_metadata.runtime");
  for (const field of ["run", "ee", "worker", "checkpoint", "interface"]) {
    requiredTrimmedString(runtime[field], `movement runtime.${field}`);
  }
  nonNegativeInteger(runtime.epoch, "movement runtime.epoch");
  return runtime;
}

function artifactIdHasLabel(id, label) {
  return id === label || id.startsWith(`${label}:`);
}

function safeSegment(value, field) {
  const segment = requiredString(value, field);
  if (segment.length > 96 || !/^[A-Za-z0-9_-]+$/.test(segment)) {
    throw new Error(`${field} must contain only letters, digits, underscore, or hyphen`);
  }
  return segment;
}

function safeFilename(value) {
  const filename = path.basename(requiredString(value, "filename"))
    .replace(/[^A-Za-z0-9_. -]/g, "_");
  return filename || "artifact";
}

function absolutePath(value, field) {
  const input = requiredString(value, field);
  if (!path.isAbsolute(input)) throw new Error(`${field} must be absolute`);
  return path.normalize(input);
}

function randomToken(length) {
  return crypto.randomBytes(Math.ceil(length / 2)).toString("hex").slice(0, length);
}

function sha256Hex(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function isObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function requiredObject(value, field) {
  if (!isObject(value)) throw new Error(`${field} must be an object`);
  return value;
}

function arrayValue(value) {
  return Array.isArray(value) ? value : [];
}

function requiredArray(value, field) {
  if (!Array.isArray(value)) throw new Error(`${field} must be an array`);
  return value;
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function streamFragmentValue(value) {
  return typeof value === "string" ? value : "";
}

function requiredString(value, field) {
  const output = stringValue(value);
  if (!output) throw new Error(`${field} is required`);
  return output;
}

function requiredTrimmedString(value, field) {
  if (typeof value !== "string" || !value || value.trim() !== value) {
    throw new Error(`${field} must be a non-empty trimmed string`);
  }
  return value;
}

function numberValue(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? value
    : fallback;
}

function nonNegativeInteger(value, field) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${field} must be a non-negative safe integer`);
  }
  return value;
}

function positiveInteger(value, field) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${field} must be a positive safe integer`);
  }
  return value;
}

function optionalServiceTier(value) {
  if (value === undefined || value === null || value === "") return "";
  if (typeof value !== "string" || value.trim() !== value || value.length > 80 || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error("serviceTier must be a non-empty native tier id");
  }
  return value;
}
