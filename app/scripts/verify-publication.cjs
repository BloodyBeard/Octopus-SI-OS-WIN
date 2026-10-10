const fs = require("node:fs/promises");
const path = require("node:path");
const Ajv = require("ajv/dist/2020");

const clientExtensionManifestSchema = require("../schemas/client_extension_manifest.schema.json");
const clientSurfaceDocumentSchema = require("../schemas/client_surface_document.schema.json");
const localPackageManifestSchema = require("../schemas/local_package_manifest.schema.json");
const projectionReadContractSchema = require("../schemas/projection_read_contract.schema.json");
const packagePublicationSchema = require("../schemas/package_publication.schema.json");
const clientExtensionPublicationSchema = require("../schemas/client_extension_publication.schema.json");
const { readLocalPackageSourcePackage } = require("../electron/localPackages.cjs");
const {
  errorMessage,
  parseOptions,
  publicationDigest,
  publicationPrivateRoots,
  readJsonRegular,
  requiredAbsoluteOption,
  requirePlainDirectory,
  requireRegularFile,
  sha256,
  stableJson,
  validatePublicPayload,
} = require("./publication-lib.cjs");

const validators = new Ajv({ allErrors: true, allowUnionTypes: true, strict: false, logger: false });
validators.addSchema(projectionReadContractSchema);
validators.addSchema(clientSurfaceDocumentSchema);
const validateLocalPackageManifest = validators.compile(localPackageManifestSchema);
const validateClientExtensionManifest = validators.compile(clientExtensionManifestSchema);
const validatePackagePublication = validators.compile(packagePublicationSchema);
const validateClientExtensionPublication = validators.compile(clientExtensionPublicationSchema);
const readMethods = Object.freeze([
  ...(projectionReadContractSchema.$defs?.clientSurfaceProjectionReadMethod?.enum ?? []),
]);

async function main() {
  const options = parseOptions(process.argv.slice(2), new Set(["bundle"]));
  const bundleRoot = await requirePlainDirectory(requiredAbsoluteOption(options, "bundle"), "publication bundle");
  const publicationFile = path.join(bundleRoot, "publication.json");
  const { value: publication } = await readJsonRegular(publicationFile, "publication manifest", 8 * 1024 * 1024);
  if (publication.schema === "octopus.package_publication") {
    await verifyRegistryPackage(bundleRoot, publication);
  } else if (publication.schema === "octopus.client_extension_publication") {
    await verifyClientExtension(bundleRoot, publication);
  } else {
    throw new Error(`publication schema is unsupported: ${publication.schema}`);
  }
  process.stdout.write(`${JSON.stringify({
    schema: publication.schema,
    id: publication.package_id ?? publication.extension_id,
    version: publication.package_version ?? publication.extension_version,
    bundle_digest: publication.bundle_digest,
    verified: true,
  }, null, 2)}\n`);
}

