const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { pipeline } = require("node:stream/promises");
const { Readable, Transform } = require("node:stream");

const RECEIPT = "codex-cli-receipt.json";
const COMPANION = "codex-code-mode-host";
const PENDING = ".codex-cli-pending.json";
const LOCK = ".codex-cli-install.lock";
const DOWNLOAD_TIMEOUT_MS = 10 * 60 * 1000;
const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const SHA256 = /^[0-9a-f]{64}$/;
const SCHEMA_NAME = /^(v2\/)?[A-Za-z][A-Za-z0-9]*\.json$/;
const PLATFORM_ENTRIES = {
  "darwin-arm64": "codex-aarch64-apple-darwin",
  "darwin-x64": "codex-x86_64-apple-darwin",
  "win32-x64": "codex-x86_64-pc-windows-msvc.exe",
};
// The Windows release runs its sandbox through these helpers, which must sit next to codex.exe.
const WINDOWS_SANDBOX_COMPANIONS = ["codex-windows-sandbox-setup", "codex-command-runner"];

function windowsPlatform(platform) { return platform.startsWith("win32-"); }
function companionNames(windows) { return windows ? [COMPANION, ...WINDOWS_SANDBOX_COMPANIONS] : [COMPANION]; }
function executableFile(name, windows) { return windows ? `${name}.exe` : name; }

function record(value) { return value !== null && typeof value === "object" && !Array.isArray(value); }

function fields(value, required, optional = []) {
  return record(value) && required.every(key => Object.hasOwn(value, key))
    && Object.keys(value).every(key => required.includes(key) || optional.includes(key));
}

function legalHashes(value) {
  return fields(value, ["LICENSE"], ["NOTICE"]) && Object.values(value).every(hash => typeof hash === "string" && SHA256.test(hash));
}

function legalFile(file, expected) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.size <= 0 || stat.size > 65536 || digest(file) !== expected) {
    throw new Error(`Codex upstream attribution differs from its pinned bytes: ${file}`);
  }
}

function digest(file) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile()) throw new Error(`Codex distribution input is not a regular file: ${file}`);
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  const hash = crypto.createHash("sha256"), buffer = Buffer.alloc(1024 * 1024);
  try {
    for (;;) { const count = fs.readSync(fd, buffer); if (!count) break; hash.update(buffer.subarray(0, count)); }
  } finally { fs.closeSync(fd); }
  return hash.digest("hex");
}

