const { execFileSync } = require("node:child_process");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { readGeneratedSourcePaths, isGeneratedSourcePath } = require("./generated-source-boundary.cjs");

const {
  errorMessage,
  publicationPrivateRoots,
  readJsonRegular,
  validatePublicPayload,
} = require("./publication-lib.cjs");

const repoRoot = path.resolve(__dirname, "../..");
const generatedOutputs = readGeneratedSourcePaths(repoRoot);
const sourceRoots = [
  path.join(repoRoot, "registry"),
  path.join(repoRoot, "app", "distribution"),
];
const excludedSourceSegments = new Set([".DS_Store", ".git", "__pycache__", "node_modules", "target"]);
const requiredRepositoryFiles = [
  ".gitignore",
  "README.md",
  "CONTRIBUTING.md",
  "AGENTS.md",
  "docs/README.md",
  "docs/architecture.md",
  "docs/development.md",
  "docs/getting-started.md",
  "LICENSE",
  "registry/.octopus-source.json",
  "app/distribution/generated-outputs.json",
  "app/schemas/package_publication.schema.json",
  "app/schemas/client_extension_publication.schema.json",
  "app/scripts/check-public-source.cjs",
  "app/scripts/generated-source-boundary.cjs",
  "app/scripts/publication-lib.cjs",
  "app/scripts/export-public-package.cjs",
  "app/scripts/export-public-client-extension.cjs",
  "app/scripts/verify-publication.cjs",
];
const selectionControlFiles = new Set([
  ".gitignore",
  "app/distribution/generated-outputs.json",
  "app/scripts/check-public-source.cjs",
  "app/scripts/generated-source-boundary.cjs",
  "app/scripts/publication-lib.cjs",
]);
const forbiddenTrackedRoots = new Set([
  ".private",
  ".agents",
  ".codex",
  ".claude",
  ".cursor",
  ".playwright-cli",
  "concepts",
  "lab",
  "lab2",
  "LAB",
  "output",
  "outputs",
  "screenshots",
  ".octopus",
  ".octopus-home",
  "client",
  "client-state",
  "interface-hand-in",
  "interfaces",
  "ledger",
  "logs",
  "octopus-home",
  "octopus_home",
  "state",
  "target",
  "node_modules",
  "build",
  "dist",
  ".build",
  ".swiftpm",
  ".cache",
  "tmp",
  "var",
]);
const requiredIgnoreEntries = [
  ".env.*",
  "!.env.example",
  ".claude/settings.local.json",
  "/.octopus/",
  "/.octopus-home/",
  "/octopus-home/",
  "/octopus-home-*/",
  "/octopus_home/",
  "/client/",
  "/client-state/",
  "/interface-hand-in/",
  "/interfaces/",
  "/ledger/",
  "/logs/",
  "/state/",
  "/host.json",
  "/client-access.json",
  "/client-lab-access.json",
  "/.octopus-source-manifest.json",
  "/publications/",
  "*.octopus-package",
  "*.octopus-client-extension",
];

async function main() {
  const worktreeMode = process.argv.slice(2).includes("--worktree");
  const unknown = process.argv.slice(2).filter((argument) => argument !== "--worktree");
  if (unknown.length > 0) throw new Error(`unknown arguments: ${unknown.join(", ")}`);
  const issues = [];
  requireRepositoryFiles(issues);
  validateIgnoreFile(issues);
  const inventory = worktreeMode ? null : trackedInventory();
  const tracked = inventory ? inventory.map((entry) => entry.relative) : currentBoundaryPaths();
  for (const relative of tracked) validateTrackedPath(relative, issues);
  const privateRoots = publicationPrivateRoots();
  if (inventory) validateSelectedFiles(inventory, issues, privateRoots);
  let sourceFiles = 0;
  for (const root of sourceRoots) sourceFiles += await scanSourceRoot(root, root, issues, privateRoots);
  await validateRegistryIdentity(issues);
  if (issues.length > 0) {
    const unique = [...new Set(issues)].sort();
    throw new Error(`public source boundary has ${unique.length} issue(s):\n${unique.map((issue) => `- ${issue}`).join("\n")}`);
  }
  process.stdout.write(`Public source boundary OK: ${tracked.length} ${worktreeMode ? "boundary" : "tracked"} paths, ${sourceFiles} package-source files\n`);
}