async function verifyRegistryPackage(bundleRoot, publication) {
  if (!validatePackagePublication(publication)) throw schemaError("package publication", validatePackagePublication.errors);
  verifyCoreDigest(publication);
  if (publication.manifest_path !== `packages/${publication.package_id}.json`) {
    throw new Error("package publication manifest path is not canonical");
  }
  const evidence = await verifyFiles(bundleRoot, publication.files, { rejectDevelopmentExecutables: true });
  verifyLicense(publication, evidence);
  const manifestEvidence = evidence.get(publication.manifest_path);
  if (!manifestEvidence) throw new Error("package publication does not list its manifest file");
  const manifestFile = path.join(bundleRoot, ...publication.manifest_path.split("/"));
  const { value: manifest } = await readJsonRegular(manifestFile, "local package manifest", 4 * 1024 * 1024);
  if (!validateLocalPackageManifest(manifest)) throw schemaError("local package manifest", validateLocalPackageManifest.errors);
  if (
    manifest.id !== publication.package_id
    || manifest.version !== publication.package_version
    || manifest.product_kind !== publication.product_kind
    || manifest.product_id !== publication.product_id
  ) throw new Error("package publication identity disagrees with its local package manifest");
  const manifestDigest = sha256(Buffer.from(stableJson(manifest), "utf8"));
  if (manifestDigest !== publication.manifest_digest) throw new Error("package publication manifest digest is invalid");
  const manifestMembers = manifest.members.map((member) => ({
    key: `${member.registry_kind}/${member.owner}`,
    primary: member.primary === true,
  })).sort((left, right) => left.key.localeCompare(right.key));
  const publicationMembers = publication.members.map((member) => ({
    key: `${member.registry_kind}/${member.owner}`,
    primary: member.primary === true,
  })).sort((left, right) => left.key.localeCompare(right.key));
  if (stableJson(manifestMembers) !== stableJson(publicationMembers)) {
    throw new Error("package publication members disagree with its local package manifest");
  }
  const exactFilePaths = new Set([publication.license.path, publication.manifest_path]);
  for (const member of publication.members) {
    const prefix = `registry/${member.registry_kind}/${member.owner}/`;
    const digestInput = member.files.map((file) => {
      if (!file.path.startsWith(prefix)) throw new Error(`package member file is outside '${prefix}': ${file.path}`);
      if (exactFilePaths.has(file.path)) throw new Error(`package publication repeats owner file '${file.path}'`);
      exactFilePaths.add(file.path);
      const globalEvidence = evidence.get(file.path);
      if (!globalEvidence || stableJson(globalEvidence) !== stableJson(file)) {
        throw new Error(`package member evidence disagrees with global file evidence: ${file.path}`);
      }
      return {
        path: file.path.slice(prefix.length),
        size: file.size,
        mode: file.mode,
        sha256: file.sha256,
      };
    });
    const digest = sha256(Buffer.from(stableJson(digestInput), "utf8"));
    if (digest !== member.digest) throw new Error(`package member digest is invalid: ${member.registry_kind}/${member.owner}`);
  }
  const sourceDigest = sha256(Buffer.from(stableJson({
    manifest_digest: publication.manifest_digest,
    members: publication.members.map((member) => ({
      registry_kind: member.registry_kind,
      owner: member.owner,
      digest: member.digest,
    })),
  }), "utf8"));
  if (sourceDigest !== publication.source_digest) throw new Error("package publication source digest is invalid");
  if (stableJson([...exactFilePaths].sort()) !== stableJson([...evidence.keys()].sort())) {
    throw new Error("package publication lists files outside its manifest, license, or registry owners");
  }
  const source = await readLocalPackageSourcePackage({
    sourceRegistryRoot: path.join(bundleRoot, "registry"),
    libraryRoot: bundleRoot,
    packageId: publication.package_id,
  });
  if (
    source.manifest_digest !== publication.manifest_digest
    || source.source_digest !== publication.source_digest
    || stableJson(normalizeSourceMembers(source.members)) !== stableJson(normalizePublishedMembers(publication.members))
  ) throw new Error("package publication does not re-admit as its declared local package source");
}

async function verifyClientExtension(bundleRoot, publication) {
  if (!validateClientExtensionPublication(publication)) {
    throw schemaError("client Extension publication", validateClientExtensionPublication.errors);
  }
  verifyCoreDigest(publication);
  if (publication.manifest_path !== `packages/${publication.extension_id}/extension.json`) {
    throw new Error("client Extension publication manifest path is not canonical");
  }
  const evidence = await verifyFiles(bundleRoot, publication.files);
  verifyLicense(publication, evidence);
  const manifestEvidence = evidence.get(publication.manifest_path);
  if (!manifestEvidence) throw new Error("client Extension publication does not list its manifest file");
  const manifestFile = path.join(bundleRoot, ...publication.manifest_path.split("/"));
  const { body, value: manifest } = await readJsonRegular(manifestFile, "client Extension manifest", 1024 * 1024);
  if (!validateClientExtensionManifest(manifest)) {
    throw schemaError("client Extension manifest", validateClientExtensionManifest.errors);
  }
  if (
    manifest.id !== publication.extension_id
    || manifest.version !== publication.extension_version
    || manifest.contribution.kind !== "surface"
  ) throw new Error("client Extension publication identity disagrees with its manifest");
  if (`sha256:${sha256(body)}` !== publication.source_digest) {
    throw new Error("client Extension publication source digest is invalid");
  }
  if (stableJson([...evidence.keys()].sort()) !== stableJson([
    publication.license.path,
    publication.manifest_path,
  ].sort())) throw new Error("client Extension publication contains files outside its manifest and license");
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
    throw new Error(`client Extension publication failed semantic admission: ${issues.map((issue) => `${issue.path}: ${issue.message}`).join("; ")}`);
  }
}

