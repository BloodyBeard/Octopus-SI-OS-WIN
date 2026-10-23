const fs = require("node:fs/promises");
const path = require("node:path");
const { modelServiceTiers, validateModelServiceTier } = require("./providerModelTiers.cjs");

// Desktop account/catalog operations do not invoke a model or require a Home run.
async function readProvider(registryRoot, id) {
  if (typeof id !== "string" || !/^[A-Za-z0-9_-]+$/.test(id)) {
    throw new Error("Invalid provider interface id.");
  }
  const descriptor = JSON.parse(await fs.readFile(path.join(registryRoot, "interface", id, "interface.json"), "utf8"));
  if (descriptor.id !== id || descriptor.interface_type !== "world" || descriptor.role !== "provider") {
    throw new Error(`Interface '${id}' is not a provider.`);
  }
  return descriptor;
}

async function loadClientMetadata(registryRoot, descriptor) {
  const metadata = descriptor.interface_driver_config?.private_config?.client_metadata;
  if (metadata === undefined) return null;
  const parts = typeof metadata?.module === "string" && metadata.module.match(/^interface:\/\/([A-Za-z0-9_-]+)\/(packages\/.+\.cjs)$/);
  if (metadata?.schema_version !== 1 || !parts || parts[2].split("/").some(part => !part || part === "." || part === ".." || /[\\\x00]/.test(part))) {
    throw new Error(`Provider '${descriptor.id}' has an invalid client metadata module.`);
  }
  const owner = await readProvider(registryRoot, parts[1]);
  if (owner.interface_driver_config?.private_config?.client_metadata?.module !== metadata.module) {
    throw new Error(`Provider '${descriptor.id}' metadata is not declared by its installed owner.`);
  }
  const root = await fs.realpath(registryRoot);
  const file = path.join(root, "interface", parts[1], parts[2]);
  if (await fs.realpath(file) !== file || !(await fs.stat(file)).isFile()) {
    throw new Error(`Provider '${descriptor.id}' metadata must be a regular registry file.`);
  }
  delete require.cache[file];
  const client = require(file);
  if (client?.schemaVersion !== 1 || typeof client.modelCatalog !== "function") {
    throw new Error(`Provider '${descriptor.id}' has an unsupported client metadata contract.`);
  }
  return client;
}

function createClientProviderMetadata({ registryRoot, sourceRegistryRoot, accountClient, reference }) {
  async function catalog(providers, selectedRegistryRoot = registryRoot) {
    const root = selectedRegistryRoot();
    const cache = new Map();
    const result = await Promise.all(providers.map(async (provider) => {
      try {
        const descriptor = await readProvider(root, provider.id);
        const client = await loadClientMetadata(root, descriptor);
        const discovered = validateModelCatalog(client
          ? await client.modelCatalog({ descriptor, accountClient, cache })
          : descriptorModelCatalog(descriptor), descriptor.id);
        if (JSON.stringify(await readProvider(root, descriptor.id)) !== JSON.stringify(descriptor)) {
          throw new Error(`Provider '${descriptor.id}' changed during model discovery. Refresh the catalogue.`);
        }
        const allowed = descriptor.interface_policy?.variables?.models?.default;
        if (!Array.isArray(allowed) || !allowed.every((value) => typeof value === "string" && value)) {
          throw new Error(`Provider '${provider.id}' has no declared model list.`);
        }
        const models = discovered.filter((model) => allowed.includes("*") || allowed.includes(model.model));
        if (!models.length) throw new Error(`Provider '${provider.id}' returned no available declared models.`);
        return { ...provider, models, modelCatalogError: "" };
      } catch (error) {
        return { ...provider, models: [], modelCatalogError: error instanceof Error ? error.message : String(error) };
      }
    }));
    if (selectedRegistryRoot() !== root) throw new Error("Provider catalogue scope changed during discovery.");
    return result;
  }
  async function accountOperation(plan, operation) {
    const root = registryRoot();
    const descriptor = await readProvider(root, plan.provider.interface_id);
    const client = await loadClientMetadata(root, descriptor);
    if (typeof client?.[operation] !== "function") {
      throw new Error(`Provider '${descriptor.id}' does not support desktop account setup.`);
    }
    const result = await client[operation]({ descriptor, accountClient, reference });
    if (registryRoot() !== root) throw new Error("Provider account scope changed during setup.");
    return result;
  }
  return {
    catalog,
    async setupCatalog(localPackages) {
      const activeRoot = registryRoot();
      const sourceRoot = sourceRegistryRoot();
      const packs = await Promise.all(localPackages.packs.map(async (pack) => {
        if (pack.workspace_setup === undefined) return pack;
        const id = pack.workspace_setup.home.provider_interface_id;
        const owners = pack.components.filter((component) => component.product_id === id);
        if (owners.length !== 1 || pack.setup_provider?.id !== id) {
          throw new Error(`Setup pack '${pack.id}' has no exact provider catalogue owner.`);
        }
        const selectedRoot = owners[0].materialized === true || owners[0].partially_materialized === true
          ? registryRoot : sourceRegistryRoot;
        const [provider] = await catalog([pack.setup_provider], selectedRoot);
        return { ...pack, setup_provider: provider };
      }));
      if (registryRoot() !== activeRoot || sourceRegistryRoot() !== sourceRoot) {
        throw new Error("Setup provider catalogue scope changed during discovery.");
      }
      return { ...localPackages, packs };
    },
    setupOwner: {
      probe: (plan) => accountOperation(plan, "probeAccount"),
      apply: (plan) => accountOperation(plan, "signIn"),
    },
    async models(request) {
      const id = request?.providerInterfaceId;
      const [{ models, modelCatalogError }] = await catalog([{ id }]);
      if (modelCatalogError) throw new Error(modelCatalogError);
      return { providerInterfaceId: id, models, observedAtUnixMs: Date.now() };
    },
  };
}

