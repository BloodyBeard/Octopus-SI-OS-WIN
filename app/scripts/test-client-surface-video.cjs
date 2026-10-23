const assert = require("node:assert/strict");
const path = require("node:path");
const { test } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

test("native video renders controls, replaces its source and exposes media failures", async () => {
  const dom = new JSDOM('<div id="root"></div>', { url: "https://fixture.invalid" });
  for (const key of ["window", "document", "HTMLElement", "Event", "navigator"]) {
    Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? dom.window : dom.window[key] });
  }
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  const React = require("react"), { createRoot } = require("react-dom/client");
  const built = await build({ entryPoints: [path.join(__dirname, "../src/client-surfaces/ClientSurfaceVideo.tsx")],
    bundle: true, platform: "node", format: "cjs", write: false, loader: { ".css": "empty" }, external: ["react", "react/jsx-runtime"] });
  const module = { exports: {} };
  Function("require", "module", "exports", built.outputFiles[0].text)(require, module, module.exports);
  const { ClientSurfaceVideo } = module.exports;
  const root = createRoot(document.getElementById("root"));
  const render = source => React.act(async () => root.render(React.createElement(ClientSurfaceVideo, { key: source, source, title: "Clip" })));
  try {
    await render("https://media.example/one.mp4");
    const first = document.querySelector("video");
    assert.equal(first.controls, true); assert.equal(first.autoplay, false);
    assert.equal(first.preload, "metadata"); assert.equal(first.crossOrigin, "anonymous");
    await React.act(async () => first.dispatchEvent(new Event("error")));
    assert.match(document.querySelector('[role="alert"]').textContent, /could not be loaded/);
    await render("https://media.example/two.mp4");
    assert.notEqual(document.querySelector("video"), first);
    assert.equal(document.querySelector("video").src, "https://media.example/two.mp4");
    assert.equal(document.querySelector('[role="alert"]'), null);
    await render("file:///private/clip.mp4");
    assert.equal(document.querySelector("video"), null);
    assert.match(document.querySelector('[role="alert"]').textContent, /HTTPS/);
  } finally { await React.act(async () => root.unmount()); dom.window.close(); }
});
