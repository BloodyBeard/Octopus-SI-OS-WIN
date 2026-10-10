import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const lock = JSON.parse(fs.readFileSync(path.join(appRoot, "package-lock.json"), "utf8"));
const outputPath = path.join(appRoot, "src", "generated", "client-licenses.json");
const entries = [];
const seen = new Set();
const supplemental = JSON.parse(fs.readFileSync(path.join(appRoot, "licenses/dependency-notices.json"), "utf8"));
if (supplemental.schema_version !== 1 || !Array.isArray(supplemental.entries) || !Array.isArray(supplemental.assets)) throw new Error("Invalid supplemental notice inventory");
const supplementalByPackage = new Map(supplemental.entries.map((entry) => [entry.package, entry]));
const omittedOptionalDependencies = [];
const args = process.argv.slice(2);
if (args.some((argument) => argument !== "--check")) throw new Error("Usage: generate-client-licenses.mjs [--check]");
if (!lock.packages || typeof lock.packages !== "object") throw new Error("The dependency lockfile has no package inventory");

for (const [packagePath, lockEntry] of Object.entries(lock.packages ?? {})) {
  if (!packagePath.startsWith("node_modules/") || lockEntry.dev === true) continue;
  const packageRoot = path.join(appRoot, packagePath);
  const packageJsonPath = path.join(packageRoot, "package.json");
  if (!fs.existsSync(packageJsonPath)) {
    if (lockEntry.optional === true) {
      omittedOptionalDependencies.push(packagePath);
      continue;
    }
    throw new Error(`Required production dependency is not installed: ${packagePath}`);
  }
  const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));
  const name = typeof packageJson.name === "string" ? packageJson.name : "";
  const version = typeof packageJson.version === "string" ? packageJson.version : "";
  if (!name || !version) throw new Error(`Dependency metadata is missing name or version: ${packagePath}`);
  if (version !== lockEntry.version) throw new Error(`Installed dependency differs from the lockfile: ${packagePath}`);
  const key = `${name}@${version}`;
  if (seen.has(key)) continue;
  seen.add(key);
  const noticeFiles = fs.readdirSync(packageRoot, { withFileTypes: true })
    .filter((entry) => entry.isFile() && /^(?:licen[cs]e|copying|notice)(?:[._-]|$)/i.test(entry.name))
    .sort((left, right) => left.name.localeCompare(right.name));
  const notices = [];
  for (const noticeFile of noticeFiles) {
    const noticePath = path.join(packageRoot, noticeFile.name);
    const size = fs.statSync(noticePath).size;
    if (size > 512_000) throw new Error(`Dependency notice exceeds the supported size: ${noticePath}`);
    const text = fs.readFileSync(noticePath, "utf8").replace(/\r\n/g, "\n").trim();
    if (text) notices.push(noticeFiles.length > 1 ? `# ${noticeFile.name}\n\n${text}` : text);
  }
  for (const notice of supplementalByPackage.get(key)?.notices ?? []) {
    const text = readVerifiedNotice(notice);
    if (!notices.includes(text)) notices.push(text);
  }
  entries.push({
    name,
    version,
    license: normalizeLicense(packageJson.license ?? lockEntry.license),
    notice: notices.join("\n\n---\n\n"),
  });
}

for (const asset of supplemental.assets) {
  const assetPath = path.resolve(appRoot, "..", asset.asset_path);
  if (!assetPath.startsWith(`${appRoot}${path.sep}`) || fs.realpathSync(assetPath) !== assetPath
      || createHash("sha256").update(fs.readFileSync(assetPath)).digest("hex") !== asset.sha256) {
    throw new Error(`Bundled asset differs from its license receipt: ${asset.asset_path}`);
  }
  entries.push({ name: asset.name, version: asset.version, license: asset.license_declaration, notice: asset.notices.map(readVerifiedNotice).join("\n\n---\n\n") });
}
const iconRoot = path.join(appRoot, "licenses/icon-assets");
const icons = JSON.parse(fs.readFileSync(path.join(iconRoot, "notices.json"), "utf8"));
if (icons.schema !== "octopus.client_icon_notices.v1" || !icons.upstreams || !icons.shapes || !Array.isArray(icons.glyphs)) throw new Error("Invalid icon notice inventory");
const usedIconSources = new Set(icons.glyphs.flatMap((glyph) => glyph.shapes).map((name) => icons.shapes[name].upstream));
for (const name of usedIconSources) {
  const source = icons.upstreams[name];
  entries.push({
    name: `Icon assets: ${name}`,
    version: source.commit,
    license: source.license,
    notice: readVerifiedNotice({ path: `app/licenses/icon-assets/${source.notice}`, sha256: source.notice_sha256 }),
  });
}

const themeNotices = JSON.parse(fs.readFileSync(path.join(appRoot, "licenses/themes/notices.json"), "utf8"));
if (themeNotices.schema_version !== 1 || !Array.isArray(themeNotices.entries)) throw new Error("Invalid theme source notice inventory");
for (const source of themeNotices.entries) {
  for (const receipt of source.sources) {
    const filename = path.resolve(appRoot, "..", receipt.path);
    if (!filename.startsWith(`${appRoot}${path.sep}`) || fs.realpathSync(filename) !== filename
        || createHash("sha256").update(fs.readFileSync(filename)).digest("hex") !== receipt.sha256) {
      throw new Error(`Theme source differs from its notice receipt: ${receipt.path}`);
    }
  }
  entries.push({ name: source.name, version: source.version, license: source.license, notice: source.notices.map(readVerifiedNotice).join("\n\n---\n\n") });
}

entries.sort((left, right) => left.name.localeCompare(right.name) || left.version.localeCompare(right.version));
const inventory = { version: 1, entries, omittedOptionalDependencies: omittedOptionalDependencies.sort() };
const output = `${JSON.stringify(inventory, null, 2)}\n`;
if (args.includes("--check")) {
  if (fs.readFileSync(outputPath, "utf8") !== output) throw new Error("Client dependency notices are stale; run npm run gen:licenses");
  console.log(`Verified ${entries.length} production dependency notices.`);
} else {
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, output);
  console.log(`Generated ${path.relative(appRoot, outputPath)} with ${entries.length} production dependency notices.`);
}
if (omittedOptionalDependencies.length) console.log(`Optional dependencies absent from this installation: ${omittedOptionalDependencies.join(", ")}`);

function normalizeLicense(value) {
  if (typeof value === "string" && value.trim()) return value.trim();
  if (value && typeof value === "object" && typeof value.type === "string") return value.type;
  return "Not declared";
}

function readVerifiedNotice(receipt) {
  const filename = path.resolve(appRoot, "..", receipt.path);
  if (!filename.startsWith(`${appRoot}${path.sep}licenses${path.sep}`) || fs.realpathSync(filename) !== filename) throw new Error(`Invalid notice path: ${receipt.path}`);
  const bytes = fs.readFileSync(filename);
  if (bytes.length > 512_000 || createHash("sha256").update(bytes).digest("hex") !== receipt.sha256) throw new Error(`Notice differs from its recorded hash: ${receipt.path}`);
  const text = bytes.toString("utf8").replace(/\r\n/g, "\n").trim();
  if (!text) throw new Error(`Empty supplemental notice: ${receipt.path}`);
  return text;
}
