const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");

const filename = path.resolve(__dirname, "../src/airHandEngine.ts");
const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), {
  fileName: filename,
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const engineModule = { exports: {} };
Function("module", "exports", "require", code)(engineModule, engineModule.exports, require);
const { AirHandGestureEngine } = engineModule.exports;

const FINGERS = [[5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16], [17, 18, 19, 20]];
const MCP_X = { 5: -0.45, 9: -0.15, 13: 0.15, 17: 0.42 };
const EXTENDED = { open: [true, true, true, true], v: [true, true, false, false], aim: [true, false, false, false] };

function hand(cx, cy, pose) {
  const landmarks = new Array(21);
  const at = (index, x, y) => { landmarks[index] = { x: cx + x * 0.1, y: cy + y * 0.1, z: 0 }; };
  at(0, 0, 1);
  FINGERS.forEach(([mcp, pip, dip, tip], finger) => {
    const x = MCP_X[mcp];
    at(mcp, x, 0);
    const open = EXTENDED[pose][finger];
    at(pip, x, open ? -0.45 : -0.35);
    at(dip, x, open ? -0.75 : -0.15);
    at(tip, x, open ? -1 : 0.05);
  });
  at(1, -0.45, 0.75);
  at(2, -0.75, 0.55);
  at(3, -1, 0.35);
  at(4, -1.25, 0.15);
  return landmarks;
}

function events(frames) {
  const engine = new AirHandGestureEngine();
  const options = { mirrorX: true, sensitivity: "balanced" };
  const fired = [];
  let now = 0;
  for (const frame of frames) {
    now += frame.dt ?? 33;
    const event = frame.lost ? engine.lost(now, options) : engine.process(hand(frame.x, frame.y ?? 0.5, frame.pose ?? "open"), now, options).event;
    if (event) fired.push(event);
  }
  return fired;
}

const still = (x, count, pose) => Array.from({ length: count }, () => ({ x, pose }));
const move = (from, to, count, pose, dt) => Array.from({ length: count }, (_, index) => ({ x: from + (to - from) * ((index + 1) / count), pose, dt }));

test("a quick open-hand or two-finger flick is one swipe in the mirrored direction", () => {
  assert.deepEqual(events([...still(0.35, 4, "open"), ...move(0.35, 0.65, 10, "open")]), ["swipe_left"]);
  assert.deepEqual(events([...still(0.65, 4, "v"), ...move(0.65, 0.35, 10, "v")]), ["swipe_right"]);
});

test("frames lost to motion blur or the frame edge do not drop the swipe", () => {
  assert.deepEqual(events([...still(0.35, 4, "open"), ...move(0.35, 0.45, 3, "open"), { lost: true }, { lost: true }, ...move(0.5, 0.65, 4, "open")]), ["swipe_left"]);
  assert.deepEqual(events([...still(0.4, 4, "open"), ...move(0.4, 0.475, 3, "open"), { lost: true }, { lost: true }]), ["swipe_left"]);
});

test("slow drift, the return stroke, and pointer movement are not swipes", () => {
  assert.deepEqual(events(move(0.3, 0.7, 60, "open")), []);
  assert.deepEqual(events([...still(0.35, 4, "open"), ...move(0.35, 0.65, 8, "open"), ...move(0.65, 0.35, 8, "open")]), ["swipe_left"]);
  assert.deepEqual(events([...still(0.35, 4, "aim"), ...move(0.35, 0.65, 10, "aim")]), []);
});