function readJson(file) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.size > 65536) throw new Error(`Invalid Codex distribution metadata: ${file}`);
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function releaseManifest(file = path.join(__dirname, "release.json")) {
  const manifest = readJson(file);
  if (!fields(manifest, ["schema_version", "version", "tag", "release_url", "platforms", "schemas", "legal"], ["source_tag_object"])
      || manifest.schema_version !== 2 || typeof manifest.version !== "string" || !VERSION.test(manifest.version)
      || manifest.tag !== `rust-v${manifest.version}`
      || manifest.release_url !== `https://github.com/openai/codex/releases/tag/${manifest.tag}`
      || !record(manifest.platforms) || !Object.keys(manifest.platforms).length
      || !record(manifest.schemas) || !Object.keys(manifest.schemas).length
      || !fields(manifest.legal, ["LICENSE"], ["NOTICE"])
      || (manifest.source_tag_object !== undefined && !/^[0-9a-f]{40}$/.test(manifest.source_tag_object))) {
    throw new Error("Invalid pinned Codex release manifest");
  }
  for (const [name, expected] of Object.entries(manifest.schemas)) {
    if (!SCHEMA_NAME.test(name) || typeof expected !== "string" || !SHA256.test(expected)) {
      throw new Error("Invalid pinned Codex schema digest");
    }
  }
  for (const [platform, asset] of Object.entries(manifest.platforms)) {
    const entry = PLATFORM_ENTRIES[platform];
    if (!entry || !fields(asset, ["archive_entry", "archive_bytes", "archive_sha256", "url", "companions"]) || asset.archive_entry !== entry
        || asset.url !== `https://github.com/openai/codex/releases/download/${manifest.tag}/${asset.archive_entry}.tar.gz`
        || !SHA256.test(asset.archive_sha256) || !Number.isSafeInteger(asset.archive_bytes)
        || asset.archive_bytes <= 0 || asset.archive_bytes > 256 * 1024 * 1024) {
      throw new Error(`Invalid pinned Codex asset for ${platform}`);
    }
  }
  for (const [platform, asset] of Object.entries(manifest.platforms)) {
    const names = companionNames(windowsPlatform(platform));
    if (!fields(asset.companions, names)) throw new Error(`Missing pinned Codex companion for ${platform}`);
    for (const name of names) {
      const helper = asset.companions[name], entry = asset.archive_entry.replace("codex-", `${name}-`);
      if (!fields(helper, ["archive_entry", "archive_bytes", "archive_sha256", "url"]) || helper.archive_entry !== entry
          || helper.url !== `https://github.com/openai/codex/releases/download/${manifest.tag}/${entry}.tar.gz`
          || !SHA256.test(helper.archive_sha256) || !Number.isSafeInteger(helper.archive_bytes)
          || helper.archive_bytes <= 0 || helper.archive_bytes > 256 * 1024 * 1024) throw new Error(`Invalid pinned Codex companion for ${platform}`);
    }
  }
  for (const [name, evidence] of Object.entries(manifest.legal)) {
    if (!fields(evidence, ["source_url", "sha256"])
        || evidence.source_url !== `https://raw.githubusercontent.com/openai/codex/${manifest.tag}/${name}`
        || typeof evidence.sha256 !== "string" || !SHA256.test(evidence.sha256)) {
      throw new Error(`Invalid pinned Codex attribution for ${name}`);
    }
    legalFile(path.join(path.dirname(file), "upstream", name), evidence.sha256);
  }
  return manifest;
}

function atomicJson(file, value) {
  const temporary = `${file}.${crypto.randomUUID()}.tmp`;
  try {
    const fd = fs.openSync(temporary, "wx", 0o644);
    try { fs.writeFileSync(fd, `${JSON.stringify(value, null, 2)}\n`); fs.fsyncSync(fd); }
    finally { fs.closeSync(fd); }
    fs.renameSync(temporary, file);
  }
  finally { if (fs.existsSync(temporary)) fs.unlinkSync(temporary); }
  const directory = fs.openSync(path.dirname(file), "r");
  try { fs.fsyncSync(directory); } finally { fs.closeSync(directory); }
}

function syncFile(file) {
  const fd = fs.openSync(file, "r");
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}

function versionOrder(left, right) {
  if (!VERSION.test(left) || !VERSION.test(right)) throw new Error("Invalid managed Codex version");
  const a = left.split(".").map(BigInt), b = right.split(".").map(BigInt);
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  return 0;
}

function sameSchemas(left, right) {
  if (!left || !right || typeof left !== "object" || typeof right !== "object") return false;
  const names = Object.keys(left).sort(), expected = Object.keys(right).sort();
  return names.length === expected.length && names.every((name, index) => name === expected[index] && left[name] === right[name]);
}

