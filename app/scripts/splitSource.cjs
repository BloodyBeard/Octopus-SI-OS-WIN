const fs = require("node:fs");
const path = require("node:path");

function splitSourceFiles(facade) {
  const directory = facade.replace(/\.cjs$/, "");
  return [facade, ...fs.readdirSync(directory)
    .filter((name) => name.endsWith(".cjs"))
    .sort()
    .map((name) => path.join(directory, name))];
}

function splitSource(facade) {
  return splitSourceFiles(facade).map((file) => fs.readFileSync(file, "utf8")).join("\n");
}

function readSource(file) {
  const directory = file.replace(/\.cjs$/, "");
  return directory !== file && fs.existsSync(directory) ? splitSource(file) : fs.readFileSync(file, "utf8");
}

const projectionFacade = path.join(__dirname, "..", "electron", "projection.cjs");
const operatorChatClientFacade = path.join(__dirname, "..", "..", "registry", "interface", "world_operator_chat", "client.cjs");

module.exports = {
  splitSource,
  splitSourceFiles,
  readSource,
  projectionSource: () => splitSource(projectionFacade),
  projectionSourceFiles: () => splitSourceFiles(projectionFacade),
  operatorChatClientSource: () => splitSource(operatorChatClientFacade),
};
