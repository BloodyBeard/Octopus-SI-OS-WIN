const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const path = require("node:path");
const os = require("node:os");
const { test } = require("node:test");
const ts = require("typescript");
const { readFileSync } = require("node:fs");
const { createClientAttachmentStagingStore, selectClientAttachments, stageClientAttachmentPaths } = require("../electron/clientAttachments.cjs");
const app = path.resolve(__dirname, "..");
function compile(relative, overrides = {}) {
  const filename = path.join(app, relative);
  const code = ts.transpileModule(readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  Function("exports", "require", code)(exports, (name) => overrides[name] ?? (name.startsWith(".") ? compile(path.relative(app, path.resolve(path.dirname(filename), `${name}.ts`)), overrides) : require(name)));
  return exports;
}
const drop = compile("src/components/chat/attachmentDrop.ts");
const file = (name = "note.txt") => new File(["bytes"], name, { type: "text/plain", lastModified: 1 });
const folder = file("folder");
const transfer = (files, items = files.map((f) => ({ kind: "file", getAsFile: () => f, webkitGetAsEntry: () => ({ isDirectory: f === folder }) })), types = ["Files"]) => ({ files, items, types });
const handle = { id: "stage-1", contentHash: "abc", label: "operator.file", name: "folder.directory.json", type: "application/json", size: 10, lastModified: 1 };

test("drop captures richer item evidence and never captures pane or text drags", () => {
  const regular = file();
  const richer = transfer([regular], transfer([regular, folder]).items);
  assert.deepEqual(drop.captureAttachmentDrop(richer).map((item) => item.isDirectory), [false, true]);
  assert.deepEqual(drop.captureAttachmentDrop(transfer([], [], ["text/plain"])), []);
  assert.equal(drop.isAttachmentDrop(transfer([folder], undefined, ["Files", "application/x-octopus-client-pane"])), false);
  assert.throws(() => drop.captureAttachmentDrop(transfer([], [{ kind: "file", getAsFile: () => null, webkitGetAsEntry: () => ({ isDirectory: true }) }])), /could not be read/);
});

test("mixed native folder and non-native file preserve order and original bytes; rejection is atomic", async () => {
  const original = file(), entries = drop.captureAttachmentDrop(transfer([original, folder])), discarded = [];
  const bridge = { stageDroppedAttachments: async () => ({ canceled: false, attachments: [handle], fileIndices: [1] }), discardClientAttachmentStages: async (handles) => discarded.push(...handles) };
  const result = await drop.resolveAttachmentDrop(entries, bridge);
  assert.equal(result[0], original); assert.equal(await result[0].text(), "bytes"); assert.equal(result[1], handle);
  await assert.rejects(drop.resolveAttachmentDrop(entries), /native attachment packaging/);
  assert.deepEqual(await drop.resolveAttachmentDrop(entries, { ...bridge, stageDroppedAttachments: async () => ({ canceled: true, attachments: [handle], fileIndices: [1] }) }), null);
  assert.deepEqual(discarded, [handle]); discarded.length = 0;
  await assert.rejects(drop.resolveAttachmentDrop(entries, { ...bridge, stageDroppedAttachments: async () => ({ canceled: false, attachments: [handle], fileIndices: [0] }) }), /no native file handle/);
  assert.deepEqual(discarded, [handle]);
  await assert.rejects(drop.resolveAttachmentDrop(entries, { ...bridge, stageDroppedAttachments: async () => { throw new Error("read denied"); } }), /read denied/);
  assert.throws(() => drop.validateAttachmentDrop([original, handle], 1), /slots/);
});

test("actual preload captures every native File path synchronously and preserves mixed indices", async () => {
  let bridge, finish; const calls = [], captures = [], regular = file(), synthetic = file("pasted.txt");
  const paths = new Map([[regular, "/operator/note.txt"], [folder, "/operator/folder"]]);
  const electron = {
    contextBridge: { exposeInMainWorld: (name, value) => { assert.equal(name, "octopusTransport"); bridge = value; } },
    webUtils: { getPathForFile: (value) => { assert.ok(value instanceof File); captures.push(value); return paths.get(value) ?? ""; } },
    ipcRenderer: { sendSync: (...args) => { assert.deepEqual(args, ["octopus:access-inherit", "claim"]); return "owner"; }, invoke: (...args) => { calls.push(args); return new Promise((resolve) => { finish = resolve; }); } },
  };
  Function("require", "process", readFileSync(path.join(app, "electron/preload.cjs"), "utf8"))((name) => { assert.equal(name, "electron"); return electron; }, { argv: ["--octopus-access-claim=claim"] });
  const pending = bridge.stageDroppedAttachments([regular, synthetic, folder]);
  assert.deepEqual(captures, [regular, synthetic, folder]);
  assert.deepEqual(calls, [["octopus:stage-dropped-attachments", "owner", ["/operator/note.txt", "/operator/folder"]]]);
  paths.delete(folder);
  finish({ canceled: false, attachments: [handle, { ...handle, id: "folder-stage" }] });
  assert.deepEqual((await pending).fileIndices, [0, 2]);
  assert.deepEqual(await bridge.stageDroppedAttachments([synthetic]), { canceled: false, attachments: [], fileIndices: [] });
  assert.equal(calls.length, 1); assert.equal((await drop.resolveAttachmentDrop(drop.captureAttachmentDrop(transfer([synthetic])), bridge))[0], synthetic);
  const rejected = assert.rejects(drop.resolveAttachmentDrop(drop.captureAttachmentDrop(transfer([regular, folder])), bridge), /no native file handle/);
  finish({ canceled: false, attachments: [handle] });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls.at(-1), ["octopus:attachment-stages-discard", "owner", [handle]]); finish(); await rejected;
  const beforeInvalid = calls.length; await assert.rejects(bridge.stageDroppedAttachments([{}]));
  await assert.rejects(bridge.stageDroppedAttachments(Array(13).fill(regular)), /up to 12/); assert.equal(calls.length, beforeInvalid);
});