function verifyCoreDigest(publication) {
  const core = { ...publication };
  delete core.bundle_digest;
  if (publicationDigest(core) !== publication.bundle_digest) throw new Error("publication bundle digest is invalid");
}

async function verifyFiles(bundleRoot, listedFiles, options = {}) {
  const listed = new Map();
  for (const file of listedFiles) {
    if (file.path.includes("\\")) throw new Error(`publication file path is not portable: ${file.path}`);
    if (listed.has(file.path)) throw new Error(`publication repeats file '${file.path}'`);
    listed.set(file.path, file);
  }
  const actualPaths = await listBundleFiles(bundleRoot, bundleRoot);
  const expectedPaths = [...listed.keys(), "publication.json"].sort();
  if (stableJson(actualPaths) !== stableJson(expectedPaths)) {
    throw new Error("publication contains missing or unlisted files");
  }
  const verified = new Map();
  const privateRoots = publicationPrivateRoots();
  for (const [relative, expected] of listed) {
    const file = path.join(bundleRoot, ...relative.split("/"));
    const { body, stat } = await requireRegularFile(file, `publication file '${relative}'`);
    validatePublicPayload(relative, body, {
      privateRoots,
      rejectDevelopmentExecutables: options.rejectDevelopmentExecutables === true,
    });
    const actual = {
      path: relative,
      size: stat.size,
      mode: stat.mode & 0o777,
      sha256: sha256(body),
    };
    if (stableJson(actual) !== stableJson(expected)) throw new Error(`publication file evidence is invalid: ${relative}`);
    verified.set(relative, actual);
  }
  return verified;
}

function normalizeSourceMembers(members) {
  return members.map((member) => {
    const prefix = `registry/${member.registry_kind}/${member.owner}/`;
    return {
      registry_kind: member.registry_kind,
      owner: member.owner,
      primary: member.primary === true,
      digest: member.digest,
      files: member.files.map((file) => ({
        path: `${prefix}${file.relative_path.split(path.sep).join("/")}`,
        size: file.size,
        mode: file.mode,
        sha256: file.sha256,
      })).sort((left, right) => left.path.localeCompare(right.path)),
    };
  }).sort(memberOrder);
}

function normalizePublishedMembers(members) {
  return members.map((member) => ({
    registry_kind: member.registry_kind,
    owner: member.owner,
    primary: member.primary === true,
    digest: member.digest,
    files: [...member.files].sort((left, right) => left.path.localeCompare(right.path)),
  })).sort(memberOrder);
}

function memberOrder(left, right) {
  return `${left.registry_kind}/${left.owner}`.localeCompare(`${right.registry_kind}/${right.owner}`);
}

function verifyLicense(publication, evidence) {
  const file = evidence.get(publication.license.path);
  if (!file || file.sha256 !== publication.license.sha256) throw new Error("publication license evidence is invalid");
}

async function listBundleFiles(root, directory) {
  const result = [];
  const entries = (await fs.readdir(directory, { withFileTypes: true })).sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of entries) {
    const candidate = path.join(directory, entry.name);
    const stat = await fs.lstat(candidate);
    const relative = path.relative(root, candidate).split(path.sep).join("/");
    if (stat.isSymbolicLink()) throw new Error(`publication contains a symbolic link: ${relative}`);
    if (stat.isDirectory()) {
      result.push(...await listBundleFiles(root, candidate));
    } else if (stat.isFile()) {
      result.push(relative);
    } else {
      throw new Error(`publication contains a non-regular entry: ${relative}`);
    }
  }
  return result.sort();
}

function schemaError(label, errors) {
  const detail = (errors ?? []).slice(0, 12).map((error) => `${error.instancePath || "/"} ${error.message}`).join("; ");
  return new Error(`${label} does not satisfy its schema${detail ? `: ${detail}` : ""}`);
}

main().catch((error) => {
  process.stderr.write(`Publication verification failed: ${errorMessage(error)}\n`);
  process.exitCode = 1;
});
