import { createHash } from "node:crypto";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binary = resolve(root, "bin/octopus-opencodex-bridge");
const inputs = [
  "package.json",
  "package-lock.json",
  "upstream.json",
  "src/main.ts"
];
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const files = Object.fromEntries(inputs.map((name) => [name, digest(readFileSync(resolve(root, name)))]));
const binaryBytes = readFileSync(binary);
const receipt = {
  schema_version: 1,
  executable: "bin/octopus-opencodex-bridge",
  executable_sha256: digest(binaryBytes),
  executable_bytes: statSync(binary).size,
  inputs: files,
  upstream: JSON.parse(readFileSync(resolve(root, "upstream.json"), "utf8"))
};
writeFileSync(resolve(root, "build-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o644 });
