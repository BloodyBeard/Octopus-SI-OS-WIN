const Ajv2020 = require("ajv/dist/2020");
const schema = require("../schemas/projection_read_contract.schema.json");

const validator = new Ajv2020({ allErrors: true, strict: false, logger: false }).compile(schema);
const PROJECTION_READ_CONTRACT_VERSION = schema.properties.contract_version.const;
const PROJECTION_READ_METHODS = Object.freeze([...schema.$defs.projectionReadMethod.enum]);
const CLIENT_SURFACE_PROJECTION_READ_METHODS = Object.freeze([
  ...schema.$defs.clientSurfaceProjectionReadMethod.enum,
]);
const projectionReadMethodSet = new Set(PROJECTION_READ_METHODS);

if (
  !Number.isSafeInteger(PROJECTION_READ_CONTRACT_VERSION)
  || PROJECTION_READ_CONTRACT_VERSION < 1
  || projectionReadMethodSet.size !== PROJECTION_READ_METHODS.length
  || CLIENT_SURFACE_PROJECTION_READ_METHODS.some((method) => !projectionReadMethodSet.has(method))
) {
  throw new Error("projection read contract schema is internally inconsistent");
}

function projectionReadRequest(method, params = {}) {
  return parseProjectionReadRequest({
    contract_version: PROJECTION_READ_CONTRACT_VERSION,
    method,
    params,
  });
}

function parseProjectionReadRequest(value) {
  if (!validator(value)) {
    throw new Error(`projection read request violates contract v${PROJECTION_READ_CONTRACT_VERSION}: ${formatErrors(validator.errors)}`);
  }
  return {
    contract_version: value.contract_version,
    method: value.method,
    params: value.params ?? {},
  };
}

function formatErrors(errors) {
  return (errors ?? []).map((error) => `${error.instancePath || "/"} ${error.message}`).join("; ");
}

module.exports = {
  CLIENT_SURFACE_PROJECTION_READ_METHODS,
  PROJECTION_READ_CONTRACT_VERSION,
  PROJECTION_READ_METHODS,
  parseProjectionReadRequest,
  projectionReadRequest,
};