function validateReceipt(receipt) {
  if (!fields(receipt, ["schema_version", "version", "release_url", "archive_sha256", "executable", "executable_sha256", "executable_bytes", "schema_baseline", "schemas", "legal", "signing", "companions"])
      || receipt.schema_version !== 2 || typeof receipt.version !== "string" || !VERSION.test(receipt.version)
      || !["codex", "codex.exe"].includes(receipt.executable)
      || receipt.schema_baseline !== `rust-v${receipt.version}`
      || receipt.release_url !== `https://github.com/openai/codex/releases/tag/rust-v${receipt.version}`
      || !SHA256.test(receipt.archive_sha256) || !SHA256.test(receipt.executable_sha256)
      || !Number.isSafeInteger(receipt.executable_bytes) || receipt.executable_bytes <= 0 || receipt.executable_bytes > 512 * 1024 * 1024
      || !record(receipt.schemas)
      || !Object.keys(receipt.schemas).length
      || Object.entries(receipt.schemas).some(([name, hash]) => !SCHEMA_NAME.test(name) || typeof hash !== "string" || !SHA256.test(hash))
      || !legalHashes(receipt.legal)
      || !fields(receipt.companions, companionNames(receipt.executable === "codex.exe"))
      || Object.entries(receipt.companions).some(([name, value]) => !validCompanion(value, executableFile(name, receipt.executable === "codex.exe")))
      || (receipt.signing !== null && (!fields(receipt.signing, ["kind", "before_sha256"])
          || receipt.signing.kind !== "ad-hoc" || !SHA256.test(receipt.signing.before_sha256)))) {
    throw new Error("Invalid managed Codex distribution receipt");
  }
}

function validCompanion(value, file) {
  return fields(value, ["archive_sha256", "executable", "executable_sha256", "executable_bytes", "signing"])
    && value.executable === file && SHA256.test(value.archive_sha256) && SHA256.test(value.executable_sha256)
    && Number.isSafeInteger(value.executable_bytes) && value.executable_bytes > 0 && value.executable_bytes <= 512 * 1024 * 1024
    && (value.signing === null || (fields(value.signing, ["kind", "before_sha256"])
      && value.signing.kind === "ad-hoc" && SHA256.test(value.signing.before_sha256)));
}

function executableReceipts(receipt) { return [receipt, ...Object.values(receipt.companions)]; }

function verifiedBinary(file, receipt) {
  const stat = fs.lstatSync(file);
  // Windows has no execute bit; an .exe is executable by name.
  const executable = file.endsWith(".exe") || !!(stat.mode & 0o111);
  return stat.isFile() && stat.size === receipt.executable_bytes && executable && digest(file) === receipt.executable_sha256;
}

function verifyInstallation(root) {
  const receipt = readJson(path.join(root, RECEIPT));
  validateReceipt(receipt);
  for (const executable of executableReceipts(receipt)) {
    if (!verifiedBinary(path.join(root, executable.executable), executable)) {
      throw new Error(`Managed Codex installation differs from its receipt: ${executable.executable} in ${root}`);
    }
  }
  for (const [name, hash] of Object.entries(receipt.legal)) legalFile(path.join(root, name), hash);
  return receipt;
}

function acquireLock(file, version) {
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      const fd = fs.openSync(file, "wx", 0o600);
      try { fs.writeFileSync(fd, JSON.stringify({ schema_version: 1, pid: process.pid, version })); fs.fsyncSync(fd); }
      catch (error) { fs.closeSync(fd); fs.unlinkSync(file); throw error; }
      return fd;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      const before = fs.lstatSync(file), owner = readJson(file);
      if (!fields(owner, ["schema_version", "pid", "version"]) || owner.schema_version !== 1
          || !Number.isSafeInteger(owner.pid) || owner.pid <= 0 || owner.pid > 2147483647
          || typeof owner.version !== "string" || !VERSION.test(owner.version)) throw new Error("Managed Codex installation lock has invalid owner evidence");
      try { process.kill(owner.pid, 0); throw new Error("Managed Codex installation is already in progress"); }
      catch (error) { if (error.code !== "ESRCH") throw error; }
      const after = fs.lstatSync(file);
      if (before.dev !== after.dev || before.ino !== after.ino || before.mtimeMs !== after.mtimeMs || before.size !== after.size) {
        throw new Error("Managed Codex installation lock changed during recovery");
      }
      fs.unlinkSync(file);
    }
  }
  throw new Error("Managed Codex installation lock acquisition raced");
}

