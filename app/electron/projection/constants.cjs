const Ajv = require("ajv");
const movementSchema = require("../../../schemas/movement.schema.json");
const runRecordSchema = require("../../../schemas/run_record.schema.json");

const canonicalSchemaValidator = new Ajv({ allErrors: true, strict: false, logger: false });

const movementSchemaValidator = canonicalSchemaValidator.compile(movementSchema);

const runRecordSchemaValidator = canonicalSchemaValidator.compile(runRecordSchema);

class ProjectionEvidenceError extends Error {}

const DESCRIPTOR_REGISTRIES = [
  { kind: "interface", file: "interface.json", identity: ["id", "rule_id"] },
  { kind: "ee_template", file: "ee_template.json", identity: ["id", "name"] },
  { kind: "worker_environment_kind", file: "worker_environment_kind.json", identity: ["worker_environment_kind_id", "kind", "id"] },
  { kind: "tool", file: "tool.json", identity: ["id", "name"] },
  { kind: "verifier", file: "verifier.json", identity: ["id", "name"] },
  { kind: "artifact", file: "artifact.json", identity: ["id", "label", "name"] },
  { kind: "mark", file: "mark.json", identity: ["name", "id", "kind"] },
];

const CLIENT_EVENT_DATA_MAX_BYTES = 8 * 1024;

const CLIENT_EVENT_DATA_MAX_STRING_LENGTH = 1_024;

const CLIENT_EVENT_DATA_MAX_ARRAY_ITEMS = 12;

const CLIENT_EVENT_DATA_MAX_OBJECT_KEYS = 48;

const CLIENT_EVENT_DATA_MAX_DEPTH = 4;

const CLIENT_REASONING_SUMMARY_MAX_BYTES = 4 * 1024;

const APPLICATION_LOGO_MAX_BYTES = 256 * 1024;

const INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES = 1024 * 1024;

const INTERFACE_PRIVATE_ENVIRONMENT_FILE = "environment.json";

const INTERFACE_PRIVATE_RESERVED_ENV = new Set([
  "OCTOPUS_INTERFACE_ID",
  "OCTOPUS_INTERFACE_DIR",
  "OCTOPUS_INTERFACE_DESCRIPTOR_JSON",
  "OCTOPUS_INTERFACE_HOST_IO_LIMITS",
  "OCTOPUS_VAR_DIR",
  "OCTOPUS_ROOT_SLOTS_JSON",
  "OCTOPUS_MOVEMENTS_JSONL",
  "OCTOPUS_BLOBS_DIR",
  "OCTOPUS_LEDGER_INDEX_SQLITE",
  "TMPDIR",
]);

const FULL_LEDGER_PROJECTION_METHODS = new Set([
  "projection.runtime.topology",
  "projection.runtime.snapshot",
  "projection.ee_instances.list",
  "projection.runs.list",
  "projection.runs.checkpoints.list",
  "projection.runs.get",
  "projection.trace.recent",
  "projection.artifacts.list",
  "projection.marks.list",
  "projection.envelopes.list",
  "projection.interfaces.pending.list",
  "projection.descriptors.artifact_labels.list",
  "projection.movements.list",
]);

const scheduleEditTails = new Map();

const MOVEMENT_PAYLOAD_PREVIEW_BYTES = 64 * 1024;

const INTERFACE_MEDIA_MAX_BYTES = 64 * 1024 * 1024;

const ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES = 2 * 1024 * 1024;

const ARTIFACT_PREVIEW_MAX_BYTES = 40 * 1024 * 1024;

const ARTIFACT_PAYLOAD_READ_CONCURRENCY = 8;

const RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES = 512;

const RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES = 64 * 1024 * 1024;

const CHANGE_JOURNAL_MAX_BYTES = 1024 * 1024;

const CHANGE_JOURNAL_MAX_FILE_BYTES = 16 * 1024 * 1024;

module.exports = { APPLICATION_LOGO_MAX_BYTES, ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES, ARTIFACT_PAYLOAD_READ_CONCURRENCY, ARTIFACT_PREVIEW_MAX_BYTES, CHANGE_JOURNAL_MAX_BYTES, CHANGE_JOURNAL_MAX_FILE_BYTES, CLIENT_EVENT_DATA_MAX_ARRAY_ITEMS, CLIENT_EVENT_DATA_MAX_BYTES, CLIENT_EVENT_DATA_MAX_DEPTH, CLIENT_EVENT_DATA_MAX_OBJECT_KEYS, CLIENT_EVENT_DATA_MAX_STRING_LENGTH, CLIENT_REASONING_SUMMARY_MAX_BYTES, DESCRIPTOR_REGISTRIES, FULL_LEDGER_PROJECTION_METHODS, INTERFACE_MEDIA_MAX_BYTES, INTERFACE_PRIVATE_ENVIRONMENT_FILE, INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES, INTERFACE_PRIVATE_RESERVED_ENV, MOVEMENT_PAYLOAD_PREVIEW_BYTES, ProjectionEvidenceError, RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES, RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES, movementSchemaValidator, runRecordSchemaValidator, scheduleEditTails };
