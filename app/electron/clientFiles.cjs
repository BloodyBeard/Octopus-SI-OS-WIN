const crypto = require("node:crypto");
const { execFile } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const { promisify } = require("node:util");

const MAX_CLIENT_FILE_BYTES = 100 * 1024 * 1024;
const execFileAsync = promisify(execFile);
const MACOS_APPLICATIONS_SCRIPT = [
  'ObjC.import("AppKit");',
  "function run(argv) {",
  "  const workspace = $.NSWorkspace.sharedWorkspace;",
  "  const fileURL = $.NSURL.fileURLWithPath(argv[0]);",
  "  const urls = workspace.URLsForApplicationsToOpenURL(fileURL);",
  "  const applications = [];",
  "  for (let index = 0; index < Number(urls.count); index += 1) {",
  "    applications.push(ObjC.unwrap(urls.objectAtIndex(index).path));",
  "  }",
  "  const preferred = workspace.URLForApplicationToOpenURL(fileURL);",
  "  return JSON.stringify({",
  "    applications,",
  "    preferred: preferred == null ? null : ObjC.unwrap(preferred.path),",
  "  });",
  "}",
].join("\n");

async function downloadClientFileCopy({ bytes, filename, root }) {
  const content = boundedBytes(bytes);
  const safeName = safeFilename(filename);
  await fs.promises.mkdir(root, { recursive: true });
  const parsed = path.parse(safeName);
  for (let suffix = 0; suffix < 10_000; suffix += 1) {
    const candidate = suffix === 0
      ? path.join(root, safeName)
      : path.join(root, `${parsed.name} ${suffix + 1}${parsed.ext}`);
    try {
      await fs.promises.writeFile(candidate, content, { flag: "wx" });
      return { saved: true, path: candidate, filename: path.basename(candidate) };
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
    }
  }
  throw new Error("No collision-free Downloads filename is available.");
}

async function saveClientFile({ browserWindow, bytes, dialog, filename }) {
  const content = boundedBytes(bytes);
  const suggestedFilename = safeFilename(filename);
  const result = await dialog.showSaveDialog(browserWindow, {
    defaultPath: suggestedFilename,
    title: "Save file",
  });
  if (result.canceled || !result.filePath) return { saved: false };
  await fs.promises.mkdir(path.dirname(result.filePath), { recursive: true });
  await fs.promises.writeFile(result.filePath, content);
  return { saved: true, path: result.filePath, filename: path.basename(result.filePath) };
}

async function openClientFile({ action, bytes, filename, root, shell }) {
  if (!new Set(["open", "reveal"]).has(action)) throw new Error("File action is invalid.");
  const target = await materializeClientFile({ bytes, filename, root });
  if (action === "reveal") shell.showItemInFolder(target);
  else {
    const failure = await shell.openPath(target);
    if (failure) throw new Error(failure);
  }
  return { opened: true, path: target };
}

async function listClientFileOpenTargets({ app, bytes, downloadsRoot, filename, platform = process.platform, root }) {
  const target = await materializeClientFile({ bytes, filename, root });
  const catalog = platform === "darwin"
    ? await readMacApplicationCatalog(target)
    : { applicationPaths: [], preferredApplicationPath: null };
  const applicationPaths = catalog.applicationPaths;
  const applications = await Promise.all(applicationPaths.map(async (appPath) => ({
    icon: await fileIconDataUrl(app, appPath),
    id: applicationId(appPath),
    label: path.basename(appPath).replace(/\.app$/iu, ""),
  })));
  return {
    applications,
    downloadsFolderIcon: await fileIconDataUrl(app, downloadsRoot),
    fileManagerIcon: await fileIconDataUrl(app, fileManagerPath(platform)),
    preferredApplicationId: catalog.preferredApplicationPath ? applicationId(catalog.preferredApplicationPath) : null,
  };
}

async function openClientFileWithApplication({ applicationId: requestedId, bytes, filename, platform = process.platform, root }) {
  if (platform !== "darwin") throw new Error("Opening with a selected application is unavailable on this platform.");
  const target = await materializeClientFile({ bytes, filename, root });
  const { applicationPaths } = await readMacApplicationCatalog(target);
  const appPath = applicationPaths.find((candidate) => applicationId(candidate) === requestedId);
  if (!appPath) throw new Error("The selected application is no longer available for this file.");
  await execFileAsync("/usr/bin/open", ["-a", appPath, target], {
    maxBuffer: 1024 * 1024,
    timeout: 15_000,
  });
  return { opened: true, path: target };
}

