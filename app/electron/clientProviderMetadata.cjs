const fs = require("node:fs/promises");
const path = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");
const { modelServiceTiers, validateModelServiceTier } = require("./providerModelTiers.cjs");

// Desktop account/catalog operations do not invoke a model or require a Home run.
async function readCodexProvider(registryRoot, id) {
  if (typeof id !== "string" || !/^[A-Za-z0-9_-]+$/.test(id)) {
    throw new Error("Invalid provider interface id.");
  }
  const descriptor = JSON.parse(await fs.readFile(path.join(registryRoot, "interface", id, "interface.json"), "utf8"));
  if (descriptor.id !== id || descriptor.interface_type !== "world" || descriptor.role !== "provider"
    || descriptor.interface_driver_ref !== "codex_app_server") {
    throw new Error(`Provider '${id}' does not support the desktop Codex account/catalog API.`);
  }
  return descriptor;
}

function createClientProviderMetadata({ registryRoot, accountClient, reference }) {
  async function catalog(providers) {
    const root = registryRoot();
    let nativeModels;
    const result = await Promise.all(providers.map(async (provider) => {
      if (provider.runtimeHandlerRef !== "codex_app_server") return provider;
      try {
        const descriptor = await readCodexProvider(root, provider.id);
        const local = descriptorModelCatalog(descriptor);
        const discovered = local ?? await (nativeModels ??= accountClient().models());
        if (JSON.stringify(await readCodexProvider(root, descriptor.id)) !== JSON.stringify(descriptor)) {
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
    if (registryRoot() !== root) throw new Error("Provider catalogue scope changed during discovery.");
    return result;
  }
  return {
    catalog,
    setupOwner: {
      async probe(plan) {
        const id = plan.provider.interface_id;
        requireNativeAccount(await readCodexProvider(registryRoot(), id));
        const account = await accountClient().account();
        return account.signedIn ? [reference("provider_observation", id, {
          source: "codex.account/read", signed_in: true,
        })] : null;
      },
      async apply(plan) {
        requireNativeAccount(await readCodexProvider(registryRoot(), plan.provider.interface_id));
        const client = accountClient();
        if ((await client.account()).signedIn) return;
        const { loginId } = await client.start();
        const deadline = Date.now() + 300_000;
        for (;;) {
          const status = client.status(loginId);
          if (status.state === "completed") return;
          if (status.state === "failed") throw new Error(status.error || "ChatGPT sign-in failed.");
          if (Date.now() >= deadline) {
            await client.cancel(loginId);
            throw new Error("ChatGPT sign-in was not completed in time.");
          }
          await delay(500);
        }
      },
    },
    async models(request) {
      const id = request?.providerInterfaceId;
      const [{ models, modelCatalogError }] = await catalog([{ id, runtimeHandlerRef: "codex_app_server" }]);
      if (modelCatalogError) throw new Error(modelCatalogError);
      return { providerInterfaceId: id, models, observedAtUnixMs: Date.now() };
    },
  };
}

function requireNativeAccount(descriptor) {
  const config = descriptor.interface_driver_config;
  const auth = config?.residual_ambient?.private_auth_store;
  if (Object.hasOwn(config?.private_config ?? {}, "opencodex")
    || config?.transport != null || !auth || Object.keys(auth).length !== 1
    || auth.auth_file_env !== "OCTOPUS_MACHINE_CODEX_AUTH_FILE") {
    throw new Error(`Provider '${descriptor.id}' does not use the desktop Codex account.`);
  }
}

function descriptorModelCatalog(descriptor) {
  const config = descriptor.interface_driver_config;
  if (!Object.hasOwn(config?.private_config ?? {}, "opencodex")) {
    requireNativeAccount(descriptor);
    return null;
  }
  const custom = config.private_config.opencodex;
  const entries = custom?.model_catalog?.models;
  if (custom?.schema_version !== 1 || !config.transport
    || !["anthropic", "azure", "google", "openai-chat", "openai-responses"].includes(custom.adapter)
    || typeof custom.credential_owner_interface !== "string" || !/^[A-Za-z0-9_-]+$/.test(custom.credential_owner_interface)
    || !Array.isArray(entries) || !entries.length) {
    throw new Error(`Provider '${descriptor.id}' has no valid OpenCodex model catalogue.`);
  }
  const seen = new Set();
  return entries.flatMap((entry) => {
    const efforts = entry.supported_reasoning_levels?.map((row) => row?.effort);
    if (typeof entry.slug !== "string" || !entry.slug || entry.slug.trim() !== entry.slug || seen.has(entry.slug)
      || typeof entry.display_name !== "string" || !Array.isArray(efforts)
      || efforts.some((effort) => typeof effort !== "string" || !effort) || new Set(efforts).size !== efforts.length
      || (efforts.length ? !efforts.includes(entry.default_reasoning_level) : Boolean(entry.default_reasoning_level))
      || !["list", "hide", "none"].includes(entry.visibility) || typeof entry.supported_in_api !== "boolean") {
      throw new Error(`Provider '${descriptor.id}' has an invalid or repeated OpenCodex model.`);
    }
    seen.add(entry.slug);
    if (!entry.supported_in_api) return [];
    return [{
      model: entry.slug, displayName: entry.display_name, description: entry.description ?? "",
      defaultReasoningEffort: entry.default_reasoning_level ?? "", supportedReasoningEfforts: efforts,
      showInPicker: entry.visibility === "list", isDefault: entry.slug === config.private_config.model,
      ...modelServiceTiers(entry.service_tiers, entry.default_service_tier, `OpenCodex model '${entry.slug}'`),
    }];
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
