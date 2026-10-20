function createClientProjectionStore(profile, options) {
  if (profile === "lab") return require("./fixtureProjection.cjs").createFixtureProjectionStore(options);
  if (profile === "live") return require("./projection.cjs").createProjectionStore(options);
  throw new Error(`Unknown client access profile: ${profile}`);
}

module.exports = {
  createClientProjectionStore,
};
