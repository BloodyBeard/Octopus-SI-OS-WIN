const fs = require("node:fs");
const path = require("node:path");

const TRACE_FOLDER = "gesture-traces";
const TRACE_FORMAT = "octopus.gesture-trace";
const TRACE_VERSION = 1;
const MAX_TRACE_BYTES = 64 * 1024 * 1024;

async function saveGestureTrace({ root, text, now = new Date() }) {
  if (typeof text !== "string" || !text) throw new Error("The gesture trace is empty.");
  const bytes = Buffer.byteLength(text);
  if (bytes > MAX_TRACE_BYTES) throw new Error("The gesture trace is too large to save.");
  let trace;
  try {
    trace = JSON.parse(text);
  } catch {
    throw new Error("The gesture trace is not valid JSON.");
  }
  if (trace?.format !== TRACE_FORMAT || trace?.version !== TRACE_VERSION || !Array.isArray(trace?.frames)) {
    throw new Error(`The gesture trace is not ${TRACE_FORMAT} v${TRACE_VERSION}.`);
  }
  const folder = path.join(root, TRACE_FOLDER);
  await fs.promises.mkdir(folder, { recursive: true, mode: 0o700 });
  const stamp = now.toISOString().replace(/[:.]/gu, "-");
  for (let suffix = 1; suffix <= 100; suffix += 1) {
    const filename = suffix === 1 ? `gesture-trace-${stamp}.json` : `gesture-trace-${stamp}-${suffix}.json`;
    const target = path.join(folder, filename);
    try {
      await fs.promises.writeFile(target, text, { flag: "wx", mode: 0o600 });
      return { path: target, filename, bytes };
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
    }
  }
  throw new Error("No free gesture trace filename is available.");
}

function revealGestureTrace({ root, target, shell }) {
  const folder = path.resolve(root, TRACE_FOLDER);
  const resolved = path.resolve(typeof target === "string" ? target : "");
  if (path.dirname(resolved) !== folder || path.extname(resolved) !== ".json") {
    throw new Error("Only saved gesture traces can be revealed.");
  }
  if (!fs.existsSync(resolved)) throw new Error("The gesture trace no longer exists.");
  shell.showItemInFolder(resolved);
  return { revealed: true, path: resolved };
}

module.exports = { revealGestureTrace, saveGestureTrace };
