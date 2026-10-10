import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { JSDOM } from "jsdom";
import ts from "typescript";
const dom = new JSDOM("<!doctype html><body></body>");
const NativeMessageChannel = globalThis.MessageChannel, channels = [];
globalThis.MessageChannel = class extends NativeMessageChannel { constructor() { super(); channels.push(this); } };
try {
const app = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const shell = ts.createSourceFile("ChatView.tsx", fs.readFileSync(path.join(app, "src/components/ChatView.tsx"), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const panel = shell.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "PinnedContextPanel").getText(shell);
const bundle = await build({ stdin: { contents: `import {useState} from 'react'; import {markdownToPlainText} from './src/components/chat/markdownPresentation'; const motion={aside:'aside'}; const Glyph=()=>null; export ${panel}; export * from './src/components/chat/markdownPresentation'; export {MessageContent} from './src/components/chat/MessageContent'; export {createElement} from 'react'; export {renderToStaticMarkup} from 'react-dom/server';`, resolveDir: app, loader: "tsx" }, bundle: true, jsx: "automatic", platform: "browser", format: "esm", write: false, loader: { ".css": "empty" }, define: { "process.env.NODE_ENV": '"production"' }, logLevel: "silent" });
for (const key of ["window", "document", "navigator", "HTMLElement", "Element", "Node", "DOMParser"]) Object.defineProperty(globalThis, key, { value: dom.window[key], configurable: true });
const { prepareStreamingMarkdown: prepare, markdownToPlainText: plain, MessageContent, PinnedContextPanel, createElement: h, renderToStaticMarkup: render } = await import(`data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString("base64")}`);
const html = (text, streaming = true) => render(h(MessageContent, { text, streaming }));
const content = (text, streaming = true) => { dom.window.document.body.innerHTML = html(text, streaming); return dom.window.document.body; };
for (const [source, completed] of [["**hello", "**hello**"], ["**hello*", "**hello**"], ["*hello", "*hello*"], ["***hello", "***hello***"], ["**outer *inner", "**outer *inner***"], ["**hello  ", "**hello**  "], ["> **hello", "> **hello**"], ["- **hello", "- **hello**"]]) assert.equal(prepare(source), completed);
for (const source of ["**", "*", "** leading", "**hello\nnext", "**hello\r\nnext", "2*3", "a*b", "2 ** 3", "* item\n* next", "***", "\\*literal\\*", "\\**literal", "`**literal`", "`` **literal ``", "```js\n**literal", "~~~\n**literal", "    **literal", "**hello `code", "$2*3$", "[link](https://example.test/a*b)", "![**alt**](image.png)", "<span title='**literal'>", "**complete**", "*complete*", "__complete__"]) assert.equal(prepare(source), source, source);
for (const final of ["**Streaming text**", "*Streaming text*", "***Streaming text***", "**outer *inner***", "**escaped \\* star**", "**text `a*b`**"]) {
  const markerLength = final.match(/^\*+/)[0].length;
  for (let end = markerLength + 1; end <= final.length; end += 1) {
    const prefix = final.slice(0, end);
    if (prefix.includes("`") && !prefix.includes("`a*b`") || /\s\*+$/.test(prefix) || prefix.endsWith("\\")) continue;
    const body = content(prefix);
    assert.ok(body.querySelector("strong, em"), prefix);
    assert.ok(!body.textContent.startsWith("**"), prefix);
  }
  assert.equal(prepare(final), final);
  assert.equal(content(final, false).querySelector("strong, em")?.textContent, plain(final));
}
assert.equal(content("**unfinished", false).textContent, "**unfinished");
assert.equal(content("**with  spaces\t", true).textContent, "with  spaces");
assert.equal(prepare("`**code` then **bold"), "`**code` then **bold**");
assert.equal(content("\\**literal", true).textContent, "**literal");
assert.equal(content("`a*b`", true).querySelector("code").textContent, "a*b");
assert.equal(content("* item\n* next", true).querySelectorAll("li").length, 2);
for (const source of ["**outer *", "**escape\\", "`**literal", "``**literal", "![**alt](image.png)", "[**label](target)", "a | b\n--- | ---\nx | **y |", "**a\n\nnext"]) assert.equal(prepare(source), source, source);
for (const [source, expected] of [["**Pinned** and *nested*", "Pinned and nested"], ["[**label**](https://example.test)", "label"], ["`**literal**` and \\*star\\*", "**literal** and *star*"], ["a*b and 2 * 3", "a*b and 2 * 3"], ["# Heading\n\n- one\n- two", "Heading\none\ntwo"], ["A  B\tC", "A  B\tC"], ["![image alt](image.png)", "image alt"], ["~~old~~ &amp; new", "old & new"], ["[**ref**][id]\n\n[id]: https://example.test", "ref"], ["a | b\n--- | ---\nx | y", "a\tb\nx\ty"]]) assert.equal(plain(source), expected);
const message = Object.freeze({ id: "m", artifactId: "a", text: "**Pinned** message", role: "assistant" });
const selection = Object.freeze({ artifactId: "s", sourceArtifactId: "a", sourceMessageId: "m", text: "**literal** selection", start: 0, end: 21, role: "assistant" });
dom.window.document.body.innerHTML = render(h(PinnedContextPanel, { messages: [message], items: [selection], rightInset: 0 }));
assert.deepEqual([...dom.window.document.querySelectorAll(".chat-pinned-row-label")].map((node) => node.textContent), ["**literal** selection", "Pinned message"]);
assert.equal(message.text, "**Pinned** message");
assert.equal(selection.text, "**literal** selection");
console.log("PASS markdown completion, stream prefixes, literal/code/list/math guards, final renderer, and pinned JSX text with raw storage preserved");
} catch (error) { console.error(`${error.name}: ${error.message}`); process.exitCode = 1; }
finally { dom.window.close(); for (const channel of channels) { channel.port1.close(); channel.port2.close(); } globalThis.MessageChannel = NativeMessageChannel; }
