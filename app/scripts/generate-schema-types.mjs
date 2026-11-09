import { compileFromFile } from "json-schema-to-typescript";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const schemasDir = path.join(here, "..", "..", "schemas");
const appSchemasDir = path.join(here, "..", "schemas");
const outDir = path.join(here, "..", "src", "generated");

await mkdir(outDir, { recursive: true });
const files = (await readdir(schemasDir)).filter((f) => f.endsWith(".schema.json")).sort();
if (files.length === 0) {
  console.error(`no schemas found in ${schemasDir}; run the Rust emitter first`);
  process.exit(1);
}
for (const file of files) {
  const stem = file.replace(/\.schema\.json$/, "");
  const ts = await compileFromFile(path.join(schemasDir, file), {
    additionalProperties: false,
    bannerComment:
      `/* Generated from schemas/${file} — do not edit.\n` +
      ` * Source of truth: the Rust types (crates/octopus-schemas surfaces()).\n` +
      ` * Regenerate: cargo run -p octopus-schemas --bin octopus-emit-schemas && npm run gen:types */`,
  });
  await writeFile(path.join(outDir, `${stem}.d.ts`), ts);
  console.log(`wrote src/generated/${stem}.d.ts`);
}

const projectionReadContract = "projection_read_contract.schema.json";
const projectionReadSchema = JSON.parse(await readFile(path.join(appSchemasDir, projectionReadContract), "utf8"));
const projectionReadVersion = projectionReadSchema.properties.contract_version.const;
const projectionReadMethods = projectionReadSchema.$defs.projectionReadMethod.enum;
const clientSurfaceProjectionReadMethods = projectionReadSchema.$defs.clientSurfaceProjectionReadMethod.enum;
const projectionImplementation = await readFile(path.join(here, "..", "electron", "projection", "queryRouter.cjs"), "utf8");
const implementedProjectionReadMethods = [...new Set([
  ...projectionImplementation.matchAll(/(?:case\s+|method\s*===\s*)"(projection\.[^"]+)"/g),
].map((match) => match[1]))];
const declaredProjectionReadMethodSet = new Set(projectionReadMethods);
const implementedProjectionReadMethodSet = new Set(implementedProjectionReadMethods);
const missingProjectionMethods = projectionReadMethods.filter((method) => !implementedProjectionReadMethodSet.has(method));
const undeclaredProjectionMethods = implementedProjectionReadMethods.filter((method) => !declaredProjectionReadMethodSet.has(method));
if (
  declaredProjectionReadMethodSet.size !== projectionReadMethods.length
  || implementedProjectionReadMethodSet.size !== implementedProjectionReadMethods.length
  || missingProjectionMethods.length > 0
  || undeclaredProjectionMethods.length > 0
  || clientSurfaceProjectionReadMethods.some((method) => !declaredProjectionReadMethodSet.has(method))
) {
  throw new Error(JSON.stringify({ missingProjectionMethods, undeclaredProjectionMethods }));
}
const projectionReadBinding = [
  `export const PROJECTION_READ_CONTRACT_VERSION = ${JSON.stringify(projectionReadVersion)} as const;`,
  `export const PROJECTION_READ_METHODS = ${JSON.stringify(projectionReadMethods, null, 2)} as const;`,
  "export type ProjectionReadMethod = typeof PROJECTION_READ_METHODS[number];",
  `export const CLIENT_SURFACE_PROJECTION_READ_METHODS = ${JSON.stringify(clientSurfaceProjectionReadMethods, null, 2)} as const;`,
  "export type ClientSurfaceProjectionReadMethod = typeof CLIENT_SURFACE_PROJECTION_READ_METHODS[number];",
  "export type OctopusClientProjectionReadRequest = {",
  "  contract_version: typeof PROJECTION_READ_CONTRACT_VERSION;",
  "  method: ProjectionReadMethod;",
  "  params?: Record<string, unknown>;",
  "};",
  "",
].join("\n");
await writeFile(path.join(outDir, "projectionReadContract.ts"), projectionReadBinding);
console.log("wrote src/generated/projectionReadContract.ts");