test("actual main drop, picker and discard handlers enforce token ownership and rollback only their stages", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-drop-handlers-"));
  const stagingDir = path.join(root, "stages", "attachment-staging");
  const store = createClientAttachmentStagingStore({ root: () => path.join(root, "stages") });
  const main = ts.createSourceFile("main.cjs", readFileSync(path.join(app, "electron/main.cjs"), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  let live = true, selectedPaths = []; const sessions = [];
  const dependencies = { stageClientAttachmentPaths, selectClientAttachments, dialog: { showOpenDialog: async () => ({ canceled: false, filePaths: selectedPaths }) }, BrowserWindow: { fromWebContents: () => ({ isDestroyed: () => !live }) }, getClientAttachmentStagingStore: () => store, getClientAccessController: () => ({ requireSession: (token) => { if (!["owner", "other"].includes(token)) throw new Error("no session"); sessions.push(token); } }) };
  const handler = (channel) => {
    const statement = main.statements.find((node) => ts.isExpressionStatement(node) && ts.isCallExpression(node.expression) && node.expression.expression.getText(main) === "ipcMain.handle" && node.expression.arguments[0]?.text === channel);
    assert.ok(statement, `missing actual handler ${channel}`);
    return Function(...Object.keys(dependencies), `return (${statement.expression.arguments[1].getText(main)});`)(...Object.values(dependencies));
  };
  const stage = handler("octopus:stage-dropped-attachments"), pick = handler("octopus:client-attachments-select"), discard = handler("octopus:attachment-stages-discard"), event = { sender: {} };
  try {
    const source = path.join(root, "note.txt"), missing = path.join(root, "missing"); await fs.writeFile(source, "exact bytes");
    await assert.rejects(stage(event, "invalid", [source]), /no session/);
    live = false; await assert.rejects(stage(event, "owner", [source]), /active window/); live = true;
    const kept = (await stage(event, "owner", [source])).attachments[0];
    await assert.rejects(discard(event, "other", [kept]), /could not be discarded/);
    await assert.rejects(discard(event, "owner", [{ id: "unknown", payloadPath: source }]), /could not be discarded/);
    assert.equal(await fs.readFile(source, "utf8"), "exact bytes");
    const before = await fs.readdir(stagingDir);
    const open = fs.open;
    fs.open = async (name, ...args) => { if (name === source && args[0] === "r") throw new Error("read denied"); return open(name, ...args); };
    try { await assert.rejects(stage(event, "owner", [source]), /read denied/); }
    finally { fs.open = open; }
    await assert.rejects(stage(event, "owner", [source, missing]), /ENOENT/);
    selectedPaths = [source, missing]; await assert.rejects(pick(event, "owner", { mode: "files" }), /ENOENT/);
    assert.deepEqual(await fs.readdir(stagingDir), before);
    const removed = (await stage(event, "owner", [source])).attachments[0];
    const foreign = (await stage(event, "other", [source])).attachments[0];
    await assert.rejects(discard(event, "owner", [removed, foreign]), /could not be discarded/);
    await assert.rejects(store.consume("owner", removed), /unavailable/); await discard(event, "other", [foreign]);
    const payload = await store.consume("owner", kept); assert.equal(await fs.readFile(payload.payloadPath, "utf8"), "exact bytes"); await store.discard(payload);
    assert.deepEqual(await fs.readdir(stagingDir), []); assert.ok(sessions.includes("other"));
  } finally { await store.abortAll(); await fs.rm(root, { recursive: true, force: true }); }
});

test("picker and drop share bounded directory packaging, exact bytes and owned failure cleanup", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-attachment-drop-"));
  const store = createClientAttachmentStagingStore({ root: () => path.join(root, "stages") });
  const discarded = [];
  const writer = { stageFile: (meta, p) => store.stageFile("owner", meta, p), stageBytes: (meta, bytes) => store.stageBytes("owner", meta, bytes), discard: async (h) => { discarded.push(h.id); await store.discard(await store.consume("owner", h)); } };
  try {
    const regular = path.join(root, "binary.dat"), directory = path.join(root, "folder");
    await fs.writeFile(regular, Buffer.from([0, 255, 128, 1])); await fs.mkdir(directory);
    await fs.writeFile(path.join(directory, "note.txt"), "folder bytes"); await fs.symlink("../binary.dat", path.join(directory, "link"));
    const selected = await selectClientAttachments({ showOpenDialog: async () => ({ canceled: false, filePaths: [regular, directory] }) }, {}, { mode: "files_and_folders" }, writer);
    const dropped = await stageClientAttachmentPaths([regular, directory], writer);
    assert.deepEqual(selected.attachments.map((h) => h.contentHash), dropped.attachments.map((h) => h.contentHash));
    await assert.rejects(store.consume("other-owner", dropped.attachments[0]), /unavailable/);
    const payload = await store.consume("owner", dropped.attachments[1]);
    const manifest = JSON.parse(await fs.readFile(payload.payloadPath, "utf8"));
    assert.equal(manifest.schema, "octopus.client.directory_attachment");
    assert.deepEqual(manifest.entries.find((e) => e.kind === "symlink"), { kind: "symlink", path: "link", target: "../binary.dat" });
    assert.equal(manifest.entries.find((e) => e.path === "note.txt").content.text, "folder bytes");
    assert.equal((await fs.stat(payload.payloadPath)).mode & 0o777, 0o600); await store.discard(payload);
    const kept = await store.stageBytes("owner", { label: "operator.file", name: "kept", type: "text/plain", lastModified: 1 }, Buffer.from("keep"));
    await assert.rejects(stageClientAttachmentPaths([regular, path.join(root, "missing")], writer), /ENOENT/);
    assert.equal(discarded.length, 1); await store.discard(await store.consume("owner", kept));
    assert.deepEqual(await selectClientAttachments({ showOpenDialog: async () => ({ canceled: true, filePaths: [directory] }) }, {}, {}, writer), { canceled: true, attachments: [] });
    await assert.rejects(stageClientAttachmentPaths([directory], writer, "files"), /not a regular file/);
    await assert.rejects(stageClientAttachmentPaths(Array(13).fill(regular), writer), /up to 12/);
  } finally { await store.abortAll(); await fs.rm(root, { recursive: true, force: true }); }
});

