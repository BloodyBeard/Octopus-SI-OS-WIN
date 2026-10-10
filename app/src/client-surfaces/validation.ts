import Ajv2020, { type ErrorObject } from "ajv/dist/2020";
import admissionSchema from "../../schemas/client_surface_admission.schema.json";
import documentSchema from "../../schemas/client_surface_document.schema.json";
import projectionReadContractSchema from "../../schemas/projection_read_contract.schema.json";
import { clientSurfaceDocumentSemanticInspection } from "../../electron/clientSurfaceDocumentSemantics.mjs";
import type {
  AdmittedClientSurfaceDocument,
  ClientSurfaceAdmissionIssue,
  ClientSurfaceAdmissionResult,
  ClientSurfaceAdmissionVerdict,
  ClientSurfaceDocument,
  ClientSurfaceDocumentSource,
} from "./types";
import { CLIENT_SURFACE_READ_METHODS } from "./sourceContract";
import { inspectHomePresentation, isPresentationTimestamp } from "./homePresentation";

const ajv = new Ajv2020({ allErrors: true, allowUnionTypes: true, strict: true });
ajv.addSchema(projectionReadContractSchema);
const validateDocumentSchema = ajv.compile<ClientSurfaceDocument>(documentSchema);
const validateAdmission = ajv.compile<ClientSurfaceAdmissionVerdict>(admissionSchema);

export function admitClientSurfaceSource(source: ClientSurfaceDocumentSource): ClientSurfaceAdmissionResult {
  let value: unknown;
  try {
    value = JSON.parse(source.text);
  } catch (error) {
    return rejected(source.digest, null, null, 0, 0, [{
      code: "document.json.invalid",
      path: "",
      message: error instanceof Error ? error.message : String(error),
    }]);
  }
  const result = admitClientSurfaceDocument(value, source.digest, source.path, source.bytes);
  if (result.admitted && (source.artifactId || source.path.startsWith("artifact:")) && (
    !source.artifactId?.trim() || !source.runId?.trim() || !source.interfaceId?.trim()
    || !Number.isSafeInteger(source.sequence) || source.sequence! < 0
  )) {
    return rejected(source.digest, result.admitted.document.id, result.admitted.document.revision, result.verdict.details.node_count, result.verdict.details.max_depth, [{
      code: "document.delivery.invalid", path: "",
      message: "Artifact-delivered surfaces require an artifact id, exact run and recipient interface, and movement sequence",
    }]);
  }
  if (result.admitted && result.admitted.document.data?.home_presentation
    && (result.admitted.document.data.home_presentation as { lifetime_ms?: number }).lifetime_ms !== undefined
    && (!source.artifactId || !isPresentationTimestamp(source.modifiedAt))) {
    return rejected(source.digest, result.admitted.document.id, result.admitted.document.revision, result.verdict.details.node_count, result.verdict.details.max_depth, [{
      code: "home.presentation.delivery_missing", path: "/data/home_presentation/lifetime_ms",
      message: "A presentation lifetime requires an artifact delivery with a valid UTC timestamp",
    }]);
  }
  if (result.admitted) {
    result.admitted.sourceArtifactId = source.artifactId;
    result.admitted.sourceRunId = source.runId;
    result.admitted.sourceInterfaceId = source.interfaceId;
    result.admitted.sourceSequence = source.sequence;
    result.admitted.sourceDeliveredAt = source.modifiedAt;
  }
  return result;
}

export function admitClientSurfaceDocument(
  value: unknown,
  sourceDigest: string,
  sourcePath = "memory",
  sourceBytes = byteLength(value),
): ClientSurfaceAdmissionResult {
  const issues: ClientSurfaceAdmissionIssue[] = [];
  const schemaPassed = validateDocumentSchema(value);
  if (!schemaPassed) issues.push(...schemaIssues(validateDocumentSchema.errors));
  const document = schemaPassed ? value : null;
  let nodeCount = 0;
  let maxDepth = 0;

  if (document) {
    const inspection = clientSurfaceDocumentSemanticInspection(document, {
      compiled: sourcePath.startsWith("compiled:"),
      readMethods: CLIENT_SURFACE_READ_METHODS,
      sourceBytes,
    });
    issues.push(...inspection.issues);
    issues.push(...inspectHomePresentation(document));
    nodeCount = inspection.nodeCount;
    maxDepth = inspection.maxDepth;
  }

  const verdict = makeVerdict(
    issues.length === 0,
    sourceDigest,
    document?.id ?? candidateIdentity(value).id,
    document?.revision ?? candidateIdentity(value).revision,
    nodeCount,
    maxDepth,
    issues,
  );
  if (!document || issues.length > 0) return { verdict };
  return {
    verdict,
    admitted: {
      document,
      sourceDigest,
      sourcePath,
      verdict: verdict as ClientSurfaceAdmissionVerdict & { passed: true },
    },
  };
}

function candidateIdentity(value: unknown): { id: string | null; revision: number | null } {
  const candidate = value && typeof value === "object" ? value as Record<string, unknown> : {};
  return {
    id: typeof candidate.id === "string" && /^[a-z0-9][a-z0-9._-]{1,63}$/.test(candidate.id) ? candidate.id : null,
    revision: typeof candidate.revision === "number" && Number.isSafeInteger(candidate.revision) && candidate.revision >= 1 ? candidate.revision : null,
  };
}

function schemaIssues(errors: ErrorObject[] | null | undefined): ClientSurfaceAdmissionIssue[] {
  return (errors ?? []).slice(0, 64).map((error) => ({
    code: `schema.${error.keyword}`,
    path: error.instancePath,
    message: error.message || "schema validation failed",
  }));
}

function rejected(
  sourceDigest: string,
  documentId: string | null,
  revision: number | null,
  nodeCount: number,
  maxDepth: number,
  issues: ClientSurfaceAdmissionIssue[],
): ClientSurfaceAdmissionResult {
  return { verdict: makeVerdict(false, sourceDigest, documentId, revision, nodeCount, maxDepth, issues) };
}

function makeVerdict(
  passed: boolean,
  sourceDigest: string,
  documentId: string | null,
  revision: number | null,
  nodeCount: number,
  maxDepth: number,
  issues: ClientSurfaceAdmissionIssue[],
): ClientSurfaceAdmissionVerdict {
  const verdict: ClientSurfaceAdmissionVerdict = {
    passed,
    details: {
      verifier: "client_surface_document_v1",
      contract: "octopus.client_surface@1",
      document_id: documentId,
      revision,
      source_digest: sourceDigest,
      node_count: nodeCount,
      max_depth: maxDepth,
      issues,
    },
  };
  if (!validateAdmission(verdict)) {
    throw new Error(`client surface admission contract is invalid: ${ajv.errorsText(validateAdmission.errors)}`);
  }
  return verdict;
}

function byteLength(value: unknown): number {
  try {
    return new TextEncoder().encode(typeof value === "string" ? value : JSON.stringify(value)).byteLength;
  } catch {
    return Number.MAX_SAFE_INTEGER;
  }
}

export function isAdmittedClientSurfaceDocument(
  value: AdmittedClientSurfaceDocument | undefined,
): value is AdmittedClientSurfaceDocument {
  return value?.verdict.passed === true;
}
