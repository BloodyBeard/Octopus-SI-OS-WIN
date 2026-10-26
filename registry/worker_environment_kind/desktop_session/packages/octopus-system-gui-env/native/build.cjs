const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");

function buildDesktopNative(destination) {
  if (process.platform !== "darwin") throw new Error("Desktop native substrate supports macOS only");
  if (!path.isAbsolute(destination)) throw new Error("Native helper destination must be absolute");
  if (!["arm64", "x64"].includes(process.arch)) throw new Error("Native helper architecture is unsupported");
  if (process.env.SWIFTCI_USE_LOCAL_DEPS !== undefined) throw new Error("Native dependencies must use their exact package resolution");
  const manifest = path.join(__dirname, "Package.swift");
  const resolved = path.join(__dirname, "Package.resolved");
  const hash = file => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
  for (const file of [manifest, resolved]) {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("Native dependency recipe must be regular package source");
  }
  const pins = JSON.parse(fs.readFileSync(resolved, "utf8"));
  if (pins.version !== 2 || !Array.isArray(pins.pins) || !pins.pins.length
      || pins.pins.some(pin => pin.kind !== "remoteSourceControl" || !/^[0-9a-f]{40}$/.test(pin.state?.revision ?? ""))) {
    throw new Error("Native dependency recipe lacks exact source revisions");
  }
  const before = [hash(manifest), hash(resolved)];
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-desktop-native-build-"));
  const args = ["swift", "build", "--package-path", __dirname, "--scratch-path", scratch,
    "--configuration", "release", "--product", "octopus-desktop-native", "--jobs", "2",
    "--arch", process.arch === "arm64" ? "arm64" : "x86_64", "--force-resolved-versions"];
  const run = (command, arguments_, capture = false) => {
    const result = spawnSync(command, arguments_, capture
      ? { encoding: "utf8", maxBuffer: 1024 * 1024, stdio: ["ignore", "pipe", "inherit"] }
      : { stdio: "inherit" });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`Desktop native build command failed (${result.status})`);
    return result.stdout;
  };
  run("xcrun", args);
  const binDirectory = run("xcrun", [...args, "--show-bin-path"], true).trim();
  if (!path.isAbsolute(binDirectory) || !fs.realpathSync(binDirectory).startsWith(`${fs.realpathSync(scratch)}${path.sep}`)) {
    throw new Error("SwiftPM returned an output outside its exact native build directory");
  }
  if (before[0] !== hash(manifest) || before[1] !== hash(resolved)) throw new Error("Native dependency recipe changed during the build");
  const binary = path.join(binDirectory, "octopus-desktop-native");
  const stat = fs.lstatSync(binary);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size === 0) throw new Error("SwiftPM did not produce the native helper");
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.copyFileSync(binary, destination);
  fs.chmodSync(destination, stat.mode & 0o777);
  run("codesign", ["--force", "--sign", "-", "--identifier", "com.octopus.desktop.native", destination]);
  fs.rmSync(scratch, { recursive: true });
  return destination;
}

module.exports = { buildDesktopNative };
if (require.main === module) buildDesktopNative(process.argv[2]);