async function materializeClientFile({ bytes, filename, root }) {
  const content = boundedBytes(bytes);
  const safeName = safeFilename(filename);
  const digest = crypto.createHash("sha256").update(content).digest("hex");
  const targetRoot = path.join(root, digest.slice(0, 20));
  const target = path.join(targetRoot, safeName);
  await fs.promises.mkdir(targetRoot, { recursive: true });
  const current = await fs.promises.readFile(target).catch(() => null);
  if (!current || !current.equals(content)) await writeAtomic(target, content);
  return target;
}

function boundedBytes(value) {
  const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value ?? []);
  if (bytes.byteLength > MAX_CLIENT_FILE_BYTES) throw new Error("The file exceeds the 100 MB client boundary.");
  return bytes;
}

async function readMacApplicationCatalog(target) {
  const { stdout } = await execFileAsync("/usr/bin/osascript", [
    "-l",
    "JavaScript",
    "-e",
    MACOS_APPLICATIONS_SCRIPT,
    target,
  ], {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
    timeout: 10_000,
  });
  let values;
  try {
    values = JSON.parse(stdout.trim() || "{}");
  } catch {
    throw new Error("macOS returned an invalid application list.");
  }
  if (!values || typeof values !== "object" || !Array.isArray(values.applications)) {
    throw new Error("macOS returned an invalid application list.");
  }
  const seen = new Set();
  const result = [];
  const ordered = [values.preferred, ...values.applications].slice(0, 129);
  for (const value of ordered) {
    if (typeof value !== "string" || !path.isAbsolute(value) || !value.toLowerCase().endsWith(".app")) continue;
    const resolved = await fs.promises.realpath(value).catch(() => "");
    if (!resolved || seen.has(resolved)) continue;
    const stat = await fs.promises.stat(resolved).catch(() => null);
    if (!stat?.isDirectory()) continue;
    seen.add(resolved);
    result.push(resolved);
  }
  const preferredCandidate = typeof values.preferred === "string"
    ? await fs.promises.realpath(values.preferred).catch(() => "")
    : "";
  return {
    applicationPaths: result,
    preferredApplicationPath: result.includes(preferredCandidate) ? preferredCandidate : null,
  };
}

async function fileIconDataUrl(app, target) {
  if (!target) return null;
  try {
    const icon = await app.getFileIcon(target, { size: "small" });
    if (icon.isEmpty()) return null;
    const dataUrl = icon.toDataURL();
    return dataUrl.length <= 256 * 1024 ? dataUrl : null;
  } catch {
    return null;
  }
}

function fileManagerPath(platform) {
  if (platform === "darwin") return "/System/Library/CoreServices/Finder.app";
  if (platform === "win32" && process.env.WINDIR) return path.join(process.env.WINDIR, "explorer.exe");
  return null;
}

function applicationId(appPath) {
  return crypto.createHash("sha256").update(appPath).digest("hex").slice(0, 24);
}

async function writeAtomic(target, bytes) {
  await fs.promises.mkdir(path.dirname(target), { recursive: true });
  const temporary = path.join(path.dirname(target), `.${path.basename(target)}.${process.pid}.${crypto.randomUUID()}.tmp`);
  try {
    await fs.promises.writeFile(temporary, bytes, { flag: "wx" });
    await fs.promises.rename(temporary, target);
  } catch (error) {
    await fs.promises.rm(temporary, { force: true });
    throw error;
  }
}

function safeFilename(value) {
  const filename = typeof value === "string" ? value.replace(/[\\/:*?"<>|\u0000-\u001f]/g, "-").trim().slice(0, 255) : "";
  return filename && filename !== "." && filename !== ".." ? filename : "artifact";
}

module.exports = {
  downloadClientFileCopy,
  listClientFileOpenTargets,
  materializeClientFile,
  openClientFile,
  openClientFileWithApplication,
  saveClientFile,
};