async function downloadArchive(url, destination, expectedBytes) {
  const response = await fetch(url, { signal: AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS), redirect: "follow" });
  const final = new URL(response.url);
  if (!response.ok || final.protocol !== "https:"
      || !["github.com", "release-assets.githubusercontent.com"].includes(final.hostname)) {
    throw new Error(`Official Codex download failed (${response.status})`);
  }
  let received = 0;
  const bound = new Transform({ transform(chunk, _, done) {
    received += chunk.length;
    if (received > expectedBytes) done(new Error("Official Codex archive exceeds its declared size"));
    else done(null, chunk);
  } });
  await pipeline(Readable.fromWeb(response.body), bound, fs.createWriteStream(destination, { flags: "wx", mode: 0o600 }));
  if (received !== expectedBytes) throw new Error("Official Codex archive is truncated");
}

// On Windows the first tar on PATH is often GNU tar from Git or MSYS, which
// reads "D:\\stage\\x.tar.gz" as host "D" plus a remote path ("Cannot connect
// to D"). Windows' own bsdtar (System32, Windows 10 1803+) takes drive paths.
function tarProgram() {
  if (process.platform !== "win32") return "tar";
  const systemTar = path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe");
  return fs.existsSync(systemTar) ? systemTar : "tar";
}

function execute(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: "utf8", timeout: 30000, maxBuffer: 4 * 1024 * 1024, ...options });
  if (result.error || result.status !== 0) throw new Error(`Codex distribution command failed: ${command}: ${result.error?.message || result.stderr || result.status}`);
  return result.stdout.trim();
}

function verifySchema(binary, manifest, stage) {
  const home = path.join(stage, "probe-home"), schemas = path.join(stage, "schema");
  fs.mkdirSync(home);
  const env = { PATH: process.env.PATH || "/usr/bin:/bin", HOME: home, CODEX_HOME: home, TMPDIR: home,
    ...(process.platform === "win32" ? { SystemRoot: process.env.SystemRoot, USERPROFILE: home, TEMP: home, TMP: home } : {}) };
  if (execute(binary, ["--version"], { env, cwd: home }) !== `codex-cli ${manifest.version}`) {
    throw new Error("Official Codex executable reports an unexpected version");
  }
  execute(binary, ["app-server", "generate-json-schema", "--experimental", "--out", schemas], { env, cwd: home });
  for (const [name, expected] of Object.entries(manifest.schemas)) {
    if (digest(path.join(schemas, name)) !== expected) throw new Error(`Official Codex protocol differs from validated schema: ${name}`);
  }
}

function recoverPending(root, manifest, asset) {
  const pendingFile = path.join(root, PENDING);
  if (!fs.existsSync(pendingFile)) return;
  const pending = readJson(pendingFile);
  if (!fields(pending, ["candidate", "receipt"]) || pending.receipt?.version !== manifest.version
      || pending.receipt?.archive_sha256 !== asset.archive_sha256 || pending.receipt?.signing !== null
      || Object.entries(asset.companions).some(([name, helper]) => pending.receipt?.companions?.[name]?.archive_sha256 !== helper.archive_sha256
        || pending.receipt?.companions?.[name]?.signing !== null)
      || !sameSchemas(pending.receipt?.schemas, manifest.schemas)
      || !sameSchemas(pending.receipt?.legal, Object.fromEntries(Object.entries(manifest.legal).map(([name, evidence]) => [name, evidence.sha256])))
      || !/^\.codex-stage-[A-Za-z0-9]+\/codex(\.exe)?$/.test(pending.candidate)) {
    throw new Error("Unfinished Codex installation has invalid or different release evidence");
  }
  validateReceipt(pending.receipt);
  const candidate = path.join(root, pending.candidate);
  const executables = executableReceipts(pending.receipt).map(evidence => {
    const binary = path.join(root, evidence.executable), staged = path.join(path.dirname(candidate), evidence.executable);
    const installed = fs.existsSync(binary) && verifiedBinary(binary, evidence);
    if (!installed && (!fs.existsSync(staged) || !verifiedBinary(staged, evidence))) {
      throw new Error(`Unfinished Codex installation has no exact verified candidate: ${evidence.executable}`);
    }
    return { binary, staged, installed };
  });
  const attribution = Object.entries(pending.receipt.legal).map(([name, hash]) => {
    const staged = path.join(path.dirname(candidate), "upstream", name), shipped = path.join(root, name);
    const source = fs.existsSync(staged) ? staged : shipped;
    legalFile(source, hash);
    return { source, shipped };
  });
  for (const { source, shipped } of attribution) if (source !== shipped) fs.renameSync(source, shipped);
  for (const { binary, staged, installed } of executables) if (!installed) fs.renameSync(staged, binary);
  atomicJson(path.join(root, RECEIPT), pending.receipt);
  verifyInstallation(root);
  fs.unlinkSync(pendingFile);
  fs.rmSync(path.dirname(candidate), { recursive: true, force: true });
}

