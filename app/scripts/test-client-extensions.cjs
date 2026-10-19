const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");

const { createClientExtensionStore } = require("../electron/clientExtensions.cjs");
const {
  PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
  readClientExtensionActionStatuses,
} = require("../electron/clientExtensionActions.cjs");

async function main() {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-client-extensions-"));
  const root = path.join(temporary, "extensions");
  let store = createClientExtensionStore({ root });
  try {
    let snapshot = await store.snapshot();
    assert.equal(snapshot.stateStatus, "valid");
    assert.equal(snapshot.lifecycle[PROVIDER_CREDENTIAL_REPAIR_EXTENSION].receiptState, "valid");
    assert.equal(snapshot.lifecycle[PROVIDER_CREDENTIAL_REPAIR_EXTENSION].desiredState, "available");
    assert.equal(snapshot.lifecycle["builtin.metrics"].desiredState, "installed");

    let statuses = await readClientExtensionActionStatuses({
      octopusHome: temporary,
      codexHome: temporary,
      installedExtensionIds: verifiedInstalled(snapshot),
    });
    assert.equal(statuses[0].installed, false);
    assert.equal(statuses[0].actionAvailable, false);

    const repairRevision = store.builtinRevision(PROVIDER_CREDENTIAL_REPAIR_EXTENSION);
    const installed = await store.applyLifecycle({
      action: "install",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_install_repair_001",
      expectedGeneration: snapshot.generation,
      expectedRevision: repairRevision,
    });
    assert.equal(installed.changed, true);
    assert.equal(installed.receipt.desiredState, "installed");
    assert.match(installed.receipt.receiptDigest, /^[a-f0-9]{64}$/);
    snapshot = installed.snapshot;
    statuses = await readClientExtensionActionStatuses({
      octopusHome: temporary,
      codexHome: temporary,
      installedExtensionIds: verifiedInstalled(snapshot),
    });
    assert.equal(statuses[0].installed, true);

    store.close();
    store = createClientExtensionStore({ root });
    snapshot = await store.snapshot();
    assert.equal(snapshot.lifecycle[PROVIDER_CREDENTIAL_REPAIR_EXTENSION].desiredState, "installed");
    const repeated = await store.applyLifecycle({
      action: "install",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_install_repair_001",
      expectedGeneration: snapshot.generation,
      expectedRevision: repairRevision,
    });
    assert.equal(repeated.changed, false);
    assert.equal(repeated.receipt.receiptDigest, installed.receipt.receiptDigest);
    await assert.rejects(store.applyLifecycle({
      action: "remove",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_install_repair_001",
      expectedGeneration: repeated.snapshot.generation,
      expectedRevision: repairRevision,
    }), /already has another request/);

    const removedRepair = await store.applyLifecycle({
      action: "remove",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_remove_repair_002",
      expectedGeneration: repeated.snapshot.generation,
      expectedRevision: repairRevision,
    });
    await delay(250);
    assert.equal(store.generation(), removedRepair.snapshot.generation);
    await assert.rejects(store.applyLifecycle({
      action: "remove",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_install_repair_001",
      expectedGeneration: removedRepair.snapshot.generation,
      expectedRevision: repairRevision,
    }), /already has another request/);
    const restoredRepair = await store.applyLifecycle({
      action: "install",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_install_repair_001",
      expectedGeneration: removedRepair.snapshot.generation,
      expectedRevision: repairRevision,
    });
    assert.equal(restoredRepair.changed, true);
    assert.equal(restoredRepair.receipt.receiptDigest, installed.receipt.receiptDigest);

    let releaseAuthorizedAction;
    let markAuthorizedActionEntered;
    const authorizedActionGate = new Promise((resolve) => { releaseAuthorizedAction = resolve; });
    const authorizedActionEntered = new Promise((resolve) => { markAuthorizedActionEntered = resolve; });
    const heldAction = store.withVerifiedLifecycleSnapshot(async ({ installedExtensionIds }) => {
      assert.ok(installedExtensionIds.includes(PROVIDER_CREDENTIAL_REPAIR_EXTENSION));
      markAuthorizedActionEntered();
      await authorizedActionGate;
      return "authorized";
    });
    await authorizedActionEntered;
    let queuedRemovalSettled = false;
    const queuedRemoval = store.applyLifecycle({
      action: "remove",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_concurrent_remove_003",
      expectedGeneration: restoredRepair.snapshot.generation,
      expectedRevision: repairRevision,
    }).finally(() => { queuedRemovalSettled = true; });
    await delay(50);
    assert.equal(queuedRemovalSettled, false);
    releaseAuthorizedAction();
    assert.equal(await heldAction, "authorized");
    const concurrentlyRemoved = await queuedRemoval;
    assert.equal(concurrentlyRemoved.receipt.desiredState, "available");
    const concurrencyRestored = await store.applyLifecycle({
      action: "install",
      id: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      operationId: "extension_install_repair_001",
      expectedGeneration: concurrentlyRemoved.snapshot.generation,
      expectedRevision: repairRevision,
    });

    const memoryId = "builtin.voice.memory";
    snapshot = await store.mutate({
      id: memoryId,
      expectedGeneration: concurrencyRestored.snapshot.generation,
      sources: { vault: { path: path.join(temporary, "vault") } },
    });
    await delay(250);
    assert.equal(store.generation(), snapshot.generation);
    const removed = await store.applyLifecycle({
      action: "remove",
      id: memoryId,
      operationId: "extension_remove_memory_001",
      expectedGeneration: snapshot.generation,
      expectedRevision: store.builtinRevision(memoryId),
    });
    assert.equal(removed.receipt.desiredState, "available");
    assert.equal(removed.snapshot.state[memoryId].sources.vault.path, path.join(temporary, "vault"));
    assert.equal(removed.snapshot.lifecycle[memoryId].desiredState, "available");

    const downloadedBuiltinId = "builtin.downloaded-review-required";
    const downloadedRoot = path.join(root, "packages", downloadedBuiltinId);
    await fs.mkdir(downloadedRoot, { recursive: true });
    await fs.writeFile(path.join(downloadedRoot, "extension.json"), `${JSON.stringify({
      schema: "octopus.client_extension",
      schema_version: 1,
      id: downloadedBuiltinId,
      version: "1.0.0",
      name: "Downloaded built-in prefix",
      description: "A downloaded package must not inherit first-party admission from its name.",
      developer: "Test",
      category: "widget",
      glyph: "metrics",
      default_installed: false,
      contribution: {
        kind: "builtin",
        surface_id: "downloaded-test",
      },
    }, null, 2)}\n`, "utf8");
    snapshot = await store.snapshot();
    await assert.rejects(
      store.currentRevision(downloadedBuiltinId),
      /reserved builtin/,
    );

    const semanticId = "custom.semantic-admission";
    const semanticRoot = path.join(root, "packages", semanticId);
    await fs.mkdir(semanticRoot, { recursive: true });
    await fs.writeFile(
      path.join(semanticRoot, "extension.json"),
      `${JSON.stringify(surfaceExtensionManifest(semanticId, "widget"), null, 2)}\n`,
      "utf8",
    );
    await assert.rejects(
      store.currentRevision(semanticId),
      /category widget does not match workspace\.surface/,
    );
    await fs.writeFile(
      path.join(semanticRoot, "extension.json"),
      `${JSON.stringify(surfaceExtensionManifest(semanticId, "tab"), null, 2)}\n`,
      "utf8",
    );
    snapshot = await store.snapshot();
    const semanticRevision = await store.currentRevision(semanticId);
    await assert.rejects(store.applyLifecycle({
      action: "install",
      id: semanticId,
      operationId: "extension_semantic_install_001",
      expectedGeneration: snapshot.generation,
      expectedRevision: semanticRevision,
    }), /unreviewed/);
    snapshot = await store.mutate({
      id: semanticId,
      expectedGeneration: snapshot.generation,
      reviewed: semanticRevision,
    });
    const semanticInstalled = await store.applyLifecycle({
      action: "install",
      id: semanticId,
      operationId: "extension_semantic_install_001",
      expectedGeneration: snapshot.generation,
      expectedRevision: semanticRevision,
    });
    assert.equal(semanticInstalled.snapshot.lifecycle[semanticId].receiptState, "valid");
    await fs.writeFile(
      path.join(semanticRoot, "extension.json"),
      `${JSON.stringify(surfaceExtensionManifest(semanticId, "widget"), null, 2)}\n`,
      "utf8",
    );
    snapshot = await store.snapshot();
    assert.equal(snapshot.lifecycle[semanticId].receiptState, "stale");
    assert.match(snapshot.errors.join("\n"), /category widget does not match workspace\.surface/);

    const invalidLayoutId = "custom.invalid-layout";
    const invalidLayoutRoot = path.join(root, "packages", invalidLayoutId);
    await fs.mkdir(invalidLayoutRoot, { recursive: true });
    await fs.writeFile(
      path.join(invalidLayoutRoot, "extension.json"),
      `${JSON.stringify(voiceSurfaceExtensionManifest(invalidLayoutId), null, 2)}\n`,
      "utf8",
    );
    await assert.rejects(
      store.currentRevision(invalidLayoutId),
      /min_width <= preferred_width <= max_width/,
    );

    const oversizedId = "custom.oversized-surface";
    const oversizedRoot = path.join(root, "packages", oversizedId);
    await fs.mkdir(oversizedRoot, { recursive: true });
    const oversizedManifest = surfaceExtensionManifest(oversizedId, "tab");
    oversizedManifest.contribution.document.data = { payload: "d".repeat(180_000) };
    oversizedManifest.contribution.document.render = {
      kind: "sandbox",
      html: "h".repeat(250_000),
      css: "c".repeat(120_000),
      javascript: "j".repeat(120_000),
    };
    await fs.writeFile(
      path.join(oversizedRoot, "extension.json"),
      `${JSON.stringify(oversizedManifest)}\n`,
      "utf8",
    );
    await assert.rejects(
      store.currentRevision(oversizedId),
      /maximum is 655360/,
    );

    const crashRecoveredId = "builtin.usage";
    const crashRecoveredOperationId = "extension_crash_recovery_001";
    snapshot = await store.snapshot();
    const crashCommitted = await store.applyLifecycle({
      action: "install",
      id: crashRecoveredId,
      operationId: crashRecoveredOperationId,
      expectedGeneration: snapshot.generation,
      expectedRevision: store.builtinRevision(crashRecoveredId),
    });
    store.close();
    const crashHistoryPath = path.join(
      root,
      "lifecycle-operations",
      crashRecoveredId,
      `${crashRecoveredOperationId}.json`,
    );
    await fs.unlink(crashHistoryPath);
    store = createClientExtensionStore({ root });
    snapshot = await store.snapshot();
    assert.equal(snapshot.lifecycle[crashRecoveredId].desiredState, "installed");
    assert.equal(
      JSON.parse(await fs.readFile(crashHistoryPath, "utf8")).request_digest,
      crashCommitted.receipt.requestDigest,
    );
    const crashReplay = await store.applyLifecycle({
      action: "install",
      id: crashRecoveredId,
      operationId: crashRecoveredOperationId,
      expectedGeneration: snapshot.generation,
      expectedRevision: store.builtinRevision(crashRecoveredId),
    });
    assert.equal(crashReplay.changed, false);
    await assert.rejects(store.applyLifecycle({
      action: "remove",
      id: crashRecoveredId,
      operationId: crashRecoveredOperationId,
      expectedGeneration: crashReplay.snapshot.generation,
      expectedRevision: store.builtinRevision(crashRecoveredId),
    }), /already has another request/);

    const crashReceiptPath = path.join(root, "lifecycle-receipts", `${crashRecoveredId}.json`);
    const inconsistentReceipt = JSON.parse(await fs.readFile(crashReceiptPath, "utf8"));
    inconsistentReceipt.request_digest = "0".repeat(64);
    await fs.writeFile(crashReceiptPath, `${JSON.stringify(inconsistentReceipt, null, 2)}\n`, "utf8");
    snapshot = await store.snapshot();
    assert.equal(snapshot.lifecycle[crashRecoveredId].receiptState, "invalid");
    assert.match(snapshot.lifecycle[crashRecoveredId].error, /request digest/);
    const repairedActiveReceipt = await store.applyLifecycle({
      action: "install",
      id: crashRecoveredId,
      operationId: crashRecoveredOperationId,
      expectedGeneration: snapshot.generation,
      expectedRevision: store.builtinRevision(crashRecoveredId),
    });
    assert.equal(repairedActiveReceipt.changed, true);
    assert.equal(repairedActiveReceipt.snapshot.lifecycle[crashRecoveredId].receiptState, "valid");

    const inputId = "builtin.input.gesture-intent";
    const secret = "sk-test-credential-value";
    snapshot = await store.setCredential({ id: inputId, sourceId: "decision_key", secret, expectedGeneration: store.generation() });
    assert.deepEqual(Object.keys(snapshot.credentials[inputId]), ["decision_key"]);
    assert.equal(snapshot.credentials[inputId].decision_key.origin, "https://openrouter.ai");
    assert.equal(JSON.stringify(snapshot).includes(secret), false);
    const credentialFile = path.join(root, "credentials", `${inputId}.json`);
    assert.equal((await fs.stat(credentialFile)).mode & 0o777, 0o600);
    assert.deepEqual(await store.credential(inputId, "decision_key"), {
      origin: "https://openrouter.ai",
      checkPath: "/api/v1/key",
      secret,
    });
    await assert.rejects(
      store.setCredential({ id: inputId, sourceId: "missing", secret, expectedGeneration: snapshot.generation }),
      /declares no credential source/,
    );
    const moved = JSON.parse(await fs.readFile(credentialFile, "utf8"));
    moved.sources.decision_key.origin = "https://elsewhere.example";
    await fs.writeFile(credentialFile, `${JSON.stringify(moved)}\n`, { mode: 0o600 });
    await assert.rejects(store.credential(inputId, "decision_key"), /save it again for https:\/\/openrouter\.ai/);
    snapshot = await store.snapshot();
    snapshot = await store.setCredential({ id: inputId, sourceId: "decision_key", secret: null, expectedGeneration: snapshot.generation });
    assert.equal(snapshot.credentials[inputId], undefined);
    await assert.rejects(fs.stat(credentialFile), /ENOENT/);

    const repairReceiptPath = path.join(root, "lifecycle-receipts", `${PROVIDER_CREDENTIAL_REPAIR_EXTENSION}.json`);
    await fs.writeFile(repairReceiptPath, "{not-json\n", "utf8");
    snapshot = await store.snapshot();
    assert.equal(snapshot.lifecycle[PROVIDER_CREDENTIAL_REPAIR_EXTENSION].receiptState, "invalid");
    assert.match(snapshot.errors.join("\n"), /lifecycle receipt is invalid/);
    assert.equal(verifiedInstalled(snapshot).includes(PROVIDER_CREDENTIAL_REPAIR_EXTENSION), false);

    await fs.writeFile(path.join(root, "state.json"), "{not-json\n", "utf8");
    snapshot = await store.snapshot();
    assert.equal(snapshot.stateStatus, "invalid");
    assert.equal(verifiedInstalled(snapshot).length, 0);
  } finally {
    store.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function surfaceExtensionManifest(id, category) {
  return {
    schema: "octopus.client_extension",
    schema_version: 1,
    id,
    version: "1.0.0",
    name: "Semantic admission",
    description: "A package used to prove shared host and renderer semantic admission.",
    developer: "Test",
    category,
    glyph: "metrics",
    default_installed: false,
    contribution: {
      kind: "surface",
      document: {
        schema: "octopus.client_surface",
        schema_version: 1,
        release_channel: "stable",
        id: `${id}.surface`,
        revision: 1,
        title: "Semantic admission",
        glyph: "metrics",
        target: {
          kind: "workspace.surface",
          placements: ["home"],
          order: 100,
          chrome: { topbar: "none", workspace: "panel" },
        },
        render: { kind: "native", root: { type: "text", value: "Ready" } },
      },
    },
  };
}

function voiceSurfaceExtensionManifest(id) {
  const manifest = surfaceExtensionManifest(id, "widget");
  manifest.contribution.document.target = {
    kind: "voice.widget",
    slot: "left",
    order: 100,
    layout: {
      min_width: 500,
      preferred_width: 200,
      max_width: 600,
      min_height: 100,
      preferred_height: 200,
      max_height: 300,
      content_fit: "reflow",
    },
  };
  return manifest;
}

function verifiedInstalled(snapshot) {
  if (snapshot.stateStatus !== "valid") return [];
  return Object.entries(snapshot.lifecycle).flatMap(([id, evidence]) => (
    evidence.receiptState === "valid" && evidence.desiredState === "installed" ? [id] : []
  ));
}

void main().catch((error) => {
  process.stderr.write(`${error?.stack || error}\n`);
  process.exitCode = 1;
});