function requireRepositoryFiles(issues) {
  for (const relative of requiredRepositoryFiles) {
    const file = path.join(repoRoot, relative);
    if (!fsSync.existsSync(file) || !fsSync.statSync(file).isFile()) issues.push(`required public-source file is missing: ${relative}`);
  }
  const license = path.join(repoRoot, "LICENSE");
  if (fsSync.existsSync(license) && !fsSync.readFileSync(license, "utf8").startsWith("MIT License\n")) {
    issues.push("root LICENSE does not match the workspace MIT declaration");
  }
}

function validateIgnoreFile(issues) {
  const file = path.join(repoRoot, ".gitignore");
  const lines = new Set(fsSync.readFileSync(file, "utf8").split(/\r?\n/).map((line) => line.trim()));
  for (const required of requiredIgnoreEntries) {
    if (!lines.has(required)) issues.push(`.gitignore is missing '${required}'`);
  }
  for (const output of generatedOutputs) {
    if (!lines.has(`/${output}`)) issues.push(`.gitignore is missing declared generated output '/${output}'`);
  }
}

function trackedInventory() {
  try {
    const body = execFileSync("git", ["-C", repoRoot, "ls-files", "--stage", "-z"], {
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "pipe"],
    });
    return body.split("\0").filter(Boolean).map((row) => {
      const match = /^(\d{6}) ([a-f0-9]{40,64}) ([0-3])\t(.+)$/.exec(row);
      if (!match) throw new Error("malformed Git index entry");
      return { mode: match[1], object: match[2], stage: Number(match[3]), relative: match[4] };
    }).sort((left, right) => left.relative.localeCompare(right.relative));
  } catch (error) {
    throw new Error(`git tracked-file inventory is unavailable; repair the worktree or use --worktree for a local source scan: ${errorMessage(error)}`);
  }
}

function validateSelectedFiles(inventory, issues, privateRoots) {
  const selected = new Set(inventory.filter((entry) => entry.stage === 0).map((entry) => entry.relative));
  for (const relative of requiredRepositoryFiles) {
    if (!selected.has(relative)) issues.push(`required public-source file is absent from Git index: ${relative}`);
  }
  const files = [];
  for (const entry of inventory) {
    if (entry.stage !== 0) issues.push(`Git index contains an unresolved conflict: ${entry.relative}`);
    else if (!["100644", "100755"].includes(entry.mode)) issues.push(`Git index contains a non-regular source entry (${entry.mode}): ${entry.relative}`);
    else files.push(entry);
  }
  if (files.length === 0) return;
  const body = execFileSync("git", ["-C", repoRoot, "cat-file", "--batch"], {
    input: files.map((entry) => `${entry.object}\n`).join(""),
    maxBuffer: 128 * 1024 * 1024,
    stdio: ["pipe", "pipe", "pipe"],
  });
  let offset = 0;
  for (const entry of files) {
    const end = body.indexOf(10, offset);
    if (end < 0) throw new Error("Git selected-blob inventory is truncated");
    const header = /^(\w+) blob (\d+)$/.exec(body.subarray(offset, end).toString("ascii"));
    if (!header || header[1] !== entry.object) throw new Error(`Git selected object is not the declared blob: ${entry.relative}`);
    const size = Number(header[2]);
    offset = end + 1;
    if (!Number.isSafeInteger(size) || offset + size >= body.length || body[offset + size] !== 10) {
      throw new Error(`Git selected blob is incomplete: ${entry.relative}`);
    }
    const payload = body.subarray(offset, offset + size);
    offset += size + 1;
    try {
      validatePublicPayload(entry.relative, payload, { privateRoots });
      if (entry.relative.endsWith(".json")) {
        try { JSON.parse(payload.toString("utf8")); }
        catch { throw new Error("selected JSON is malformed"); }
      }
      if (selectionControlFiles.has(entry.relative) && !payload.equals(fsSync.readFileSync(path.join(repoRoot, entry.relative)))) {
        throw new Error("selected publication control differs from the checked worktree");
      }
      if (entry.relative === "LICENSE" && !payload.toString("utf8").startsWith("MIT License\n")) {
        throw new Error("selected LICENSE does not match the workspace MIT declaration");
      }
    } catch (error) {
      issues.push(`selected ${entry.relative}: ${errorMessage(error)}`);
    }
  }
  if (offset !== body.length) throw new Error("Git selected-blob inventory contains unexpected trailing data");
}

