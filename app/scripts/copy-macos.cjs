const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

function copyTree(source, destination, options = {}) {
  const entries = [];
  function visit(from, to) {
    if (options.filter && !options.filter(from, to)) return;
    const stat = fs.lstatSync(from);
    if (stat.isSymbolicLink()) {
      const target = fs.readlinkSync(from);
      entries.push({ type: "symlink", destination: to, target: options.verbatimSymlinks ? target : path.resolve(path.dirname(from), target) });
    } else if (stat.isDirectory()) {
      if (!options.recursive) throw new Error(`Recursive copy is required for ${from}`);
      entries.push({ type: "directory", destination: to, mode: stat.mode & 0o777 });
      for (const name of fs.readdirSync(from)) visit(path.join(from, name), path.join(to, name));
    } else if (stat.isFile()) {
      entries.push({ type: "file", source: from, destination: to });
    } else {
      throw new Error(`Unsupported package entry: ${from}`);
    }
  }
  visit(path.resolve(source), path.resolve(destination));
  cloneEntries(entries);
}

function copyFile(source, destination) {
  cloneEntries([{ type: "file", source: path.resolve(source), destination: path.resolve(destination) }]);
}

function cloneFiles(pairs) {
  cloneEntries(pairs.map(({ source, destination }) => ({ type: "file", source: path.resolve(source), destination: path.resolve(destination) })));
}

function cloneEntries(entries) {
  if (process.platform !== "darwin") throw new Error("macOS packaging requires clonefile support");
  const planRoot = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-package-copy-"));
  try {
    const planPath = path.join(planRoot, "entries.json");
    fs.writeFileSync(planPath, JSON.stringify(entries), { encoding: "utf8", flag: "wx", mode: 0o600 });
    const result = spawnSync("/usr/bin/python3", [path.join(__dirname, "copy-macos.py"), planPath], {
      stdio: ["ignore", "pipe", "pipe"], encoding: "utf8", maxBuffer: 1024 * 1024,
    });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`APFS package copy failed: ${result.stderr.trim() || result.signal || result.status}`);
  } finally {
    fs.rmSync(planRoot, { recursive: true, force: true });
  }
}

module.exports = { copyTree, copyFile, cloneFiles };
