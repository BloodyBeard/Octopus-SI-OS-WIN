import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { reconstructCodex, reconstructRaycast } from "../src/components/settings/code-themes/reconstruct.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const receipt = JSON.parse(fs.readFileSync(path.join(root, "app/publication/custom-theme-provenance.json"), "utf8"));
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const canonical = (value) => Array.isArray(value) ? value.map(canonical)
  : value && typeof value === "object" ? Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])])) : value;
const objectHash = (value) => hash(JSON.stringify(canonical(value)));
const verify = (actual, expected, label) => {
  if (actual !== expected) throw new Error(`Theme receipt differs: ${label}`);
};
for (const source of receipt.sources) {
  verify(hash(fs.readFileSync(path.join(root, source.path))), source.sha256, source.path);
}
verify(hash(fs.readFileSync(path.join(root, receipt.appearance_parameters.path))), receipt.appearance_parameters.sha256, "appearance parameters");
for (const theme of receipt.reconstructed_themes) {
  if (fs.existsSync(path.join(root, theme.previous_file))) throw new Error(`Extracted theme input remains: ${theme.previous_file}`);
  const stem = path.basename(theme.previous_file, ".json");
  const kind = stem.split("-")[0];
  const reconstructed = theme.base === "pierre-0.0.29" ? reconstructCodex(kind)
    : theme.base === "ray-so" ? reconstructRaycast(kind) : (() => { throw new Error(`Unknown reconstruction base: ${theme.base}`); })();
  verify(objectHash(reconstructed), theme.theme_canonical_sha256, stem);
}
for (const theme of receipt.retained_themes) {
  verify(hash(fs.readFileSync(path.join(root, theme.previous_file))), theme.previous_sha256, theme.previous_file);
}
const seeds = JSON.parse(fs.readFileSync(path.join(root, "app/src/design-system/code-theme-seeds.json"), "utf8"));
verify(Object.values(seeds).reduce((count, entries) => count + Object.keys(entries).length, 0), receipt.seed_count, "seed count");
verify(objectHash(seeds), receipt.seed_canonical_sha256, "all seeds");
console.log("Verified 4 reconstructed full objects, 16 unchanged retained themes, pinned sources, and 43 baseline seeds.");