function currentBoundaryPaths() {
  const candidates = [
    ".claude/settings.local.json",
    ".env",
    ".octopus-source-manifest.json",
    "client-access.json",
    "client-lab-access.json",
    "host.json",
  ];
  return candidates.filter((relative) => fsSync.existsSync(path.join(repoRoot, relative))).sort();
}

function validateTrackedPath(relativeValue, issues) {
  const relative = relativeValue.split(path.sep).join("/");
  if (isGeneratedSourcePath(relative, generatedOutputs)) issues.push(`tracked generated build output: ${relative}`);
  if (relative === ".claude/settings.local.json") issues.push("tracked editor-local policy: .claude/settings.local.json");
  const first = relative.split("/")[0];
  if (forbiddenTrackedRoots.has(first) || first.startsWith("octopus-home-")) {
    issues.push(`tracked private or local-only root: ${relative}`);
  }
  if ([
    ".env",
    ".octopus-source-manifest.json",
    "client-access.json",
    "client-lab-access.json",
    "host.json",
  ].includes(relative)) issues.push(`tracked private root file: ${relative}`);
  if (relative === ".gitmodules" || /^docs\/(references|ai|archive2)\//.test(relative)
    || ["docs/octopus_architecture.md", "docs/octopus_os_distribution_and_welcome_plan.md"].includes(relative)
    || /^(logs.*\.md|notes\.md|goal\.md|design\.md|CLAUDE\.md)$/i.test(relative)) {
    issues.push(`tracked private authoring or reference material: ${relative}`);
  }
  if (relative.split("/").some((part) => ["node_modules", "target", ".build", ".swiftpm", ".cache"].includes(part))
    || /^app\/(build|dist)(?:\/|$)/.test(relative)) issues.push(`tracked dependency or build output: ${relative}`);
  const basename = path.posix.basename(relative).toLowerCase();
  if (basename === ".env" || basename.startsWith(".env.") && !basename.endsWith(".example")) {
    issues.push(`tracked environment file: ${relative}`);
  }
}

async function scanSourceRoot(root, directory, issues, privateRoots) {
  let count = 0;
  const entries = (await fs.readdir(directory, { withFileTypes: true })).sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of entries) {
    if (excludedSourceSegments.has(entry.name)) continue;
    const candidate = path.join(directory, entry.name);
    const relative = path.relative(repoRoot, candidate).split(path.sep).join("/");
    if (isGeneratedSourcePath(relative, generatedOutputs)) continue;
    const stat = await fs.lstat(candidate);
    if (stat.isSymbolicLink()) {
      issues.push(`package source contains a symbolic link: ${relative}`);
      continue;
    }
    if (stat.isDirectory()) {
      count += await scanSourceRoot(root, candidate, issues, privateRoots);
      continue;
    }
    if (!stat.isFile()) {
      issues.push(`package source contains a non-regular entry: ${relative}`);
      continue;
    }
    count += 1;
    const body = await fs.readFile(candidate);
    if (body.byteLength !== stat.size) {
      issues.push(`package source changed while reading: ${relative}`);
      continue;
    }
    try {
      validatePublicPayload(relative, body, { privateRoots });
      if (entry.name.endsWith(".json")) JSON.parse(body.toString("utf8"));
    } catch (error) {
      issues.push(`${relative}: ${errorMessage(error)}`);
    }
  }
  return count;
}

async function validateRegistryIdentity(issues) {
  const file = path.join(repoRoot, "registry", ".octopus-source.json");
  try {
    const { value } = await readJsonRegular(file, "source registry identity", 64 * 1024);
    if (
      value.schema_version !== 1
      || typeof value.id !== "string"
      || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(value.id)
    ) issues.push("registry/.octopus-source.json has an invalid source identity");
  } catch (error) {
    issues.push(errorMessage(error));
  }
  if (fsSync.existsSync(path.join(repoRoot, "registry", ".octopus-source-manifest.json"))) {
    issues.push("source registry contains an Active Registry install manifest");
  }
}

main().catch((error) => {
  process.stderr.write(`${errorMessage(error)}\n`);
  process.exitCode = 1;
});
