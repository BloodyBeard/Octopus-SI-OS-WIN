module.exports = { configurationProjection };

const path = require("node:path");
const { readClientAgentCatalog } = require("../clientAgentCatalog.cjs");
const { readLocalPackageCatalog } = require("../localPackages.cjs");
const { readWorkspaceSetupState } = require("../workspaceSetup.cjs");
const { readApplicationPackageCatalog } = require("./applicationCatalog.cjs");
const { DESCRIPTOR_REGISTRIES } = require("./constants.cjs");
const { readOperatorChatProtocol, readProviderInterfaceDescriptors } = require("./providerDescriptors.cjs");
const { readEeInstanceRecords, readHostRootRefs, readRunRecords, validateEePolicyProjectionSnapshot } = require("./registryRows.cjs");
const { normalizeRuntimeLaunchEnvironment } = require("./sidecars.cjs");
const { arrayValue, stringValue } = require("./values.cjs");

function configurationProjection(store) {
  let configurationProjectionGeneration = 1;
  let configurationProjectionBuild = null;
  let policyProjectionBuild = null;
  let policyProjectionGeneration = 1;

  async function cachedConfigurationProjection(repoRoot) {
    const scope = store.fileProjectionScope(repoRoot);
    for (;;) {
      if (scope !== store.fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during configuration read");
      let build = configurationProjectionBuild;
      if (!build || build.scope !== scope) {
        build = { scope, generation: configurationProjectionGeneration, promise: buildConfigurationProjection(repoRoot) };
        configurationProjectionBuild = build;
      }
      try {
        const projection = await build.promise;
        if (scope !== store.fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during configuration read");
        if (build.generation === configurationProjectionGeneration) return projection;
      } catch (error) {
        if (build.generation === configurationProjectionGeneration) {
          if (configurationProjectionBuild === build) configurationProjectionBuild = null;
          throw error;
        }
      }
      if (configurationProjectionBuild === build) configurationProjectionBuild = null;
    }
  }

  async function buildConfigurationProjection(repoRoot) {
    const warnings = store.projectionWatchWarnings();
    const homeRoot = store.octopusHome();
    const durableRoot = store.resolveDurableRoot();
    const registryRoot = path.join(homeRoot, "registry");
    const stateRoot = path.join(homeRoot, "state");
    const runRecordsRoot = path.join(stateRoot, "runs");
    let hostRootRefs = {};
    let hostManifestError = "";
    try {
      hostRootRefs = await readHostRootRefs(homeRoot);
    } catch (error) {
      hostManifestError = String(error?.message ?? error);
      warnings.push(hostManifestError);
    }
    let launchEnvironment = null;
    try {
      launchEnvironment = normalizeRuntimeLaunchEnvironment(store.runtimeLaunchEnvironment());
    } catch (error) {
      warnings.push(`Managed runtime launch environment could not be verified: ${String(error?.message ?? error)}`);
    }
    const descriptorEntries = await Promise.all(DESCRIPTOR_REGISTRIES.map(async (registry) => {
      return [registry.kind, await store.readDescriptorRows(registryRoot, registry, warnings, launchEnvironment)];
    }));
    const descriptors = Object.fromEntries(descriptorEntries);
    const sourceRegistryRoot = store.resolveSourceRegistry();
    const localPackageRegistryRoot = store.resolveLocalPackageRegistry();
    const localPackageLibraryRoot = store.resolveLocalPackageLibrary();
    const localPackages = await readLocalPackageCatalog({
      sourceRegistryRoot: localPackageRegistryRoot,
      activeRegistryRoot: registryRoot,
      libraryRoot: localPackageLibraryRoot,
      stateRoot: path.join(homeRoot, "client", "package-state"),
    });
    warnings.push(...arrayValue(localPackages.errors));
    const setupSourceProviders = await readProviderInterfaceDescriptors(localPackageRegistryRoot);
    const setupInstalledProviders = await readProviderInterfaceDescriptors(registryRoot);
    for (const pack of localPackages.packs) {
      const id = pack.workspace_setup?.home?.provider_interface_id;
      if (!id) continue;
      const owner = pack.components.find((component) => component.product_id === id);
      const providers = owner?.materialized === true || owner?.partially_materialized === true ? setupInstalledProviders : setupSourceProviders;
      pack.setup_provider = providers.find((provider) => provider.id === id) ?? null;
    }
    let clientAgentCatalog = null;
    let clientAgentCatalogError = "";
    try {
      clientAgentCatalog = await readClientAgentCatalog(homeRoot);
      if (!clientAgentCatalog.exists) {
        clientAgentCatalogError = `Client Agent catalog is absent: ${clientAgentCatalog.file}`;
        warnings.push(clientAgentCatalogError);
      }
    } catch (error) {
      clientAgentCatalogError = String(error?.message ?? error);
      warnings.push(clientAgentCatalogError);
    }
    const applicationPackages = await readApplicationPackageCatalog(
      registryRoot,
      localPackageRegistryRoot,
      warnings,
    );
    const descriptorIndexes = Object.fromEntries(Object.entries(descriptors).map(([kind, rows]) => [
      kind,
      new Map(rows.flatMap((row) => {
        const identities = new Set([
          stringValue(row.id),
          stringValue(row.rule_id),
          stringValue(row.name),
          stringValue(row.registry_dir),
        ].filter(Boolean));
        return [...identities].map((identity) => [identity, row]);
      })),
    ]));
    const eeInstances = await readEeInstanceRecords(path.join(stateRoot, "ee_instances"), warnings);
    let eePolicyProjection = { environments: [], issues: [] };
    let eePolicyProjectionError = "";
    try {
      eePolicyProjection = await cachedEePolicyProjection();
      warnings.push(...eePolicyProjection.issues);
    } catch (error) {
      eePolicyProjectionError = String(error?.message ?? error);
      warnings.push(eePolicyProjectionError);
    }
    const operatorChatProtocol = await readOperatorChatProtocol(registryRoot);
    return {
      repoRoot,
      octopusHome: homeRoot,
      registryRoot,
      sourceRegistryRoot,
      localPackageRegistryRoot,
      localPackageLibraryRoot,
      stateRoot,
      runRecordsRoot,
      durableRoot,
      descriptors,
      localPackages,
      clientAgentCatalog,
      clientAgentCatalogError,
      applicationPackages,
      descriptorIndexes,
      operatorChatProtocol,
      eeInstances,
      eePolicyProjection,
      eePolicyProjectionError,
      hostRootRefs,
      hostManifestError,
      warnings,
    };
  }

  async function buildFileProjection(repoRoot) {
    const configuration = await cachedConfigurationProjection(repoRoot);
    const warnings = [...configuration.warnings];
    const runRecordProjection = await readRunRecords(configuration.runRecordsRoot, warnings);
    let workspaceSetup;
    try {
      workspaceSetup = await (store.options.readWorkspaceSetupState ?? readWorkspaceSetupState)({ octopusHomeRoot: configuration.octopusHome, registryRoot: configuration.registryRoot });
      warnings.push(...arrayValue(workspaceSetup.errors));
    } catch (error) {
      const message = String(error?.message ?? error);
      workspaceSetup = { status: "degraded", receipt: null, errors: [message] };
      warnings.push(message);
    }
    const workerEnvironments = await store.readWorkerEnvironmentRecords(configuration.octopusHome, warnings, configuration.descriptorIndexes.worker_environment_kind);
    const cachedLedger = store.ledgerProjectionSnapshot?.durableRoot === configuration.durableRoot
      ? store.ledgerProjectionSnapshot
      : null;
    return {
      ...configuration,
      workspaceSetup,
      workerEnvironments,
      generatedAt: new Date().toISOString(),
      generation: store.projectionGeneration,
      ledgerRows: cachedLedger?.rows ?? [],
      ledgerRowsByRunSources: cachedLedger?.rowsByRunSources ?? [],
      invalidLedgerRows: cachedLedger?.invalidRows ?? [],
      ledgerRevision: cachedLedger?.revision ?? "",
      structureMemo: new Map(),
      ledgerMemo: new Map(),
      operatorChatsListCache: null,
      operatorActivityCache: null,
      topologySnapshotCache: null,
      operatorChatMessagesCache: new Map(),
      operatorChatRunIndexCache: null,
      operatorChatStatusCursor: -1,
      operatorChatStatusInitialized: false,
      operatorChatStatusRefreshTail: Promise.resolve(),
      operatorChatTurnStateCache: new Map(),
      runRecords: runRecordProjection.records,
      invalidRunRecords: runRecordProjection.invalidRecords,
      warnings,
    };
  }

  async function cachedEePolicyProjection() {
    if (typeof store.options.projectEePolicies !== "function") {
      throw new Error("canonical EE policy projector is unavailable");
    }
    while (true) {
      let build = policyProjectionBuild;
      if (!build) {
        const generation = policyProjectionGeneration;
        const promise = store.options.projectEePolicies().then(validateEePolicyProjectionSnapshot);
        build = { generation, promise };
        policyProjectionBuild = build;
      }
      try {
        const snapshot = await build.promise;
        if (build.generation === policyProjectionGeneration) return snapshot;
      } catch (error) {
        if (build.generation === policyProjectionGeneration) {
          if (policyProjectionBuild === build) policyProjectionBuild = null;
          throw error;
        }
      }
      if (policyProjectionBuild === build) policyProjectionBuild = null;
    }
  }

  function invalidateConfigurationProjection(configurationChanged, policyChanged) {
    if (configurationChanged) configurationProjectionGeneration += 1;
    if (policyChanged) policyProjectionGeneration += 1;
  }

  function closeConfigurationProjection() {
    configurationProjectionBuild = null;
  }

  return { buildConfigurationProjection, buildFileProjection, cachedConfigurationProjection, cachedEePolicyProjection, closeConfigurationProjection, invalidateConfigurationProjection };
}
