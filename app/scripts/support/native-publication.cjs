const path = require("node:path");
const { execFileSync } = require("node:child_process");
const { createStoragePublisher } = require("../../../registry/interface/world_operator_chat/publication.cjs");

function nativePublication(binary = path.resolve(__dirname, "../../../target/debug/octopus-operator-chat-topology")) {
  return createStoragePublisher(binary, (executable, mode, request, maxBuffer) => JSON.parse(execFileSync(executable, [mode], {
    input: JSON.stringify(request), encoding: "utf8", timeout: 30000, maxBuffer,
  })));
}

const transactionEntries = async directory => (await require("node:fs/promises").readdir(directory)).filter(name => name !== ".octopus-publication");
module.exports = { nativePublication, transactionEntries };