function descriptorModelCatalog(descriptor) {
  const config = descriptor.interface_driver_config?.private_config;
  if (!Array.isArray(config?.model_catalog)) {
    throw new Error(`Provider '${descriptor.id}' has no declared model catalogue.`);
  }
  return config.model_catalog.map(entry => ({
    model: entry?.model, displayName: entry?.display_name, description: entry?.description ?? "",
    defaultReasoningEffort: entry?.default_reasoning_effort ?? "",
    supportedReasoningEfforts: Array.isArray(entry?.supported_reasoning_efforts)
      ? entry.supported_reasoning_efforts.map(effort => typeof effort === "string" ? effort : effort?.effort) : undefined,
    showInPicker: entry?.show_in_picker, isDefault: entry?.model === config.model,
    ...modelServiceTiers(entry?.service_tiers, entry?.default_service_tier, `Provider model '${entry?.model}'`),
  }));
}

function validateModelCatalog(entries, providerId) {
  if (!Array.isArray(entries) || !entries.length) throw new Error(`Provider '${providerId}' has no model catalogue.`);
  const seen = new Set();
  return entries.map((entry) => {
    const efforts = entry?.supportedReasoningEfforts;
    if (typeof entry?.model !== "string" || !entry.model || entry.model.trim() !== entry.model || seen.has(entry.model)
      || typeof entry.displayName !== "string" || !entry.displayName.trim() || !Array.isArray(efforts)
      || efforts.some((effort) => typeof effort !== "string" || !effort) || new Set(efforts).size !== efforts.length
      || typeof entry.defaultReasoningEffort !== "string"
      || (efforts.length ? !efforts.includes(entry.defaultReasoningEffort) : entry.defaultReasoningEffort !== "")
      || typeof entry.showInPicker !== "boolean") {
      throw new Error(`Provider '${providerId}' has an invalid or repeated model.`);
    }
    seen.add(entry.model);
    return { ...entry, ...modelServiceTiers(entry.serviceTiers, entry.defaultServiceTier, `Provider model '${entry.model}'`) };
  });
}

function validateProviderModelSelection(providers, request) {
  const provider = providers.find((row) => row.id === request.providerInterfaceId);
  if (provider?.modelCatalogError) throw new Error(provider.modelCatalogError);
  const model = provider?.models.find((row) => row.model === request.providerModel);
  if (!model || (!model.showInPicker && model.model !== provider.model)) {
    throw new Error("Choose an available model from the selected provider catalogue.");
  }
  if (typeof request.reasoningEffort !== "string"
    || (model.supportedReasoningEfforts.length
      ? !model.supportedReasoningEfforts.includes(request.reasoningEffort)
      : request.reasoningEffort !== "")) {
    throw new Error("The selected reasoning level is not supported by this model.");
  }
  const requestedTier = request.serviceTier === undefined || request.serviceTier === ""
    ? provider.serviceTier || model.defaultServiceTier || "default"
    : request.serviceTier;
  validateModelServiceTier(model, requestedTier);
}

module.exports = { createClientProviderMetadata, validateProviderModelSelection };
