const path = require("node:path");
const Ajv = require("ajv/dist/2020");

const publicationSchema = require("../schemas/package_publication.schema.json");
const { readLocalPackageSourcePackage } = require("../electron/localPackages.cjs");
const {
  assertDisjointPaths,
  assertOutsideDetectedOctopusHome,
  assertOutsidePrivateRoots,
  copyPublicationFile,
  createAtomicPublication,
  errorMessage,
  parseOptions,
  prettyJson,
  publicationDigest,
  publicationPrivateRoots,
  readPublicationLicense,
  readSourceRegistryIdentity,
  requiredAbsoluteOption,
  requiredOption,
  requirePlainDirectory,
  requireRegularFile,
  validatePublicPayload,
  writePublicationFile,
} = require("./publication-lib.cjs");

const validatePublication = new Ajv({ allErrors: true, strict: false, logger: false }).compile(publicationSchema);

async function main() {
  const options = parseOptions(process.argv.slice(2), new Set([
    "package",
    "source-registry",
    "library-root",
    "out",
    "license",
    "license-file",
  ]));
  const privateRoots = publicationPrivateRoots();
  const packageId = requiredOption(options, "package");
  const sourceRegistryRoot = await requirePlainDirectory(
    assertOutsidePrivateRoots(requiredAbsoluteOption(options, "source-registry"), "source registry", privateRoots),
    "source registry",
  );
  await assertOutsideDetectedOctopusHome(sourceRegistryRoot, "source registry");
  const libraryRoot = await requirePlainDirectory(
    assertOutsidePrivateRoots(requiredAbsoluteOption(options, "library-root"), "package library", privateRoots),
    "package library",
  );
  await assertOutsideDetectedOctopusHome(libraryRoot, "package library");
  const outRoot = requiredAbsoluteOption(options, "out");
  await assertDisjointPaths(outRoot, "publication output", sourceRegistryRoot, "source registry");
  await assertDisjointPaths(outRoot, "publication output", libraryRoot, "package library");
  const license = await readPublicationLicense(options, privateRoots);
  const sourceRegistryId = await readSourceRegistryIdentity(sourceRegistryRoot);
  const source = await readLocalPackageSourcePackage({
    sourceRegistryRoot,
    libraryRoot,
    packageId,
  });
  const bundleName = `${source.manifest.id}-${source.manifest.version}-${source.source_digest.slice(0, 12)}.octopus-package`;
  const result = await createAtomicPublication(outRoot, bundleName, async (stageRoot) => {
    const files = [];
    const licenseEvidence = await writePublicationFile(stageRoot, license.path, license.body, 0o644);
    files.push(licenseEvidence);
    const manifestPath = path.posix.join("packages", `${source.manifest.id}.json`);
    const manifestBody = prettyJson(source.manifest);
    validatePublicPayload(manifestPath, manifestBody, { privateRoots, rejectDevelopmentExecutables: true });
    files.push(await writePublicationFile(stageRoot, manifestPath, manifestBody, 0o644));
    const members = [];
    for (const member of source.members) {
      const memberFiles = [];
      for (const file of [...member.files].sort((left, right) => left.relative_path.localeCompare(right.relative_path))) {
        const relativePath = path.posix.join(
          "registry",
          member.registry_kind,
          member.owner,
          file.relative_path.split(path.sep).join("/"),
        );
        const { body } = await requireRegularFile(file.path, `package source '${relativePath}'`);
        validatePublicPayload(relativePath, body, { privateRoots, rejectDevelopmentExecutables: true });
        const evidence = await copyPublicationFile(stageRoot, relativePath, file.path, file);
        files.push(evidence);
        memberFiles.push(evidence);
      }
      members.push({
        registry_kind: member.registry_kind,
        owner: member.owner,
        ...(member.primary ? { primary: true } : {}),
        digest: member.digest,
        files: memberFiles,
      });
    }
    const publicationCore = {
      schema: "octopus.package_publication",
      schema_version: 1,
      layout: "octopus.local_package_library.v1",
      source_registry_id: sourceRegistryId,
      package_id: source.manifest.id,
      package_version: source.manifest.version,
      product_kind: source.manifest.product_kind,
      product_id: source.manifest.product_id,
      manifest_path: manifestPath,
      manifest_digest: source.manifest_digest,
      source_digest: source.source_digest,
      license: {
        spdx: license.spdx,
        path: license.path,
        sha256: license.sha256,
      },
      members,
      files: [...files].sort((left, right) => left.path.localeCompare(right.path)),
    };
    const publication = { ...publicationCore, bundle_digest: publicationDigest(publicationCore) };
    if (!validatePublication(publication)) throw schemaError(validatePublication.errors);
    await writePublicationFile(stageRoot, "publication.json", prettyJson(publication), 0o644);
    return {
      package_id: publication.package_id,
      package_version: publication.package_version,
      source_digest: publication.source_digest,
      bundle_digest: publication.bundle_digest,
      file_count: publication.files.length,
    };
  }, privateRoots);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

function schemaError(errors) {
  const detail = (errors ?? []).slice(0, 12).map((error) => `${error.instancePath || "/"} ${error.message}`).join("; ");
  return new Error(`package publication does not satisfy its schema${detail ? `: ${detail}` : ""}`);
}

main().catch((error) => {
  process.stderr.write(`Package publication refused: ${errorMessage(error)}\n`);
  process.exitCode = 1;
});
