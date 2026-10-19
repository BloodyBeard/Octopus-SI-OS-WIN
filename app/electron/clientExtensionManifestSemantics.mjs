export function clientExtensionManifestSemanticIssues(manifest, options = {}) {
  const issues = [];
  const compiled = options.compiled === true;

  if (!compiled && manifest.id.startsWith("builtin.")) {
    issues.push({
      code: "manifest.id.reserved",
      path: "/id",
      message: "custom extension ids must not use the reserved builtin. prefix",
    });
  }
  if (!compiled && manifest.default_installed) {
    issues.push({
      code: "manifest.installation.implicit",
      path: "/default_installed",
      message: "downloaded extensions cannot install themselves by default",
    });
  }
  if (!compiled && (
    manifest.contribution.kind === "builtin"
    || manifest.contribution.kind === "builtin_action"
    || manifest.contribution.kind === "builtin_input"
  )) {
    issues.push({
      code: "manifest.contribution.reserved",
      path: "/contribution",
      message: "downloaded extensions cannot claim a built-in client contribution",
    });
  }

  const sourceIds = new Set();
  for (const [index, source] of (manifest.sources ?? []).entries()) {
    if (sourceIds.has(source.id)) {
      issues.push({
        code: "manifest.source.id.duplicate",
        path: `/sources/${index}/id`,
        message: `source id "${source.id}" is duplicated`,
      });
    }
    sourceIds.add(source.id);
    if (source.kind !== "filesystem") continue;
    if (source.selection === "directory" && source.read !== "directory") {
      issues.push({
        code: "manifest.source.filesystem.mode.invalid",
        path: `/sources/${index}/read`,
        message: "directory selection must use directory reading",
      });
    }
    if (source.selection === "file" && source.read === "directory") {
      issues.push({
        code: "manifest.source.filesystem.mode.invalid",
        path: `/sources/${index}/read`,
        message: "file selection cannot use directory reading",
      });
    }
  }

  if (manifest.contribution.kind === "builtin_action") {
    if (manifest.category !== "repair") {
      issues.push({
        code: "manifest.category.mismatch",
        path: "/category",
        message: `category ${manifest.category} does not match a built-in repair action`,
      });
    }
    const triggerKeys = new Set();
    for (const [index, trigger] of manifest.contribution.triggers.entries()) {
      const key = `${trigger.incident_code}\u0000${trigger.interface_id}`;
      if (triggerKeys.has(key)) {
        issues.push({
          code: "manifest.trigger.duplicate",
          path: `/contribution/triggers/${index}`,
          message: `trigger for "${trigger.incident_code}" on "${trigger.interface_id}" is duplicated`,
        });
      }
      triggerKeys.add(key);
    }
  } else if (manifest.category === "repair") {
    issues.push({
      code: "manifest.category.mismatch",
      path: "/category",
      message: "repair extensions must contribute a built-in repair action",
    });
  }

  if (manifest.contribution.kind === "builtin_input") {
    if (manifest.category !== "input") {
      issues.push({
        code: "manifest.category.mismatch",
        path: "/category",
        message: `category ${manifest.category} does not match a built-in input`,
      });
    }
    const provider = manifest.contribution.provider;
    const credential = (manifest.sources ?? []).find((source) => source.id === provider.credential_source);
    if (!credential || credential.kind !== "credential") {
      issues.push({
        code: "manifest.provider.credential.missing",
        path: "/contribution/provider/credential_source",
        message: `provider credential "${provider.credential_source}" is not a declared credential source`,
      });
    } else if (new URL(provider.endpoint).origin !== credential.origin) {
      issues.push({
        code: "manifest.provider.origin.mismatch",
        path: "/contribution/provider/endpoint",
        message: `provider endpoint must use the credential origin ${credential.origin}`,
      });
    }
  } else if (manifest.category === "input") {
    issues.push({
      code: "manifest.category.mismatch",
      path: "/category",
      message: "input extensions must contribute a built-in input",
    });
  }

  if (manifest.contribution.kind === "surface") {
    for (const [index, source] of (manifest.contribution.document.sources ?? []).entries()) {
      if (!sourceIds.has(source.id)) continue;
      issues.push({
        code: "manifest.source.id.surface_conflict",
        path: `/contribution/document/sources/${index}/id`,
        message: `projection source id "${source.id}" conflicts with a client-configured source`,
      });
    }
    const targetKind = manifest.contribution.document.target.kind;
    const targetCategory = targetKind === "voice.widget" || targetKind === "voice.window"
      ? "widget"
      : targetKind === "workspace.surface"
        ? "tab"
        : null;
    if (!targetCategory) {
      issues.push({
        code: "manifest.contribution.target.unsupported",
        path: "/contribution/document/target/kind",
        message: "Extensions may contribute only a widget/window or a workspace tab",
      });
    } else if (targetCategory !== manifest.category) {
      issues.push({
        code: "manifest.category.mismatch",
        path: "/category",
        message: `category ${manifest.category} does not match ${targetKind}`,
      });
    }
  }

  return issues;
}
