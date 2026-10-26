const fs = require("node:fs/promises");
const path = require("node:path");
const { modelServiceTiers, validateModelServiceTier } = require("./providerModelTiers.cjs");
const { readInterfaceEnvironmentDeclarations, readInterfacePrivateEnvironmentDocument } = require("./projection/privateConfiguration.cjs");
const { resolveAuthoredApplicationSetupFields } = require("./projection/sidecars.cjs");

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
    const credentials = descriptorAccountKind(descriptor) === "credentials";
    const expectedState = credentials ? "credentials_configured" : "account_readable";
    if (plan.provider.required_state !== undefined && plan.provider.required_state !== expectedState) {
      throw new Error(`Provider '${descriptor.id}' setup must verify '${expectedState}'.`);
    }
    if (credentials) {
      const result = await credentialOperation(root, descriptor, operation);
      if (registryRoot() !== root) throw new Error("Provider configuration scope changed during setup.");
      return result;
    }
    const client = await loadClientMetadata(root, descriptor);
    if (typeof client?.[operation] !== "function") {
      throw new Error(`Provider '${descriptor.id}' does not support desktop account setup.`);
    }
    const result = await client[operation]({ descriptor, accountClient, reference });
    if (registryRoot() !== root) throw new Error("Provider account scope changed during setup.");
    return result;
  }
  // Providers that connect with an API key, server URL and model name instead of a
  // desktop account are ready once every required setup field has a value.
  async function credentialOperation(root, descriptor, operation) {
    const status = await providerCredentialStatus(root, descriptor);
    if (status.missing.length === 0) {
      return operation === "probeAccount"
        ? [reference("provider_observation", descriptor.id, { source: "provider.configuration", configured: status.configured })]
        : undefined;
    }
    if (operation === "probeAccount") return null;
    throw new Error(`${status.name} needs ${status.missing.join(", ")} before Home can use it.`);
  }
  return {
    catalog,
    async providerAccountKind(id) {
      return setupAccountKind(registryRoot(), id);
    },
    async providerConfiguration(id) {
      const root = registryRoot();
      return providerCredentialStatus(root, await readProvider(root, id));
    },
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
        const choices = await Promise.all((pack.setup_provider_choices ?? []).map(async (choice) => {
          const owner = pack.components.find((component) => component.product_id === choice.id);
          const root = owner?.materialized === true || owner?.partially_materialized === true ? registryRoot : sourceRegistryRoot;
          const [discovered] = await catalog([choice], root);
          return { ...discovered, setupAccount: await setupAccountKind(root(), choice.id) };
        }));
        return {
          ...pack,
          setup_provider: { ...provider, setupAccount: await setupAccountKind(selectedRoot(), id) },
          setup_provider_choices: choices,
        };
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

// "chatgpt": a desktop account module signs in; "credentials": an API key, server URL
// or model name configures it; "unsupported": desktop setup cannot connect it.
function descriptorAccountKind(descriptor) {
  const config = descriptor.interface_driver_config?.private_config;
  if (config?.client_metadata !== undefined) return "chatgpt";
  return config?.opencodex !== undefined ? "credentials" : "unsupported";
}

async function setupAccountKind(root, id) {
  try {
    return descriptorAccountKind(await readProvider(root, id));
  } catch {
    return "unsupported";
  }
}

async function providerCredentialStatus(root, descriptor) {
  const directory = path.join(root, "interface", descriptor.id);
  let application = null;
  try {
    application = JSON.parse(await fs.readFile(path.join(directory, "application.json"), "utf8"));
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  const declarations = await readInterfaceEnvironmentDeclarations(descriptor, descriptor.id, directory);
  const authored = Array.isArray(application?.setup?.fields) ? application.setup.fields : [];
  const fields = authored.some((field) => field?.interface === descriptor.id)
    ? resolveAuthoredApplicationSetupFields(authored, descriptor.id, declarations, `provider '${descriptor.id}' setup`)
      .map(({ presentation, declaration }) => ({
        name: declaration.name,
        label: String(presentation.label ?? declaration.name),
        kind: ["text", "url", "secret"].includes(presentation.kind) ? presentation.kind : "secret",
        required: presentation.required === true || declaration.required === true,
      }))
    : declarations.map((declaration) => ({ name: declaration.name, label: declaration.name, kind: "secret", required: declaration.required === true }));
  const document = await readInterfacePrivateEnvironmentDocument(
    path.join(path.dirname(root), "interfaces", descriptor.id),
    descriptor.id,
    new Set(declarations.map((declaration) => declaration.name)),
  );
  const rows = fields.map((field) => ({
    ...field,
    configured: document.values.has(field.name) || Boolean(process.env[field.name]),
  }));
  return {
    interfaceId: descriptor.id,
    name: typeof application?.name === "string" ? application.name : descriptor.id,
    documentDigest: document.digest,
    fields: rows,
    configured: rows.filter((field) => field.configured).map((field) => field.name).sort(),
    missing: rows.filter((field) => field.required && !field.configured).map((field) => field.label),
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
