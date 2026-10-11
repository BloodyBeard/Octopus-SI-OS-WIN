const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const ts = require("typescript");
const React = require("react");
const { JSDOM } = require("jsdom");
const root = path.resolve(__dirname, "..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");
function load(file, dependencies, source = read(file)) {
  const module = { exports: {} };
  const code = ts.transpileModule(source, { fileName: file, compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true,
  } }).outputText;
  Function("require", "module", "exports", code)((id) => id.endsWith(".css") ? {} : dependencies[id] ?? require(id), module, module.exports);
  return module.exports;
}
const dom = new JSDOM("<div id='root'></div>");
for (const key of ["window", "document", "Node", "HTMLElement", "HTMLImageElement"]) global[key] = key === "document" ? dom.window.document : key === "window" ? dom.window : dom.window[key];
Object.defineProperty(global, "navigator", { configurable: true, value: dom.window.navigator });
global.ResizeObserver = class { observe() {} disconnect() {} };
global.IS_REACT_ACT_ENVIRONMENT = true;
const { createRoot } = require("react-dom/client");
const { createPortal } = require("react-dom");
const scope = { artifactId: "operator.input:manifest", runId: "run-a", interfaceId: "interface-a" };
function fixture(name, type, text) {
  const content = Buffer.from(text), hash = crypto.createHash("sha256").update(content).digest("hex");
  return { file: { id: hash, name, type, size: content.length, label: "operator.file", lastModified: 0 }, content,
    bytes: { artifact_id: scope.artifactId, run_id: scope.runId, interface_id: scope.interfaceId,
      attachment_id: hash, content_hash: hash, base64: content.toString("base64"), mime_type: type, size: content.length } };
}
const image = fixture("image.png", "image/png", "abc"), pdf = fixture("report.pdf", "application/pdf", "%PDF-1.7\nexact original bytes");
const { file, bytes } = image, hash = file.id;
let response = bytes;
const reads = [], errors = [], opens = [], saves = [], stagedReads = [];
const registry = load("src/components/artifacts/rendererRegistry.ts", {});
const presentation = load("src/components/chat/attachmentPresentation.ts", {
  "../artifacts/rendererRegistry": registry,
  "../../projectionClient": { readProjection: async (method, params) => { reads.push({ method, params }); return typeof response === "function" ? response(params) : response; } },
});
const toasts = { appToast: { danger: (message) => errors.push(message) } };
const fileOperations = load("src/components/artifacts/artifactFileOperations.ts", {
  "./rendererRegistry": registry,
  "./artifactResolver": { resolveArtifactPayload: () => { throw new Error("Attachment opening must read its admitted object, not resolve another path"); } },
});
const icons = load("src/components/chat/AttachmentIcon.tsx", { "./attachmentIcons.json": JSON.parse(read("src/components/chat/attachmentIcons.json")) });
const artifactPreview = load("src/components/artifacts/ArtifactPreview.tsx", {
  "./rendererRegistry": registry, "../IconGlyphs": { Glyph: () => null },
  "../chat/MessageContent": { MessageContent: ({ text }) => React.createElement("span", null, text) },
});
const filePreview = load("src/components/chat/ChatAttachmentFilePreview.tsx", {
  "../../design-system/components": {
    OcDialog: ({ children, ariaLabel }) => createPortal(React.createElement("div", { role: "dialog", "aria-modal": true, "aria-label": ariaLabel }, children), document.body),
    OcIconButton: ({ label, onClick }) => React.createElement("button", { "aria-label": label, onClick }, label),
  }, "../../toasts": toasts, "../artifacts/ArtifactPreview": artifactPreview, "../artifacts/artifactFileOperations": fileOperations,
});
const imagePreview = load("src/components/chat/ChatAttachmentImagePreview.tsx", {
  "../../design-system/modalFocus": load("src/design-system/modalFocus.ts", {}), "../../toasts": toasts,
  "../artifacts/artifactFileOperations": fileOperations, "./AttachmentIcon": icons,
});
let imageReads = 0, preview = presentation.readAttachmentImage;
const { ChatAttachmentCard, ChatAttachmentStrip } = load("src/components/chat/ChatAttachmentCard.tsx", {
  "./AttachmentIcon": icons, "./ChatAttachmentFilePreview": filePreview, "./ChatAttachmentImagePreview": imagePreview,
  "../../toasts": toasts, "../artifacts/artifactFileOperations": fileOperations,
  "./attachmentPresentation": { ...presentation, readAttachmentImage: (...args) => { imageReads++; return preview(...args); } },
});
const container = document.getElementById("root"), tree = createRoot(container);
const render = (props) => React.act(async () => tree.render(React.createElement(ChatAttachmentCard, props)));
const click = (selector, surface = document) => React.act(async () => { const element = surface.querySelector(selector); assert.ok(element, selector); element.click(); });
const created = [], revoked = [];
URL.createObjectURL = () => { const url = "blob:preview-" + created.length; created.push(url); return url; };
URL.revokeObjectURL = (url) => revoked.push(url);
window.octopusTransport = {
  readClientAttachmentPreview: async (request) => { stagedReads.push(request); return { base64: bytes.base64, mimeType: file.type, contentHash: hash, size: file.size }; },
  openClientFile: async (request) => { opens.push(request); }, saveClientFile: async (request) => { saves.push(request); },
};
(async () => {
  assert.equal(await (await presentation.readAttachmentImage(file, scope)).text(), "abc");
  assert.deepEqual(reads[0], { method: "projection.artifact.bytes.read", params: {
    artifact_id: scope.artifactId, run_id: scope.runId, interface_id: scope.interfaceId, attachment_id: hash,
  } });
  for (const change of [{ attachment_id: "private-file" }, { run_id: "other" }, { interface_id: "other" },
    { artifact_id: "other" }, { content_hash: "b".repeat(64) }, { size: 2 }, { base64: "YQ==" }, { mime_type: "image/svg+xml" }]) {
    response = { ...bytes, ...change }; await assert.rejects(presentation.readAttachmentImage(file, scope));
  }
  const before = reads.length;
  await assert.rejects(presentation.readAttachmentImage(file, { ...scope, runId: "" }));
  await assert.rejects(presentation.readAttachmentImage({ ...file, type: "image/svg+xml" }, scope));
  assert.equal(reads.length, before);
  const local = new Blob([image.content], { type: file.type });
  assert.equal(await presentation.readAttachmentImage({ ...file, file: local }), local);
  await assert.rejects(presentation.readAttachmentBytes({ ...file, file: new Blob(["wrong size"], { type: file.type }) }));
  const staged = { ...file, stage: { ...file, id: "owned-stage", contentHash: hash } };
  assert.equal(await (await presentation.readAttachmentImage(staged)).text(), "abc");
  assert.deepEqual(stagedReads, [{ id: "owned-stage", contentHash: hash }]);
  response = bytes; await presentation.readAttachmentImage(staged, scope);
  assert.equal(stagedReads.length, 1, "sent attachments use admission, not staging");

  await render({ attachment: file, scope });
  const trigger = container.querySelector("button"); trigger.focus();
  await click('button[aria-label="View image.png"]', container);
  for (let index = 0; index < 8; index++) await render({ attachment: { ...staged, stage: { ...staged.stage } }, scope: { ...scope, text: "stream " + index } });
  assert.equal(imageReads, 1, "stream updates and fresh metadata do not reload immutable bytes");
  assert.equal(document.querySelectorAll("img").length, 2, "portal preview survives stream updates");
  assert.equal(container.querySelector("img").src, created[0]); assert.deepEqual(revoked, []);
  const expanded = document.querySelector(".chat-attachment-lightbox img"), viewport = document.querySelector(".chat-attachment-lightbox-viewport");
  Object.defineProperties(expanded, { naturalWidth: { value: 1000 }, naturalHeight: { value: 500 } });
  viewport.getBoundingClientRect = () => ({ left: 0, top: 0, right: 800, bottom: 400, width: 800, height: 400 });
  await React.act(async () => expanded.dispatchEvent(new window.Event("load")));
  assert.equal(expanded.style.width, "800px");
  await click('button[aria-label="Zoom in image"]'); assert.equal(expanded.style.width, "900px");
  await React.act(async () => viewport.dispatchEvent(new window.WheelEvent("wheel", { ctrlKey: true, deltaY: -200, bubbles: true, cancelable: true })));
  assert.ok(parseFloat(expanded.style.width) > 2000);
  await React.act(async () => expanded.dispatchEvent(new window.KeyboardEvent("keydown", { key: "0", metaKey: true, bubbles: true })));
  assert.equal(expanded.style.width, "800px");
  await click('button[aria-label="Download image"]'); assert.deepEqual(Buffer.from(saves.at(-1).bytes), image.content); assert.equal(saves.at(-1).filename, file.name);
  await React.act(async () => expanded.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  assert.equal(document.querySelector('[role="dialog"]'), null); assert.equal(document.activeElement, trigger);

  let pending, removals = 0;
  preview = () => new Promise((resolve) => { pending = resolve; });
  await render({ attachment: file, scope: { ...scope, runId: "run-b" } });
  assert.equal(container.querySelector("img"), null, "changed admission retires the old bytes immediately");
  await render({ attachment: { ...file }, scope: { ...scope, runId: "run-b" } });
  assert.equal(imageReads, 2, "stream updates preserve an in-flight read"); assert.deepEqual(revoked, [created[0]]);
  await render({ attachment: pdf.file, onRemove: () => removals++ });
  await React.act(async () => pending(local)); assert.equal(created.length, 1, "retired reads cannot create URLs");
  assert.equal(container.textContent, "report.pdfPDF");
  assert.equal(container.querySelector(".chat-attachment-file-icon svg").getAttribute("width"), "24");
  await click('button[aria-label="Remove report.pdf"]', container); assert.equal(removals, 1);
  const beforePending = imageReads;
  await render({ attachment: staged, scope: { ...scope, artifactId: undefined, pending: true } });
  assert.equal(imageReads, beforePending); assert.equal(container.querySelector('[role="status"]').getAttribute("aria-label"), "image.png: Preparing image");
  preview = async () => { throw new Error("Not admitted"); }; await render({ attachment: file, scope });
  assert.equal(container.textContent, "Failed"); assert.equal(container.querySelector("img"), null);
  const beforeFailure = imageReads;
  for (let index = 0; index < 8; index++) await render({ attachment: { ...file }, scope: { ...scope } });
  assert.equal(imageReads, beforeFailure, "failed previews do not poll or retry"); assert.equal(container.querySelector(".chat-attachment-retry"), null);
  preview = async () => local; await render({ attachment: file, scope: { ...scope, artifactId: "changed" } });
  await React.act(async () => container.querySelector("img").dispatchEvent(new window.Event("error")));
  assert.match(container.querySelector(".chat-attachment-card").title, /could not be decoded/);
  const decodeReads = imageReads; await render({ attachment: { ...file }, scope: { ...scope, artifactId: "changed" } }); assert.equal(imageReads, decodeReads);
  await render({ attachment: file, scope: { ...scope, interfaceId: "changed" } }); assert.ok(container.querySelector("img"));
  await render({ attachment: { ...file, file: local } }); const localReads = imageReads;
  await render({ attachment: { ...file, file: local } }); assert.equal(imageReads, localReads);
  await render({ attachment: { ...file, file: new Blob(["new"], { type: file.type }) } }); assert.equal(imageReads, localReads + 1);
  await render({ attachment: staged }); const stageReads = imageReads;
  await render({ attachment: { ...staged, stage: { ...staged.stage } } }); assert.equal(imageReads, stageReads);
  await render({ attachment: { ...staged, stage: { ...staged.stage, id: "another-stage" } } }); assert.equal(imageReads, stageReads + 1);
  preview = presentation.readAttachmentImage;

  response = pdf.bytes; await render({ attachment: pdf.file, scope }); assert.equal(container.textContent, "report.pdf");
  const fileRead = reads.length; await click('button[aria-label="report.pdf"]', container);
  assert.equal(reads.length, fileRead + 1); assert.equal(reads.at(-1).params.attachment_id, pdf.file.id);
  assert.equal(document.querySelector('object[type="application/pdf"]').getAttribute("data"), "data:application/pdf;base64," + pdf.bytes.base64);
  for (let index = 0; index < 3; index++) await render({ attachment: { ...pdf.file }, scope: { ...scope } });
  assert.equal(reads.length, fileRead + 1); assert.ok(document.querySelector('object[type="application/pdf"]'));
  await click('button[aria-label="Open file"]'); assert.deepEqual(Buffer.from(opens.at(-1).bytes), pdf.content);
  await click('button[aria-label="Close preview"]');
  for (const data of [fixture("notes.txt", "text/plain", "original line\nsecond line"), fixture("invalid.json", "application/json", '{"incomplete":')]) {
    response = data.bytes; await render({ attachment: data.file, scope }); await click(".chat-attachment-file-button", container);
    assert.equal([...document.querySelectorAll(".artifact-line-content")].map((line) => line.textContent).join("\n"), data.content.toString());
    await click('button[aria-label="Open file"]'); assert.deepEqual(Buffer.from(opens.at(-1).bytes), data.content);
    await click('button[aria-label="Close preview"]');
  }
  const notebook = fixture("broken.ipynb", "application/json", "{invalid notebook");
  response = notebook.bytes; await render({ attachment: notebook.file, scope }); await click(".chat-attachment-file-button", container);
  assert.match(document.querySelector('[role="alert"]').textContent, /Preview unavailable/);
  await click('button[aria-label="Open file"]'); assert.deepEqual(Buffer.from(opens.at(-1).bytes), notebook.content);
  await click('button[aria-label="Close preview"]');
  const office = fixture("original.docx", "application/octet-stream", "original office binary");
  response = office.bytes; await render({ attachment: office.file, scope }); await click(".chat-attachment-file-button", container);
  assert.equal(document.querySelector('[role="dialog"]'), null); assert.deepEqual(Buffer.from(opens.at(-1).bytes), office.content);
  response = new Promise((resolve) => { pending = resolve; });
  await render({ attachment: pdf.file, scope }); await click(".chat-attachment-file-button", container);
  const openCount = opens.length; await render({ attachment: office.file, scope: { ...scope, runId: "run-b" } });
  await React.act(async () => pending(pdf.bytes));
  assert.equal(document.querySelector('[role="dialog"]'), null); assert.equal(opens.length, openCount, "retired file reads cannot open another object");

  const messageSource = ts.createSourceFile("messages.tsx", read("src/components/chat/ChatMessageList.tsx"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const functions = messageSource.statements.filter((node) => ts.isFunctionDeclaration(node) && ["UserMessage", "MessageAttachments"].includes(node.name?.text)).map((node) => node.getText(messageSource)).join("\n");
  const layout = load("src/components/chat/ChatMessageList.tsx", {
    "./ChatAttachmentCard": { ChatAttachmentStrip },
    "layout-surfaces": { SelectableMessageText: ({ children }) => children, CollapsibleMessageBody: ({ children }) => children,
      MessageContent: ({ text }) => React.createElement("span", null, text), MessageActions: () => null },
  }, 'import { useEffect, useState } from "react"; import { ChatAttachmentStrip } from "./ChatAttachmentCard"; import { SelectableMessageText, CollapsibleMessageBody, MessageContent, MessageActions } from "layout-surfaces";\n' + functions + '\nexport { UserMessage };');
  const gallery = [image, fixture("second.png", "image/png", "def"), fixture("third.png", "image/png", "ghi")];
  const docs = Array.from({ length: 5 }, (_, index) => fixture("file" + index + ".txt", "text/plain", "text" + index));
  const objects = new Map([...gallery, ...docs].map((data) => [data.file.id, data.bytes])); response = (params) => objects.get(params.attachment_id);
  const message = { ...scope, id: "message", role: "user", text: "Attached message", attachments: [...gallery, ...docs].map((data) => data.file) };
  const renderMessage = (text) => React.act(async () => tree.render(React.createElement(layout.UserMessage, { message: { ...message, text } })));
  await renderMessage(message.text);
  const attachments = container.querySelector(".octopus-message-attachments.is-user"), bubble = container.querySelector("[data-user-message-bubble]");
  assert.equal(attachments.closest("[data-user-message-bubble]"), null); assert.ok(attachments.compareDocumentPosition(bubble) & Node.DOCUMENT_POSITION_FOLLOWING);
  assert.equal(attachments.querySelectorAll(".chat-attachment-card").length, 8); assert.equal(attachments.querySelectorAll(".chat-attachment-scroll").length, 2);
  const galleryReads = imageReads; await click('button[aria-label="View image.png"]', container); await click('button[aria-label="Next image"]');
  assert.equal(document.querySelector(".chat-attachment-lightbox img").alt, "second.png"); assert.equal(imageReads, galleryReads);
  await React.act(async () => document.querySelector(".chat-attachment-lightbox").dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true })));
  assert.equal(document.querySelector(".chat-attachment-lightbox img").alt, "image.png"); await click('button[aria-label="Close image preview"]');
  await renderMessage(""); assert.equal(container.querySelector("[data-user-message-bubble]"), null); assert.equal(container.querySelectorAll(".chat-attachment-card").length, 8);
  await React.act(async () => tree.render(React.createElement(ChatAttachmentStrip, { attachments: [{ ...file, file: local }, { ...pdf.file, file: new Blob([pdf.content], { type: pdf.file.type }) }], onRemove: () => {} })));
  assert.ok(container.querySelector(".is-image.is-compact")); assert.ok(container.querySelector(".is-file-card")); assert.equal(container.querySelectorAll(".chat-attachment-scroll").length, 1);
  await React.act(async () => tree.unmount());
  assert.deepEqual(revoked.slice().sort(), created.slice().sort(), "every URL is revoked once"); assert.deepEqual(errors, []);
  dom.window.close();
  console.log("PASS exact admitted/staged bytes, stable preview lifetime, finite error states, source-derived image/file rows, gallery/zoom/download, PDF/text/JSON renderers, original Open after notebook error, Office native Open, removal, and stale-read disposal");
})().catch((error) => { console.error(error); process.exitCode = 1; });
