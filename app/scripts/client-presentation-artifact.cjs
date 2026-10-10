"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { createHash, randomBytes } = require("node:crypto");
const { sourceContract, settingsAssets } = require("../electron/nativePresentation.cjs");
const digest = bytes => createHash("sha256").update(bytes).digest("hex");
function regular(file) {
  if (!path.isAbsolute(file) || fs.realpathSync(file) !== file || !fs.lstatSync(file).isFile()) throw new Error("Native presentation build input must be a canonical regular file");
  return fs.readFileSync(file);
}
function publishClientPresentation({ sourceRoot, buildReceipt, destination }) {
  if (!path.isAbsolute(destination) || fs.existsSync(destination) || fs.realpathSync(path.dirname(destination)) !== path.dirname(destination)) throw new Error("Client presentation publication requires a fresh resource directory");
  if (fs.realpathSync(sourceRoot) !== sourceRoot) throw new Error("Native presentation source must be canonical");
  require(path.join(sourceRoot, "build.cjs")).verifyReceipt(buildReceipt, buildReceipt.architecture);
  const binary = regular(buildReceipt.output), manifest = regular(path.join(sourceRoot, "manifest.json"));
  if (digest(binary) !== buildReceipt.output_sha256) throw new Error("Native presentation compiler output changed before client publication");
  const receipt = { schema_version: 1, id: "octopus.client.presentation", module: "octopus-pip-receiver.node", architecture: buildReceipt.architecture,
    compiled_output_sha256: buildReceipt.output_sha256, output_sha256: buildReceipt.output_sha256, build: buildReceipt, signing: null };
  sourceContract(JSON.parse(manifest), receipt, digest(manifest), buildReceipt.architecture);
  fs.mkdirSync(destination); fs.mkdirSync(path.join(destination, "assets"));
  fs.writeFileSync(path.join(destination, receipt.module), binary, { flag: "wx", mode: 0o755 });
  fs.writeFileSync(path.join(destination, "manifest.json"), manifest, { flag: "wx", mode: 0o644 });
  fs.writeFileSync(path.join(destination, "assets", "pop-out-window-egg@3x.png"), regular(path.join(sourceRoot, "assets", "pop-out-window-egg@3x.png")), { flag: "wx", mode: 0o644 });
  for (const [name, expected] of Object.entries(settingsAssets)) {
    const bytes = regular(path.join(sourceRoot, name)); if (digest(bytes) !== expected) throw new Error("Settings source resource changed before publication");
    fs.mkdirSync(path.dirname(path.join(destination, name)), { recursive: true });fs.writeFileSync(path.join(destination, name), bytes, { flag: "wx", mode: 0o644 });
  }
  fs.writeFileSync(path.join(destination, "receipt.json"), JSON.stringify(receipt, null, 2) + "\n", { flag: "wx", mode: 0o644 });
  return destination;
}
function sealClientPresentation(destination) {
  const receiptPath = path.join(destination, "receipt.json"), receipt = JSON.parse(regular(receiptPath));
  const manifest = regular(path.join(destination, "manifest.json"));
  sourceContract(JSON.parse(manifest), receipt, digest(manifest), receipt.architecture);
  if (receipt.signing !== null || receipt.output_sha256 !== receipt.compiled_output_sha256) throw new Error("Client presentation receipt was already sealed or replaced");
  const result = { ...receipt, output_sha256: digest(regular(path.join(destination, receipt.module))), signing: { scheme: "ad-hoc", owner: "package-macos.cjs" } };
  const temporary = receiptPath + "." + randomBytes(12).toString("hex");
  fs.writeFileSync(temporary, JSON.stringify(result, null, 2) + "\n", { flag: "wx", mode: 0o644 }); fs.renameSync(temporary, receiptPath);
}
module.exports = { publishClientPresentation, sealClientPresentation };
