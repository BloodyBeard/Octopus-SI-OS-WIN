const path = require("node:path");

let binding;
function cloneFile(sourceDescriptor, directoryDescriptor, name) {
  if (process.platform !== "darwin") throw new Error("Client package cloning requires macOS with clonefile support");
  if (!binding) binding = require(path.join(__dirname, "..", "native", "build", "octopus-filesystem.node"));
  return binding.cloneFile(sourceDescriptor, directoryDescriptor, name);
}

function renameDirectoryExclusive(sourceDirectoryDescriptor, sourceName, destinationDirectoryDescriptor, destinationName) {
  if (process.platform !== "darwin") throw new Error("Client package directory moves require macOS with RENAME_EXCL support");
  if (!binding) binding = require(path.join(__dirname, "..", "native", "build", "octopus-filesystem.node"));
  return binding.renameDirectoryExclusive(sourceDirectoryDescriptor, sourceName, destinationDirectoryDescriptor, destinationName);
}

module.exports = { cloneFile, renameDirectoryExclusive };
