function createClientProjectionStore(profile, options) {
  if (profile === "lab") return require("./fixtureProjection.cjs").createFixtureProjectionStore(options);
  if (profile === "live") return require("./projection.cjs").createProjectionStore(options);
  throw new Error(`Unknown client access profile: ${profile}`);
}

async function readClientProviderInterfaceDescriptors(profile, registryRoot) {
  if (profile === "lab") return require("./fixtureProjection.cjs").fixtureProviderInterfaceDescriptors();
  if (profile === "live") return require("./projection.cjs").readProviderInterfaceDescriptors(registryRoot);
  throw new Error(`Unknown client access profile: ${profile}`);
}

module.exports = {
  createClientProjectionStore,
  readClientProviderInterfaceDescriptors,
};