async function materialize({ destination, platform = `${process.platform}-${process.arch}`, manifestPath, fetchArchive = downloadArchive }) {
  if (!path.isAbsolute(destination)) throw new Error("Managed Codex destination must be explicit and absolute");
  const manifest = releaseManifest(manifestPath), asset = manifest.platforms[platform];
  if (!asset) throw new Error(`No official Codex distribution is pinned for ${platform}`);
  fs.mkdirSync(destination, { recursive: true });
  if (!fs.lstatSync(destination).isDirectory()) throw new Error("Managed Codex destination must be a real directory");
  const lock = path.join(destination, LOCK), fd = acquireLock(lock, manifest.version);
  let stage;
  try {
    recoverPending(destination, manifest, asset);
    const receiptPath = path.join(destination, RECEIPT);
    if (fs.existsSync(receiptPath)) {
      const existing = verifyInstallation(destination);
      if (versionOrder(existing.version, manifest.version) > 0) throw new Error("Managed Codex downgrade is forbidden");
      if (existing.version === manifest.version) {
        if (existing.archive_sha256 !== asset.archive_sha256
            || Object.entries(asset.companions).some(([name, helper]) => existing.companions[name]?.archive_sha256 !== helper.archive_sha256)
            || !sameSchemas(existing.schemas, manifest.schemas)
            || !sameSchemas(existing.legal, Object.fromEntries(Object.entries(manifest.legal).map(([name, evidence]) => [name, evidence.sha256])))) {
          throw new Error("Managed Codex release identity conflicts with the pinned manifest");
        }
        return existing;
      }
    } else if (["codex", ...companionNames(true)].some(name => [name, `${name}.exe`].some(file => fs.existsSync(path.join(destination, file))))) {
      throw new Error("Refusing to overwrite a Codex binary without distribution evidence");
    }
    const windows = windowsPlatform(platform);
    stage = fs.mkdtempSync(path.join(destination, ".codex-stage-"));
    const stageExecutable = async (asset, name) => {
      const archive = path.join(stage, `${name}.tar.gz`);
      await fetchArchive(asset.url, archive, asset.archive_bytes);
      if (fs.statSync(archive).size !== asset.archive_bytes || digest(archive) !== asset.archive_sha256) {
        throw new Error(`Official Codex archive SHA-256 does not match the pinned release: ${name}`);
      }
      const entries = execute(tarProgram(), ["-tzf", archive]).split("\n");
      if (entries.length !== 1 || entries[0] !== asset.archive_entry) throw new Error("Official Codex archive has unexpected entries");
      execute(tarProgram(), ["-xzf", archive, "-C", stage, asset.archive_entry]);
      const candidate = path.join(stage, executableFile(name, windows));
      if (asset.archive_entry !== path.basename(candidate)) fs.renameSync(path.join(stage, asset.archive_entry), candidate);
      if (!fs.lstatSync(candidate).isFile() || fs.statSync(candidate).size > 512 * 1024 * 1024) {
        throw new Error("Official Codex archive did not contain a bounded regular executable");
      }
      fs.chmodSync(candidate, 0o755);
      syncFile(candidate);
      return { archive_sha256: asset.archive_sha256, executable: path.basename(candidate), executable_sha256: digest(candidate),
        executable_bytes: fs.statSync(candidate).size, signing: null };
    };
    const primary = await stageExecutable(asset, "codex");
    const companions = {};
    for (const name of companionNames(windows)) companions[name] = await stageExecutable(asset.companions[name], name);
    const candidate = path.join(stage, executableFile("codex", windows));
    verifySchema(candidate, manifest, stage);
    fs.mkdirSync(path.join(stage, "upstream"));
    for (const [name, evidence] of Object.entries(manifest.legal)) {
      fs.copyFileSync(path.join(path.dirname(manifestPath || path.join(__dirname, "release.json")), "upstream", name), path.join(stage, "upstream", name));
      legalFile(path.join(stage, "upstream", name), evidence.sha256);
      syncFile(path.join(stage, "upstream", name));
    }
    syncFile(candidate);
    syncFile(path.join(stage, "upstream"));
    syncFile(stage);
    const receipt = {
      schema_version: 2, version: manifest.version, release_url: manifest.release_url,
      ...primary, companions, schema_baseline: manifest.tag, schemas: manifest.schemas,
      legal: Object.fromEntries(Object.entries(manifest.legal).map(([name, evidence]) => [name, evidence.sha256])), signing: null,
    };
    atomicJson(path.join(destination, PENDING), { candidate: path.relative(destination, candidate).split(path.sep).join("/"), receipt });
    recoverPending(destination, manifest, asset);
    return receipt;
  } finally {
    fs.closeSync(fd); fs.unlinkSync(lock);
    if (stage && !fs.existsSync(path.join(destination, PENDING))) fs.rmSync(stage, { recursive: true, force: true });
  }
}

