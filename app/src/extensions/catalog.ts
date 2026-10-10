import builtinCatalog from "../../distribution/client-extensions/builtin-manifests.json";
import type { ClientExtensionManifest } from "./types";
import { admitClientExtensionManifest } from "./validation";
import { hasBuiltinClientExtensionSurface } from "./builtinSurfaceContributions";
import { hasBuiltinClientExtensionAction } from "./builtinActionContributions";

type BuiltinManifestSource = {
  source_digest: string;
  manifest: ClientExtensionManifest;
};

if (
  builtinCatalog.schema !== "octopus.builtin_client_extension_catalog"
  || builtinCatalog.schema_version !== 1
  || !Array.isArray(builtinCatalog.extensions)
) {
  throw new Error("built-in client extension catalog has an unsupported contract");
}

const catalog = builtinCatalog.extensions as unknown as BuiltinManifestSource[];

export const builtinClientExtensions = catalog.map(({ manifest, source_digest: sourceDigest }) => {
  if (!/^sha256:[a-f0-9]{64}$/.test(sourceDigest)) {
    throw new Error(`built-in client extension ${manifest.id} has an invalid source digest`);
  }
  if (
    manifest.category === "tab"
    && manifest.contribution.kind === "builtin"
    && !hasBuiltinClientExtensionSurface(manifest.contribution.surface_id)
  ) {
    throw new Error(`built-in tab extension ${manifest.id} has no registered surface contribution`);
  }
  if (
    manifest.contribution.kind === "builtin_action"
    && !hasBuiltinClientExtensionAction(manifest.contribution.action_id)
  ) {
    throw new Error(`built-in repair extension ${manifest.id} has no registered action contribution`);
  }
  const result = admitClientExtensionManifest(
    manifest,
    sourceDigest,
    `compiled:${manifest.id}`,
  );
  if (!result.admitted) {
    const detail = result.verdict.issues.map((issue) => `${issue.path}: ${issue.message}`).join("; ");
    throw new Error(`built-in client extension ${manifest.id} failed admission: ${detail}`);
  }
  return result.admitted;
});
