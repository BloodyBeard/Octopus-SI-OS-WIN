const fs = require("node:fs");
const path = require("node:path");

const electronRoot = path.join(__dirname, "..", "electron");

function projectionSourceFiles() {
  const directory = path.join(electronRoot, "projection");
  return [path.join(electronRoot, "projection.cjs"), ...fs.readdirSync(directory)
    .filter((name) => name.endsWith(".cjs"))
    .sort()
    .map((name) => path.join(directory, name))];
}

function projectionSource() {
  return projectionSourceFiles().map((file) => fs.readFileSync(file, "utf8")).join("\n");
}

module.exports = { projectionSource, projectionSourceFiles };
