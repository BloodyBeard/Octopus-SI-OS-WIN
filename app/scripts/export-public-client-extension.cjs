const fs = require("node:fs/promises");
const path = require("node:path");
const Ajv = require("ajv/dist/2020");

const extensionManifestSchema = require("../schemas/client_extension_manifest.schema.json");
const surfaceDocumentSchema = require("../schemas/client_surface_document.schema.json");
const projectionReadContractSchema = require("../schemas/projection_read_contract.schema.json");
const publicationSchema = require("../schemas/client_extension_publication.schema.json");
const {
  assertDisjointPaths,
  assertOutsideDetectedOctopusHome,
  assertOutsidePrivateRoots,
  createAtomicPublication,
  errorMessage,
  parseOptions,
  prettyJson,
  publicationDigest,
  publicationPrivateRoots,
  readPublicationLicense,
  requiredAbsoluteOption,
  requirePlainDirectory,
  requireRegularFile,
  sha256,
  validatePublicPayload,
  writePublicationFile,
} = require("./publication-lib.cjs");

const validators = new Ajv({ allErrors: true, allowUnionTypes: true, strict: true });
validators.addSchema(projectionReadContractSchema);
validators.addSchema(surfaceDocumentSchema);
const validateManifest = validators.compile(extensionManifestSchema);
const validatePublication = validators.compile(publicationSchema);
const readMethods = Object.freeze([
  ...(projectionReadContractSchema.$defs?.clientSurfaceProjectionReadMethod?.enum ?? []),
]);

async function main() {
  const options = parseOptions(process.argv.slice(2), new Set([
    "source",
    "out",
    "license",
    "license-file",
  ]));
  const privateRoots = publicationPrivateRoots();
  const sourceRoot = await requirePlainDirectory(
    assertOutsidePrivateRoots(requiredAbsoluteOption(options, "source"), "client Extension source", privateRoots),
    "client Extension source",
  );
  await assertOutsideDetectedOctopusHome(sourceRoot, "client Extension source");
  const outRoot = requiredAbsoluteOption(options, "out");
  await assertDisjointPaths(outRoot, "publication output", sourceRoot, "client Extension source");
  const entries = await fs.readdir(sourceRoot, { withFileTypes: true });
  const unexpected = entries.filter((entry) => entry.name !== "extension.json");
  if (unexpected.length > 0) {
    throw new Error(`client Extension source contains files outside the current extension contract: ${unexpected.map((entry) => entry.name).sort().join(", ")}`);
  }
  const manifestFile = path.join(sourceRoot, "extension.json");
  const { body: manifestBody } = await requireRegularFile(manifestFile, "client Extension manifest", 1024 * 1024);
  validatePublicPayload("extension.json", manifestBody, { privateRoots });
  let manifest;
  try {
    manifest = JSON.parse(manifestBody.toString("utf8"));
  } catch (error) {
    throw new Error(`client Extension manifest is malformed JSON: ${errorMessage(error)}`);
  }
  if (!validateManifest(manifest)) throw schemaError("client Extension manifest", validateManifest.errors);
  if (manifest.contribution.kind !== "surface") {
    throw new Error("only declared client surface Extensions are downloadable; built-in client code is published with the Octopus source repository");
  }
  const { clientExtensionManifestSemanticIssues } = await import("../electron/clientExtensionManifestSemantics.mjs");
  const { clientSurfaceDocumentSemanticInspection } = await import("../electron/clientSurfaceDocumentSemantics.mjs");
  const issues = await clientExtensionManifestSemanticIssues(manifest, { compiled: false });
  const inspection = await clientSurfaceDocumentSemanticInspection(manifest.contribution.document, {
    compiled: false,
    readMethods,
  });
  issues.push(...inspection.issues.map((issue) => ({
    ...issue,
    code: `surface.${issue.code}`,
    path: `/contribution/document${issue.path}`,
  })));
  if (issues.length > 0) {
    throw new Error(`client Extension failed semantic admission: ${issues.map((issue) => `${issue.path}: ${issue.message}`).join("; ")}`);
  }
  const license = await readPublicationLicense(options, privateRoots);
  const sourceDigest = `sha256:${sha256(manifestBody)}`;
  const bundleName = `${manifest.id}-${manifest.version}-${sourceDigest.slice(7, 19)}.octopus-client-extension`;
  const result = await createAtomicPublication(outRoot, bundleName, async (stageRoot) => {
    const files = [];
    files.push(await writePublicationFile(stageRoot, license.path, license.body, 0o644));
    const manifestPath = path.posix.join("packages", manifest.id, "extension.json");
    files.push(await writePublicationFile(stageRoot, manifestPath, manifestBody, 0o644));
    const publicationCore = {
      schema: "octopus.client_extension_publication",
      schema_version: 1,
      layout: "octopus.client_extension_library.v1",
      extension_id: manifest.id,
      extension_version: manifest.version,
      manifest_path: manifestPath,
      source_digest: sourceDigest,
      license: {
        spdx: license.spdx,
        path: license.path,
        sha256: license.sha256,
      },
      files: [...files].sort((left, right) => left.path.localeCompare(right.path)),
    };
    const publication = { ...publicationCore, bundle_digest: publicationDigest(publicationCore) };
    if (!validatePublication(publication)) throw schemaError("client Extension publication", validatePublication.errors);
    await writePublicationFile(stageRoot, "publication.json", prettyJson(publication), 0o644);
    return {
      extension_id: publication.extension_id,
      extension_version: publication.extension_version,
      source_digest: publication.source_digest,
      bundle_digest: publication.bundle_digest,
      file_count: publication.files.length,
    };
  }, privateRoots);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

function schemaError(label, errors) {
  const detail = (errors ?? []).slice(0, 12).map((error) => `${error.instancePath || "/"} ${error.message}`).join("; ");
  return new Error(`${label} does not satisfy its schema${detail ? `: ${detail}` : ""}`);
}

main().catch((error) => {
  process.stderr.write(`Client Extension publication refused: ${errorMessage(error)}\n`);
  process.exitCode = 1;
});
