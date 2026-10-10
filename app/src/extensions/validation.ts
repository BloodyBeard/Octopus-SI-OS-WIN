import Ajv2020, { type ErrorObject } from "ajv/dist/2020";
import documentSchema from "../../schemas/client_surface_document.schema.json";
import manifestSchema from "../../schemas/client_extension_manifest.schema.json";
import projectionReadContractSchema from "../../schemas/projection_read_contract.schema.json";
import { clientExtensionManifestSemanticIssues } from "../../electron/clientExtensionManifestSemantics.mjs";
import { admitClientSurfaceDocument } from "../client-surfaces/validation";
import type { ClientSurfaceDocument } from "../client-surfaces/types";
import type {
  AdmittedClientExtension,
  ClientExtensionAdmissionIssue,
  ClientExtensionAdmissionVerdict,
  ClientExtensionManifest,
} from "./types";

const MAX_MANIFEST_BYTES = 1024 * 1024;
const ajv = new Ajv2020({ allErrors: true, allowUnionTypes: true, strict: true });
ajv.addSchema(projectionReadContractSchema);
ajv.addSchema(documentSchema);
const validateManifestSchema = ajv.compile<ClientExtensionManifest>(manifestSchema);

export function admitClientExtensionSource(source: {
  path: string;
  digest: string;
  bytes: number;
  text: string;
}): { verdict: ClientExtensionAdmissionVerdict; admitted?: AdmittedClientExtension } {
  let value: unknown;
  try {
    value = JSON.parse(source.text);
  } catch (error) {
    return rejected(source.digest, null, [{
      code: "manifest.json.invalid",
      path: "",
      message: error instanceof Error ? error.message : String(error),
    }]);
  }
  return admitClientExtensionManifest(value, source.digest, source.path, source.bytes);
}

export function admitClientExtensionManifest(
  value: unknown,
  sourceDigest: string,
  sourcePath = "memory",
  sourceBytes = byteLength(value),
): { verdict: ClientExtensionAdmissionVerdict; admitted?: AdmittedClientExtension } {
  const issues: ClientExtensionAdmissionIssue[] = [];
  if (sourceBytes > MAX_MANIFEST_BYTES) {
    issues.push({
      code: "manifest.size.exceeded",
      path: "",
      message: `manifest is ${sourceBytes} bytes; maximum is ${MAX_MANIFEST_BYTES}`,
    });
  }
  const schemaPassed = validateManifestSchema(value);
  if (!schemaPassed) issues.push(...schemaIssues(validateManifestSchema.errors));
  const manifest = schemaPassed ? value : null;
  let surface: AdmittedClientExtension["surface"];

  if (manifest) {
    const compiled = sourcePath.startsWith("compiled:");
    if (!compiled) {
      const pathParts = sourcePath.split(/[\\/]/g);
      const packageName = pathParts.length >= 3 && pathParts.at(-1) === "extension.json"
        ? pathParts.at(-2) ?? ""
        : "";
      if (!packageName || packageName !== manifest.id) {
        issues.push({
          code: "manifest.package.identity_mismatch",
          path: "/id",
          message: "extension id must match its package directory name",
        });
      }
    }
    issues.push(...clientExtensionManifestSemanticIssues(manifest, { compiled }));

    if (manifest.contribution.kind === "surface") {
      const admittedSurface = admitClientSurfaceDocument(
        manifest.contribution.document,
        `${sourceDigest}:surface`,
        `extension:${sourcePath}`,
      );
      if (admittedSurface.admitted) {
        surface = admittedSurface.admitted;
      } else {
        issues.push(...admittedSurface.verdict.details.issues.map((issue) => ({
          code: `surface.${issue.code}`,
          path: `/contribution/document${issue.path}`,
          message: issue.message,
        })));
      }
    }
  }

  const verdict: ClientExtensionAdmissionVerdict = {
    passed: Boolean(manifest) && issues.length === 0,
    extensionId: manifest?.id ?? null,
    sourceDigest,
    issues,
  };
  if (!manifest || issues.length > 0) return { verdict };
  if (surface) surface.sourceExtensionId = manifest.id;
  return {
    verdict,
    admitted: {
      manifest,
      sourceDigest,
      sourcePath,
      surface,
      verdict: verdict as ClientExtensionAdmissionVerdict & { passed: true },
    },
  };
}

function schemaIssues(errors: ErrorObject[] | null | undefined): ClientExtensionAdmissionIssue[] {
  return (errors ?? []).slice(0, 64).map((error) => ({
    code: `schema.${error.keyword}`,
    path: error.instancePath,
    message: error.message || "schema validation failed",
  }));
}

function rejected(
  sourceDigest: string,
  extensionId: string | null,
  issues: ClientExtensionAdmissionIssue[],
): { verdict: ClientExtensionAdmissionVerdict } {
  return {
    verdict: {
      passed: false,
      extensionId,
      sourceDigest,
      issues,
    },
  };
}

function byteLength(value: unknown): number {
  try {
    return new TextEncoder().encode(typeof value === "string" ? value : JSON.stringify(value)).byteLength;
  } catch {
    return Number.MAX_SAFE_INTEGER;
  }
}

export function manifestDocument(manifest: ClientExtensionManifest): ClientSurfaceDocument | null {
  return manifest.contribution.kind === "surface" ? manifest.contribution.document : null;
}
