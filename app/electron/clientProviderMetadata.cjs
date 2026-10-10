const fs = require("node:fs/promises");
const path = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");

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
  return {
    setupOwner: {
      async probe(plan) {
        const id = plan.provider.interface_id;
        await readCodexProvider(registryRoot(), id);
        const account = await accountClient().account();
        return account.signedIn ? [reference("provider_observation", id, {
          source: "codex.account/read", signed_in: true,
        })] : null;
      },
      async apply(plan) {
        await readCodexProvider(registryRoot(), plan.provider.interface_id);
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
      const descriptor = await readCodexProvider(registryRoot(), id);
      const allowed = descriptor.interface_policy?.variables?.models?.default;
      if (!Array.isArray(allowed) || !allowed.every((value) => typeof value === "string")) {
        throw new Error(`Provider '${id}' has no declared model list.`);
      }
      const models = (await accountClient().models()).filter((model) => allowed.includes("*") || allowed.includes(model.model));
      if (!models.length) throw new Error(`Provider '${id}' returned no available declared models.`);
      return { providerInterfaceId: id, models, observedAtUnixMs: Date.now() };
    },
  };
}

module.exports = { createClientProviderMetadata };