test("actual draft hook rejects a stale drop after navigation and retains existing attachments on failure", async () => {
  const { JSDOM } = require("jsdom"), React = require("react");
  const dom = new JSDOM("<div id='root'></div>", { url: "http://localhost" });
  const old = new Map();
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    old.set(key, Object.getOwnPropertyDescriptor(globalThis, key)); Object.defineProperty(globalThis, key, { configurable: true, value, writable: true });
  }
  const storage = { loadChatSettings: () => ({}), loadChatDrafts: () => [], saveChatDrafts() {} };
  const { useChatDraftState } = compile("src/components/chat/useChatDraftState.ts", { "./storage": storage });
  const { createRoot } = require("react-dom/client"), root = createRoot(document.getElementById("root"));
  let state, runId = "a", resolveStage, rejectStage; const errors = [], discarded = [];
  window.octopusTransport = { stageDroppedAttachments: () => new Promise((resolve) => { resolveStage = resolve; }), discardClientAttachmentStages: async (h) => discarded.push(...h) };
  function Host() { state = useChatDraftState({ accessProfile: "live", storageScope: "test", activeRunId: runId, onError: (e) => errors.push(e), preserveSubmissionIntentOnEdit: () => false }); return null; }
  const render = () => React.act(async () => { root.render(React.createElement(Host)); });
  const fire = (files) => state.dropFiles({ dataTransfer: transfer(files), preventDefault() {} });
  try {
    await render(); await React.act(async () => { fire([folder]); });
    runId = "b"; await render(); await React.act(async () => { resolveStage({ canceled: false, attachments: [handle], fileIndices: [0] }); });
    assert.equal(state.attachments.length, 0); assert.deepEqual(discarded, [handle]);
    window.octopusTransport.stageDroppedAttachments = () => new Promise((_resolve, reject) => { rejectStage = reject; });
    await React.act(async () => { fire([folder]); }); runId = "c"; await render();
    await React.act(async () => { rejectStage(new Error("old draft rejection")); }); assert.deepEqual(errors, []);
    const original = file(); await React.act(async () => { state.onFilesPicked([original]); });
    window.octopusTransport.stageDroppedAttachments = async () => { throw new Error("rejected folder"); };
    await React.act(async () => { fire([folder, file("next.txt")]); });
    assert.equal(state.attachments.length, 1); assert.equal(state.attachments[0].file, original); assert.match(errors.at(-1), /rejected folder/);
    window.octopusTransport.stageDroppedAttachments = async () => ({ canceled: true, attachments: [], fileIndices: [] });
    await React.act(async () => { fire([folder]); }); assert.equal(state.attachments.length, 1);
    window.octopusTransport.stageDroppedAttachments = async () => ({ canceled: false, attachments: [handle], fileIndices: [0] });
    await React.act(async () => { fire([folder]); });
    assert.equal(state.attachments.length, 2); assert.equal(state.attachments[1].stage, handle);
  } finally {
    await React.act(async () => root.unmount()); dom.window.close();
    for (const [key, descriptor] of old) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; }
  }
});


