const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");

const appRoot = path.resolve(__dirname, "..");
const source = (relativePath) => fs.readFileSync(path.join(appRoot, relativePath), "utf8");

const main = source("src/main.tsx");
const filesView = source("src/components/files/FilesView.tsx");
const filesEditor = source("src/components/files/FilesCodeEditor.tsx");
const filesCss = source("src/components/files/files.css");
const pierreEditor = source("src/components/files/FilesPierreEditor.tsx");
const readonlyViewer = source("src/components/files/FilesReadonlyViewer.tsx");
const codexSurface = source("src/components/OctopusSurface.tsx");
const viteConfig = source("vite.config.ts");

assert.match(main, /WorkerPoolContextProvider, useWorkerPool/);
assert.match(main, /new PierreDiffWorker\(\)/);
assert.match(main, /dark: octopusDiffThemeName\("dark", appearance\.darkCodeTheme\)/);
assert.match(main, /light: octopusDiffThemeName\("light", appearance\.lightCodeTheme\)/);
assert.doesNotMatch(filesEditor, /disableWorkerPool/);
assert.match(viteConfig, /worker: \{ format: "es" \}/);

assert.match(filesCss, /--diffs-gap-block: 0/);
assert.doesNotMatch(filesEditor, /<textarea/);
assert.match(filesEditor, /lazy\(\(\) => import\("\.\/FilesPierreEditor"\)/);
assert.match(pierreEditor, /from "@octopus\/files-diffs\/edit"/);
assert.match(pierreEditor, /from "@octopus\/files-diffs"/);
assert.doesNotMatch(pierreEditor, /@octopus\/files-diffs\/react/);
assert.match(filesView, /documentId=\{JSON\.stringify\(\[file\.rootId, file\.path\]\)\}/);
assert.match(filesEditor, /lazy\(\(\) => import\("\.\/FilesReadonlyViewer"\)/);
assert.doesNotMatch(filesEditor + readonlyViewer, /@octopus\/files-diffs\/react/);
assert.match(readonlyViewer, /name: filename, lang: fileLanguage\(filename\), contents: value/);
assert.match(filesView, /file\?\.kind === "text" && !file\.truncated \? draft : null/);
assert.match(filesView, /FILES_TEXT_PREVIEW_MAX_BYTES = 2 \* 1024 \* 1024/);
assert.match(filesView, /return \[safeArtifactFilename\(filename\)\]/);
assert.match(filesView, /Promise\.all\(\["", \.\.\.ancestors\]\.map\(\(path\) => loadDirectory/);

assert.match(codexSurface, /useLayoutEffect\(\(\) => \{\s*const applied = appliedRef\.current;/);

assert.doesNotMatch(codexSurface, /model\.expand\(parentPath\)/);
assert.match(codexSurface, /model\.scrollToPath\(selectedPath, \{ offset: revealSelectedPathScrollOffset \}\)/);

test("Files render worker applies its theme and throws the exact sync failure", async () => {
  const parsed = ts.createSourceFile("main.tsx", main, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owner = parsed.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "PierreDiffWorkerThemeSync");
  assert.ok(owner, "The current render-worker failure owner must exist");
  const code = ts.transpileModule(owner.getText(parsed), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const theme = { dark: "dark-fixture", light: "light-fixture" };
  const failure = new Error("injected worker theme failure");
  for (const rejected of [null, failure]) {
    let state = null, effect;
    const requests = [];
    const sync = Function("React", "useWorkerPool", `${code}\nreturn PierreDiffWorkerThemeSync;`)(
      { useState: () => [state, (value) => { state = value; }], useEffect: (callback) => { effect = callback; } },
      () => ({ setRenderOptions: (request) => { requests.push(request); return rejected ? Promise.reject(rejected) : Promise.resolve(); } }),
    );
    assert.equal(sync({ theme }), null);
    const cleanup = effect();
    try {
      await Promise.resolve();
      assert.deepEqual(requests, [{ theme }]);
      if (rejected) assert.throws(() => sync({ theme }), (error) => error === failure);
      else assert.equal(sync({ theme }), null);
    } finally { cleanup(); }
  }
});

console.log("Files source contracts passed");


test("trace file links resolve only through their task and exact tool workspace", () => {
  const parsed = ts.createSourceFile("ChatView.tsx", source("src/components/ChatView.tsx"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owner = parsed.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "traceFileOpenTarget");
  const code = ts.transpileModule(owner.getText(parsed), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const resolve = Function(code + ";return traceFileOpenTarget;")();
  const room = (tool, root) => ({ root, tools: [tool], surfaces: [{ kind: "files", mode: "direct", root }, { kind: "files", mode: "direct", root: "/toolchain" }] });
  const run = { workerEnvironments: [room("read.one", "/fixture"), room("read.two", "/another")], workerEnvironment: {} };
  assert.deepEqual(resolve("./context.txt", run, "read.one"), { kind: "file", absolutePath: "/fixture/context.txt" });
  assert.deepEqual(resolve("context.txt", run, "read.two"), { kind: "file", absolutePath: "/another/context.txt" });
  assert.deepEqual(resolve("/explicit/file.txt", null), { kind: "file", absolutePath: "/explicit/file.txt" });
  assert.equal(resolve("context.txt", { ...run, workerEnvironments: [room("read", "/")] }, "read").absolutePath, "/context.txt");
  assert.equal(resolve("context.txt", { ...run, workerEnvironments: [room("read", "C:\\\\")] }, "read").absolutePath, "C:/context.txt");
  for (const [file, selectedRun, tool] of [["context.txt", run], ["context.txt", null], ["context.txt", run, "missing"], ["../context.txt", run, "read.one"]]) assert.throws(() => resolve(file, selectedRun, tool));
});
