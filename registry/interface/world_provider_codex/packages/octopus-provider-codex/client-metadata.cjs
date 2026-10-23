const { setTimeout: delay } = require("node:timers/promises");

function requireNativeAccount(descriptor) {
  const config = descriptor.interface_driver_config;
  const auth = config?.residual_ambient?.private_auth_store;
  if (descriptor.interface_driver_ref !== "codex_app_server"
    || Object.hasOwn(config?.private_config ?? {}, "opencodex")
    || config?.transport != null || !auth || Object.keys(auth).length !== 1
    || auth.auth_file_env !== "OCTOPUS_MACHINE_CODEX_AUTH_FILE") {
    throw new Error(`Provider '${descriptor.id}' does not use the desktop Codex account.`);
  }
}

module.exports = {
  schemaVersion: 1,
  async modelCatalog({ descriptor, accountClient, cache }) {
    requireNativeAccount(descriptor);
    const client = accountClient();
    if (!cache.has(client)) cache.set(client, client.models());
    return cache.get(client);
  },
  async probeAccount({ descriptor, accountClient, reference }) {
    requireNativeAccount(descriptor);
    const account = await accountClient().account();
    return account.signedIn ? [reference("provider_observation", descriptor.id, {
      source: "codex.account/read", signed_in: true,
    })] : null;
  },
  async signIn({ descriptor, accountClient }) {
    requireNativeAccount(descriptor);
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
};
