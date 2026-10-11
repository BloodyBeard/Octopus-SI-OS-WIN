const path = require("node:path");

let binding;
function cloneFile(sourceDescriptor, directoryDescriptor, name) {
  if (process.platform !== "darwin") throw new Error("Client package cloning requires macOS with clonefile support");
  if (!binding) binding = require(path.join(__dirname, "..", "native", "build", "octopus-filesystem.node"));
  return binding.cloneFile(sourceDescriptor, directoryDescriptor, name);
}

module.exports = { cloneFile };
