const path = require("node:path");

function createPackageStoragePublisher(sourceRegistryRoot, installationRoot) {
  let loaded;
  const publisher = () => loaded ??= Promise.resolve().then(async () => {
    const owner = require(path.join(sourceRegistryRoot, "interface", "world_operator_chat", "client.cjs"));
    if (typeof owner.loadStoragePublisher !== "function") throw new Error("The distribution client has no native storage publisher");
    return owner.loadStoragePublisher({ registryRoot: () => sourceRegistryRoot, installationRoot: () => installationRoot });
  });
  return Object.freeze(Object.fromEntries(["publishFiles", "snapshotDirectory", "publishDirectoryMoves", "reclaimDirectory"].map(method =>
    [method, async (...args) => (await publisher())[method](...args)])));
}

module.exports = { createPackageStoragePublisher };