function copyInstallation(source, destination) {
  const receipt = verifyInstallation(source);
  fs.mkdirSync(destination, { recursive: true });
  for (const { executable } of executableReceipts(receipt)) {
    fs.copyFileSync(path.join(source, executable), path.join(destination, executable), fs.constants.COPYFILE_FICLONE);
    fs.chmodSync(path.join(destination, executable), 0o755);
  }
  fs.copyFileSync(path.join(source, RECEIPT), path.join(destination, RECEIPT));
  for (const name of Object.keys(readJson(path.join(source, RECEIPT)).legal)) fs.copyFileSync(path.join(source, name), path.join(destination, name));
  return verifyInstallation(destination);
}

function captureSigning(roots) {
  return roots.map(root => ({ root, receipt: verifyInstallation(root) }));
}

function sealSigning(inputs) {
  for (const { root, receipt } of inputs) {
    const seal = evidence => {
      const binary = path.join(root, evidence.executable);
      execute("codesign", ["--verify", "--strict", binary]);
      return { ...evidence, executable_sha256: digest(binary), executable_bytes: fs.statSync(binary).size,
        signing: { kind: "ad-hoc", before_sha256: evidence.executable_sha256 } };
    };
    atomicJson(path.join(root, RECEIPT), { ...seal(receipt),
      companions: Object.fromEntries(Object.entries(receipt.companions).map(([name, evidence]) => [name, seal(evidence)])) });
    verifyInstallation(root);
  }
}

module.exports = { materialize, releaseManifest, verifyInstallation, verifySchema, copyInstallation, captureSigning, sealSigning, digest, RECEIPT };

if (require.main === module) {
  const args = process.argv.slice(2);
  if (args.length !== 3 || args[0] !== "materialize" || args[1] !== "--destination") {
    process.stderr.write("Usage: bootstrap.cjs materialize --destination <absolute provider bin directory>\n"); process.exitCode = 1;
  } else materialize({ destination: args[2] }).then(receipt => {
    process.stdout.write(`${JSON.stringify(receipt)}\n`);
  }).catch(error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
}