test("selected binary attachments preserve exact cached manifest refs without JSON payload wrapping", async () => {
  const {materializeContentAddressedAttachment, prepareContentAddressedPayloadBindings, payloadArtifactBindings, readContentAddressedArtifactPayload} = require("../electron/contentAddressedArtifact.cjs");
  const {blake3} = require("@noble/hashes/blake3");
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-selected-payload-")));
  const store = createClientAttachmentStagingStore({root:() => path.join(root,"stages")});
  const blobsRoot = path.join(root,"blobs"), bytes = Buffer.from([0,255,128,1,13,10]);
  await fs.mkdir(blobsRoot, {mode:0o700}); let current = true;
  const revalidate = async () => { if (!current) throw new Error("client access ended"); };
  try {
    const handle = await store.stageBytes("owner", {name:"binary.dat",label:"operator.file",type:"application/octet-stream",lastModified:1}, bytes);
    const attachment = await store.consume("owner",handle);
    const reference = await materializeContentAddressedAttachment({blobsRoot,attachment,revalidate});
    assert.deepEqual(payloadArtifactBindings([reference]),[reference]);
    const manifestHash = Buffer.from(reference.content).toString("hex");
    assert.equal(reference.id, `operator.file:${manifestHash}`);
    const manifestBytes = await fs.readFile(path.join(blobsRoot,manifestHash));
    assert.deepEqual([...blake3(manifestBytes)],reference.content);
    const manifest = JSON.parse(manifestBytes);
    assert.deepEqual(Object.keys(manifest),["label","payload_path","payload_content_hash"]);
    assert.equal(manifest.payload_content_hash,Buffer.from(blake3(bytes)).toString("hex"));
    assert.deepEqual(await fs.readFile(manifest.payload_path),bytes);
    assert.deepEqual(await materializeContentAddressedAttachment({blobsRoot,attachment,revalidate}),reference);
    await assert.rejects(materializeContentAddressedAttachment({blobsRoot,attachment:{...attachment,contentHash:"f".repeat(64)},revalidate}),/captured stage/);
    await store.discard(attachment);
    const recovered = await readContentAddressedArtifactPayload({blobsRoot,manifestHash,artifactId:reference.id,label:"operator.file"});
    assert.deepEqual(recovered.payloadBytes,bytes);
    const nextHandle = await store.stageBytes("owner", {name:"next.dat",label:"operator.file",type:"application/octet-stream",lastModified:2}, Buffer.from([3,4,255]));
    const next = await store.consume("owner", nextHandle);
    const base = payloadArtifactBindings([reference]), preserved = JSON.stringify(base);
    const joined = await prepareContentAddressedPayloadBindings({blobsRoot, attachments:[next], payloadArtifactBindings:base, revalidate});
    assert.equal(JSON.stringify(base), preserved);
    assert.equal(joined.length, 2); assert.deepEqual(joined[0], reference);
    const nextRef = joined[1];
    assert.deepEqual(await prepareContentAddressedPayloadBindings({blobsRoot, attachments:[next], payloadArtifactBindings:[], revalidate}), [nextRef]);
    assert.deepEqual(await prepareContentAddressedPayloadBindings({blobsRoot, attachments:[next], payloadArtifactBindings:joined, revalidate}), joined);
    const changed = {...nextRef, content:nextRef.content.map(byte => byte ^ 1)};
    await assert.rejects(prepareContentAddressedPayloadBindings({blobsRoot, attachments:[next], payloadArtifactBindings:[changed], revalidate}), /exact artifact identity/);
    await store.discard(next);
    assert.throws(() => payloadArtifactBindings([reference,reference]),/duplicate/);
    assert.throws(() => payloadArtifactBindings([{...reference,content:"f".repeat(64)}]),/BLAKE3/);
    current = false;
    await assert.rejects(materializeContentAddressedAttachment({blobsRoot,attachment,revalidate}),/client access ended/);
    assert.equal((await fs.readdir(blobsRoot)).some(name => name.endsWith(".tmp")),false);
  } finally { await store.abortAll(); await fs.rm(root,{recursive:true,force:true}); }
});
