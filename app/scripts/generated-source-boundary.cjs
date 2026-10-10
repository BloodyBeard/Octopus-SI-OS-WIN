const fs = require("node:fs");
const path = require("node:path");

function readGeneratedSourcePaths(repoRoot) {
  const file = path.join(repoRoot, "app", "distribution", "generated-outputs.json");
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 65536) throw new Error("generated source output declaration must be a bounded regular file");
  const declaration = JSON.parse(fs.readFileSync(file, "utf8"));
  if (declaration?.schema_version !== 1 || Object.keys(declaration).sort().join("|") !== "paths|schema_version"
    || !Array.isArray(declaration.paths) || !declaration.paths.length || declaration.paths.length > 128) throw new Error("generated source output declaration is malformed");
  const paths = new Set();
  for (const entry of declaration.paths) {
    if (typeof entry !== "string" || !entry.startsWith("registry/") || entry.includes("\\")
      || entry.replace(/\/$/, "").split("/").some((part) => !part || part === "." || part === ".." || part.includes("\0"))
      || paths.has(entry)) throw new Error(`invalid generated source output '${entry}'`);
    paths.add(entry);
  }
  return [...paths];
}

function isGeneratedSourcePath(relative, outputs) {
  const normalized = path.posix.normalize(relative.split(path.sep).join("/"));
  return outputs.some((output) => normalized === output.replace(/\/$/, "") || output.endsWith("/") && normalized.startsWith(output));
}

function assertDeclaredGeneratedOutput(file, repoRoot, outputs) {
  const relative = path.relative(repoRoot, file).split(path.sep).join("/");
  if (!isGeneratedSourcePath(relative, outputs)) throw new Error(`build output is not declared as generated source data: ${relative}`);
}

module.exports = { assertDeclaredGeneratedOutput, isGeneratedSourcePath, readGeneratedSourcePaths };
