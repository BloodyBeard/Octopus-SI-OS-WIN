'use strict';

const crypto = require('node:crypto').webcrypto;

const Yv = "codex-agent-overlay-root";

function Ah(e, t) { return e === "ControlOrMeta" ? Xl(t) ? "Meta" : "Control" : e; }

function to(e, t) { if (!e)
    return 0; let r = 0; for (let n of e)
    switch (Ah(n, t)) {
        case "Alt":
            r |= 1;
            break;
        case "Control":
            r |= 2;
            break;
        case "Meta":
            r |= 4;
            break;
        case "Shift":
            r |= 8;
            break;
    } return r; }

function Xl(e) { return e === "darwin"; }

function H(e) { let t = Number(e); if (!Number.isInteger(t) || t <= 0)
    throw new Error("Expected a positive integer"); return t; }

function Me(e) { let t = typeof e.timeout_ms == "number" ? e.timeout_ms : 3e3; return Math.min(Math.max(0, t), e.max || 3e3); }

function KU(e, t, r) { let n = { x: r[0] ?? 0, y: r[1] ?? 0 }, o = { x: r[2] ?? 0, y: r[3] ?? 0 }, i = { x: r[6] ?? 0, y: r[7] ?? 0 }, s = e.x / t.width, a = e.y / t.height; return { x: n.x + s * (o.x - n.x) + a * (i.x - n.x), y: n.y + s * (o.y - n.y) + a * (i.y - n.y) }; }

function ro(e, t, r) { if (r == null || r.length < 8)
    return null; if (t != null)
    return KU(e, t, r); let n = Jl(r); return n == null ? null : { x: n.left + e.x, y: n.top + e.y }; }

function Jl(e) { if (e == null || e.length < 8)
    return null; let t = [e[0], e[2], e[4], e[6]].filter(n => n != null), r = [e[1], e[3], e[5], e[7]].filter(n => n != null); return t.length !== 4 || r.length !== 4 ? null : { bottom: Math.max(...r), left: Math.min(...t), right: Math.max(...t), top: Math.min(...r) }; }

const XU = "__codexIabExpectedInputTargetToken";

function Yl(e, t) { return t == null ? e : { ...e, [XU]: t }; }

const Rh = "Native clipboard shortcuts are disabled; use Browser Use virtual clipboard commands instead.";

async function ec(e, t, r, n = {}) { let o = new Zl(e, bx(t), n.iabInputTargetToken, n.deadlineMs), i = Array.isArray(r) ? r.flatMap(Ql) : Ql(r), s = wx(i), a = s[s.length - 1]; if (!a)
    throw new Error("keypress requires at least one key"); if (yx(s, e.platform) != null)
    throw new Error(Rh); for (let l of s.slice(0, -1))
    await o.down(l); await o.down(a), await o.up(a); for (let l of s.slice(0, -1).reverse())
    await o.up(l); }

async function hx(e, t, r, n = {}) { if (Array.from(r).length !== 1)
    throw new Error("dispatchTextCharacter requires one Unicode character"); let o = new Zl(e, bx(t), n.iabInputTargetToken, n.deadlineMs); if (!kh.has(r)) {
    await o.insertText(r);
    return;
} await o.down(r), await o.up(r); }

function gx(e, t = "linux") { let r = Array.isArray(e) ? e.flatMap(Ql) : Ql(e); return yx(wx(r), t); }

const Zl = class {
    constructor(t, r, n, o) { this.cdp = t; this.target = r; this.iabInputTargetToken = n; this.deadlineMs = o; }
    cdp;
    target;
    iabInputTargetToken;
    deadlineMs;
    pressedModifiers = new Set;
    async down(t) { let r = Ih(t, this.pressedModifiers, this.cdp.platform), n = Ph(r.key); n && this.pressedModifiers.add(n); let o = this.keyText(r), i = ZU(r.code, this.pressedModifiers, this.cdp.platform); await this.callInputDispatch(Yl({ type: o ? "keyDown" : "rawKeyDown", modifiers: to(this.pressedModifiers, this.cdp.platform), windowsVirtualKeyCode: r.keyCodeWithoutLocation, code: r.code, ...i.length > 0 ? { commands: i } : {}, key: r.key, text: o, unmodifiedText: o, location: r.location, isKeypad: r.location === Ke }, this.iabInputTargetToken)); }
    async up(t) { let r = Ih(t, this.pressedModifiers, this.cdp.platform), n = Ph(r.key); n && this.pressedModifiers.delete(n), await this.callInputDispatch(Yl({ type: "keyUp", modifiers: to(this.pressedModifiers, this.cdp.platform), windowsVirtualKeyCode: r.keyCodeWithoutLocation, code: r.code, key: r.key, location: r.location, isKeypad: r.location === Ke }, this.iabInputTargetToken)); }
    async insertText(t) { let r = Yl({ text: t }, this.iabInputTargetToken); if (this.target.sessionId == null && this.target.targetId == null) {
        await this.cdp.call(this.target.tabId, "Input.insertText", r, { deadlineMs: this.deadlineMs });
        return;
    } await this.cdp.callTarget(this.target, "Input.insertText", r, { deadlineMs: this.deadlineMs }); }
    keyText(t) { return this.pressedModifiers.size === 0 || this.pressedModifiers.size === 1 && this.pressedModifiers.has("Shift") ? t.text : ""; }
    async callInputDispatch(t) { if (this.target.sessionId == null && this.target.targetId == null) {
        await this.cdp.call(this.target.tabId, "Input.dispatchKeyEvent", t, { deadlineMs: this.deadlineMs });
        return;
    } await this.cdp.callTarget(this.target, "Input.dispatchKeyEvent", t, { deadlineMs: this.deadlineMs }); }
};

function bx(e) { return typeof e == "object" ? e : { tabId: e }; }

function yx(e, t) { let r = new Set, n = [], o = !1; for (let s of e) {
    let a = Ih(s, r, t), l = Ph(a.key);
    l ? r.add(l) : (n.push(a.code), fx(a.code, r) && (o = !0));
} let i = n.length === 1 ? n[0] : void 0; if (i != null) {
    let s = QU(i, r);
    if (s != null)
        return s;
    if (fx(i, r))
        return "blocked";
} return o ? "blocked" : null; }

function Ih(e, t, r) { let n = Ah(e, r), o = kh.get(n); if (!o)
    throw new Error(`Unknown key: "${e}"`); return t.has("Shift") && o.shifted ? o.shifted : o; }

function Ql(e) { let t = [], r = ""; for (let n of e)
    n === "+" && r ? (t.push(r), r = "") : r += n; return r && t.push(r), t; }

function wx(e) { let t = o3.get(YU(e)); return t || e.map(JU); }

function JU(e) { return kh.has(e) ? e : n3.get(e.toLowerCase().replaceAll("_", "")) ?? e; }

function YU(e) { return e.map(t => t.toLowerCase()).join("+"); }

function Ph(e) { switch (e) {
    case "Alt":
    case "Control":
    case "Meta":
    case "Shift": return e;
    default: return;
} }

function ZU(e, t, r) { if (e === "KeyA" && t.size === 1 && t.has(Xl(r) ? "Meta" : "Control"))
    return ["selectAll"]; if (!Xl(r))
    return []; let o = [...e3.filter(s => t.has(s)), e].join("+"), i = r3.get(o); return i ? [i] : []; }

function fx(e, t) { return (e === "KeyC" || e === "KeyX") && t.size === 2 && t.has("Shift") && (t.has("Meta") || t.has("Control")) ? !1 : (t.has("Meta") || t.has("Control")) && t3.has(e) || e === "Insert" && (t.has("Control") || t.has("Shift")) || e === "Delete" && t.has("Shift"); }

function QU(e, t) { if (t.size === 1) {
    if (e === "Insert" && t.has("Control"))
        return "copy";
    if (e === "Insert" && t.has("Shift"))
        return "paste";
    if (e === "Delete" && t.has("Shift"))
        return "cut";
} let r = (t.has("Meta") || t.has("Control")) && !(t.has("Meta") && t.has("Control")), n = t.size === 1 || t.size === 2 && t.has("Shift"); if (!r || !n)
    return null; switch (e) {
    case "KeyC": return t.has("Shift") ? null : "copy";
    case "KeyV": return t.has("Shift") ? "paste-plain-text" : "paste";
    case "KeyX": return t.has("Shift") ? null : "cut";
    default: return null;
} }

const Ke = 3;

const e3 = ["Shift", "Control", "Alt", "Meta"];

const t3 = new Set(["KeyC", "KeyV", "KeyX"]);

const r3 = new Map([["Backspace", "deleteBackward"], ["Delete", "deleteForward"], ["Meta+KeyZ", "undo"], ["Shift+Meta+KeyZ", "redo"]]);

const n3 = new Map([["alt", "Alt"], ["option", "Alt"], ["control", "Control"], ["controlormeta", "ControlOrMeta"], ["ctrl", "ControlOrMeta"], ["cmd", "Meta"], ["command", "Meta"], ["meta", "Meta"], ["win", "Meta"], ["super", "Meta"], ["shift", "Shift"], ["capslock", "CapsLock"], ["shiftlock", "CapsLock"], ["esc", "Escape"], ["escape", "Escape"], ["enter", "Enter"], ["return", "Enter"], ["linefeed", "Enter"], ["backspace", "Backspace"], ["delete", "Delete"], ["del", "Delete"], ["tab", "Tab"], ["space", "Space"], ["left", "ArrowLeft"], ["arrowleft", "ArrowLeft"], ["right", "ArrowRight"], ["arrowright", "ArrowRight"], ["up", "ArrowUp"], ["arrowup", "ArrowUp"], ["down", "ArrowDown"], ["arrowdown", "ArrowDown"], ["home", "Home"], ["begin", "Home"], ["end", "End"], ["insert", "Insert"], ["pageup", "PageUp"], ["prior", "PageUp"], ["pagedown", "PageDown"], ["next", "PageDown"], ["menu", "ContextMenu"], ["exclam", "!"], ["quotedbl", '"'], ["numbersign", "#"], ["dollar", "$"], ["percent", "%"], ["ampersand", "&"], ["apostrophe", "'"], ["parenleft", "("], ["parenright", ")"], ["asterisk", "*"], ["plus", "+"], ["comma", ","], ["minus", "-"], ["period", "."], ["slash", "/"], ["colon", ":"], ["semicolon", ";"], ["less", "<"], ["equal", "="], ["greater", ">"], ["question", "?"], ["at", "@"], ["bracketleft", "["], ["backslash", "\\"], ["bracketright", "]"], ["asciicircum", "^"], ["underscore", "_"], ["grave", "`"], ["braceleft", "{"], ["bar", "|"], ["braceright", "}"], ["asciitilde", "~"], ["kpenter", "NumpadEnter"], ["kpmultiply", "NumpadMultiply"], ["kpadd", "NumpadAdd"], ["kpsubtract", "NumpadSubtract"], ["kpdecimal", "NumpadDecimal"], ["kpdivide", "NumpadDivide"], ["kp0", "Numpad0"], ["kp1", "Numpad1"], ["kp2", "Numpad2"], ["kp3", "Numpad3"], ["kp4", "Numpad4"], ["kp5", "Numpad5"], ["kp6", "Numpad6"], ["kp7", "Numpad7"], ["kp8", "Numpad8"], ["kp9", "Numpad9"]]);

const o3 = new Map([["ctrl+a", ["ControlOrMeta", "a"]], ["ctrl+c", ["ControlOrMeta", "c"]], ["ctrl+l", ["ControlOrMeta", "l"]], ["ctrl+n", ["ControlOrMeta", "n"]], ["ctrl+shift+v", ["ControlOrMeta", "Shift", "v"]], ["ctrl+v", ["ControlOrMeta", "v"]], ["ctrl+x", ["ControlOrMeta", "x"]], ["ctrl+y", ["ControlOrMeta", "Shift", "z"]], ["ctrl+z", ["ControlOrMeta", "z"]]]);

const i3 = new Map([["AltLeft", ["Alt"]], ["ControlLeft", ["Control"]], ["Enter", [`
`, "\r"]], ["MetaLeft", ["Meta"]], ["ShiftLeft", ["Shift"]]]);

const kh = s3({ Escape: { keyCode: 27, key: "Escape" }, F1: { keyCode: 112, key: "F1" }, F2: { keyCode: 113, key: "F2" }, F3: { keyCode: 114, key: "F3" }, F4: { keyCode: 115, key: "F4" }, F5: { keyCode: 116, key: "F5" }, F6: { keyCode: 117, key: "F6" }, F7: { keyCode: 118, key: "F7" }, F8: { keyCode: 119, key: "F8" }, F9: { keyCode: 120, key: "F9" }, F10: { keyCode: 121, key: "F10" }, F11: { keyCode: 122, key: "F11" }, F12: { keyCode: 123, key: "F12" }, F13: { keyCode: 124, key: "F13" }, F14: { keyCode: 125, key: "F14" }, F15: { keyCode: 126, key: "F15" }, F16: { keyCode: 127, key: "F16" }, F17: { keyCode: 128, key: "F17" }, F18: { keyCode: 129, key: "F18" }, F19: { keyCode: 130, key: "F19" }, F20: { keyCode: 131, key: "F20" }, Backquote: { keyCode: 192, shiftKey: "~", key: "`" }, Digit1: { keyCode: 49, shiftKey: "!", key: "1" }, Digit2: { keyCode: 50, shiftKey: "@", key: "2" }, Digit3: { keyCode: 51, shiftKey: "#", key: "3" }, Digit4: { keyCode: 52, shiftKey: "$", key: "4" }, Digit5: { keyCode: 53, shiftKey: "%", key: "5" }, Digit6: { keyCode: 54, shiftKey: "^", key: "6" }, Digit7: { keyCode: 55, shiftKey: "&", key: "7" }, Digit8: { keyCode: 56, shiftKey: "*", key: "8" }, Digit9: { keyCode: 57, shiftKey: "(", key: "9" }, Digit0: { keyCode: 48, shiftKey: ")", key: "0" }, Minus: { keyCode: 189, shiftKey: "_", key: "-" }, Equal: { keyCode: 187, shiftKey: "+", key: "=" }, Backslash: { keyCode: 220, shiftKey: "|", key: "\\" }, Backspace: { keyCode: 8, key: "Backspace" }, Tab: { keyCode: 9, key: "Tab" }, KeyQ: { keyCode: 81, shiftKey: "Q", key: "q" }, KeyW: { keyCode: 87, shiftKey: "W", key: "w" }, KeyE: { keyCode: 69, shiftKey: "E", key: "e" }, KeyR: { keyCode: 82, shiftKey: "R", key: "r" }, KeyT: { keyCode: 84, shiftKey: "T", key: "t" }, KeyY: { keyCode: 89, shiftKey: "Y", key: "y" }, KeyU: { keyCode: 85, shiftKey: "U", key: "u" }, KeyI: { keyCode: 73, shiftKey: "I", key: "i" }, KeyO: { keyCode: 79, shiftKey: "O", key: "o" }, KeyP: { keyCode: 80, shiftKey: "P", key: "p" }, BracketLeft: { keyCode: 219, shiftKey: "{", key: "[" }, BracketRight: { keyCode: 221, shiftKey: "}", key: "]" }, CapsLock: { keyCode: 20, key: "CapsLock" }, KeyA: { keyCode: 65, shiftKey: "A", key: "a" }, KeyS: { keyCode: 83, shiftKey: "S", key: "s" }, KeyD: { keyCode: 68, shiftKey: "D", key: "d" }, KeyF: { keyCode: 70, shiftKey: "F", key: "f" }, KeyG: { keyCode: 71, shiftKey: "G", key: "g" }, KeyH: { keyCode: 72, shiftKey: "H", key: "h" }, KeyJ: { keyCode: 74, shiftKey: "J", key: "j" }, KeyK: { keyCode: 75, shiftKey: "K", key: "k" }, KeyL: { keyCode: 76, shiftKey: "L", key: "l" }, Semicolon: { keyCode: 186, shiftKey: ":", key: ";" }, Quote: { keyCode: 222, shiftKey: '"', key: "'" }, Enter: { keyCode: 13, key: "Enter", text: "\r" }, ShiftLeft: { keyCode: 160, keyCodeWithoutLocation: 16, key: "Shift", location: 1 }, KeyZ: { keyCode: 90, shiftKey: "Z", key: "z" }, KeyX: { keyCode: 88, shiftKey: "X", key: "x" }, KeyC: { keyCode: 67, shiftKey: "C", key: "c" }, KeyV: { keyCode: 86, shiftKey: "V", key: "v" }, KeyB: { keyCode: 66, shiftKey: "B", key: "b" }, KeyN: { keyCode: 78, shiftKey: "N", key: "n" }, KeyM: { keyCode: 77, shiftKey: "M", key: "m" }, Comma: { keyCode: 188, shiftKey: "<", key: "," }, Period: { keyCode: 190, shiftKey: ">", key: "." }, Slash: { keyCode: 191, shiftKey: "?", key: "/" }, ShiftRight: { keyCode: 161, keyCodeWithoutLocation: 16, key: "Shift", location: 2 }, ControlLeft: { keyCode: 162, keyCodeWithoutLocation: 17, key: "Control", location: 1 }, MetaLeft: { keyCode: 91, key: "Meta", location: 1 }, AltLeft: { keyCode: 164, keyCodeWithoutLocation: 18, key: "Alt", location: 1 }, Space: { keyCode: 32, key: " " }, AltRight: { keyCode: 165, keyCodeWithoutLocation: 18, key: "Alt", location: 2 }, AltGraph: { keyCode: 225, key: "AltGraph" }, MetaRight: { keyCode: 92, key: "Meta", location: 2 }, ContextMenu: { keyCode: 93, key: "ContextMenu" }, ControlRight: { keyCode: 163, keyCodeWithoutLocation: 17, key: "Control", location: 2 }, PageUp: { keyCode: 33, key: "PageUp" }, PageDown: { keyCode: 34, key: "PageDown" }, Insert: { keyCode: 45, key: "Insert" }, Delete: { keyCode: 46, key: "Delete" }, Home: { keyCode: 36, key: "Home" }, End: { keyCode: 35, key: "End" }, ArrowLeft: { keyCode: 37, key: "ArrowLeft" }, ArrowUp: { keyCode: 38, key: "ArrowUp" }, ArrowRight: { keyCode: 39, key: "ArrowRight" }, ArrowDown: { keyCode: 40, key: "ArrowDown" }, NumpadEnter: { keyCode: 13, key: "Enter", text: "\r", location: Ke }, Numpad0: { keyCode: 96, key: "0", location: Ke }, Numpad1: { keyCode: 97, key: "1", location: Ke }, Numpad2: { keyCode: 98, key: "2", location: Ke }, Numpad3: { keyCode: 99, key: "3", location: Ke }, Numpad4: { keyCode: 100, key: "4", location: Ke }, Numpad5: { keyCode: 101, key: "5", location: Ke }, Numpad6: { keyCode: 102, key: "6", location: Ke }, Numpad7: { keyCode: 103, key: "7", location: Ke }, Numpad8: { keyCode: 104, key: "8", location: Ke }, Numpad9: { keyCode: 105, key: "9", location: Ke }, NumpadMultiply: { keyCode: 106, key: "*", location: Ke }, NumpadAdd: { keyCode: 107, key: "+", location: Ke }, NumpadSubtract: { keyCode: 109, key: "-", location: Ke }, NumpadDecimal: { keyCode: 110, key: ".", location: Ke }, NumpadDivide: { keyCode: 111, key: "/", location: Ke } });

function s3(e) { let t = new Map; for (let [r, n] of Object.entries(e)) {
    let o = { key: n.key, keyCode: n.keyCode, keyCodeWithoutLocation: n.keyCodeWithoutLocation ?? n.keyCode, code: r, text: n.text ?? "", location: n.location ?? 0 };
    n.key.length === 1 && (o.text = n.key);
    let i;
    n.shiftKey && (i = { ...o, key: n.shiftKey, keyCode: n.shiftKeyCode ?? o.keyCode, text: n.shiftKey }, o.shifted = i), t.set(r, o);
    for (let s of i3.get(r) ?? [])
        t.set(s, o);
    n.location || (o.key.length === 1 && t.set(o.key, o), i && t.set(i.key, { ...i, shifted: void 0 }));
} return t; }

const on = { attributes: ["type", "autocomplete", "id", "name", "placeholder", "aria-label", "title"], pattern: /user[-_ ]?name|e[-_ ]?mail|one[-_ ]?time[-_ ]?code|password|passcode|passwd|\botp\b|\b(?:2fa|mfa)\b|phone|mobile|\btel\b/i.source };

function ai(e, t) { if (e.tagName.toLowerCase() !== "input")
    return !1; if (e.getAttribute("type")?.toLowerCase() === "hidden")
    return !0; let r = ""; for (let n of t.attributes)
    r += ` ${e.getAttribute(n) ?? ""}`; return new RegExp(t.pattern, "i").test(r); }

const tc = async (e) => { let t = Object.getOwnPropertyDescriptor(window.Element.prototype, "localName")?.get, r = Object.getOwnPropertyDescriptor(window.Element.prototype, "namespaceURI")?.get; if (t == null || r == null)
    throw new Error("Browser Use virtual clipboard requires native DOM getters"); let n = F => { if (F == null)
    return null; try {
    t.call(F);
}
catch {
    return null;
} let D = F; return D.ownerDocument.defaultView == null ? null : D; }, o = F => r.call(F) === "http://www.w3.org/1999/xhtml", i = (F, D) => o(F) && t.call(F) === D, s = F => F.ownerDocument.defaultView ?? window, a = (F, D) => F.tagName?.toLowerCase() === "input" && (F.getAttribute("type")?.toLowerCase() === "hidden" || new RegExp(D.pattern, "i").test(D.attributes.map(P => F.getAttribute(P) ?? "").join(" "))), l = (F, D) => { let P = !1; for (let W of F.querySelectorAll("input[value]"))
    a(W, D) && (W.removeAttribute("value"), P = !0); return P; }, c = F => { let D = n(F.activeElement); if (D == null)
    return null; if (o(D) && D.shadowRoot != null)
    return c(D.shadowRoot) ?? D; if (i(D, "iframe") || i(D, "frame"))
    try {
        let P = D.contentDocument ?? D.contentWindow?.document ?? null;
        if (P != null)
            return c(P) ?? D;
    }
    catch {
        return D;
    } return D; }, p = (F, D) => { if (D == null)
    return; let P = n(F), W = P ?? null, V = e.requireDocumentFocus === !0 ? document.hasFocus() : null, ce = W?.__codexIabInputTargetToken === D; if (V === !1 || !ce) {
    let K = V === !1 ? "document is not focused" : "target token mismatch";
    throw new Error(`Active element is no longer the expected input target: ${K}`);
} }, m = (F, D) => F.flatMap(P => P.entries).find(P => P.mime_type === D)?.text ?? "", h = F => { let D = atob(F), P = new Uint8Array(D.length); for (let W = 0; W < D.length; W += 1)
    P[W] = D.charCodeAt(W); return P.buffer; }, y = F => { let D = new Uint8Array(F), P = 32768, W = ""; for (let V = 0; V < D.length; V += P) {
    let ce = D.subarray(V, V + P);
    W += String.fromCharCode(...ce);
} return btoa(W); }, w = (F, D, P) => { F.clearData(); for (let W of D)
    for (let V of W.entries) {
        if (typeof V.text == "string") {
            F.setData(V.mime_type, V.text);
            continue;
        }
        let ce = V.mime_type.split("/")[1] ?? "bin";
        F.items.add(new P.File([h(V.base64 ?? "")], `clipboard.${ce}`, { type: V.mime_type }));
    } }, v = async (F) => { let D = []; for (let P of Array.from(F.types))
    P !== "Files" && D.push({ mime_type: P, text: F.getData(P) }); for (let P of Array.from(F.files))
    P.type.length !== 0 && D.push({ mime_type: P.type, base64: y(await P.arrayBuffer()) }); return D.length === 0 ? [] : [{ entries: D, presentation_style: "unspecified" }]; }, S = (F, D) => { let P = []; return F.length > 0 && P.push({ mime_type: "text/plain", text: F }), D.length > 0 && P.push({ mime_type: "text/html", text: D }), P.length === 0 ? [] : [{ entries: P, presentation_style: "unspecified" }]; }, R = (F, D) => new F.InputEvent(D, { bubbles: !0, cancelable: D === "beforeinput", composed: !0, inputType: "deleteByCut" }), T = (F, D) => { let P = n(F); if (P == null)
    return null; let W = s(P); if (i(P, "textarea") || i(P, "input")) {
    let De = P.selectionStart, Ce = P.selectionEnd;
    if (De == null || Ce == null || De === Ce)
        return null;
    let tt = P.value.slice(De, Ce);
    return { items: S(tt, ""), delete: () => { P.disabled || P.readOnly || P.value.slice(De, Ce) !== tt || !P.dispatchEvent(R(W, "beforeinput")) || (P.setRangeText("", De, Ce, "end"), P.dispatchEvent(R(W, "input"))); } };
} let V = W.getSelection?.(); if (V == null || V.rangeCount === 0 || V.isCollapsed)
    return null; let ce = Array.from({ length: V.rangeCount }, (De, Ce) => V.getRangeAt(Ce).cloneRange()), K = P.ownerDocument.createElement("div"); for (let De of ce)
    K.append(De.cloneContents()); l(K, D); let fe = V.toString(), Ne = null; if (o(P) && P.isContentEditable)
    for (Ne = P; Ne.parentElement?.isContentEditable;)
        Ne = Ne.parentElement; return { items: S(fe, K.innerHTML), delete: () => { if (!(Ne == null || !Ne.isConnected || !Ne.isContentEditable || !ce.every(De => Ne?.contains(De.commonAncestorContainer)) || ce.map(De => De.toString()).join("") !== fe || !P.dispatchEvent(R(W, "beforeinput")))) {
        for (let De of ce.reverse())
            De.deleteContents();
        P.dispatchEvent(R(W, "input"));
    } } }; }, k = (F, D, P, W) => { let V = n(F); if (V == null)
    return; let ce = s(V); if (i(V, "textarea") || i(V, "input")) {
    if (V.disabled || V.readOnly || P.length === 0)
        return;
    let K = fe => { let Ne = Object.getPrototypeOf(V), De = Object.getOwnPropertyDescriptor(Ne, "value")?.set, Ce = Object.getOwnPropertyDescriptor(V, "value")?.set; if (De != null && Ce !== De) {
        De.call(V, fe);
        return;
    } V.value = fe; };
    if (V.selectionStart == null || V.selectionEnd == null)
        K(W ? P : V.value + P);
    else {
        let fe = V.selectionStart ?? V.value.length, Ne = V.selectionEnd ?? V.value.length;
        try {
            V.setRangeText(P, fe, Ne, "end");
        }
        catch {
            K(W ? P : V.value + P);
        }
    }
    V.dispatchEvent(new ce.InputEvent("input", { bubbles: !0 }));
    return;
} if (o(V) && (V.isContentEditable || V.closest("[contenteditable=true]"))) {
    if (V.focus(), D.length > 0) {
        V.ownerDocument.execCommand("insertHTML", !1, D);
        return;
    }
    P.length > 0 && V.ownerDocument.execCommand("insertText", !1, P);
} }, I = globalThis; delete I.__browserUsePendingCut; let L = c(document) ?? document.body; p(L, e.iabInputTargetToken); let $ = n(L), U = $ == null ? window : s($), X = e.clipboardItems; if (e.action === "paste") {
    if (X.length === 0)
        throw new Error("Browser Use virtual clipboard has no data to paste");
    let F = m(X, "text/plain"), D = e.richTextFallback === !0 ? m(X, "text/html") : "";
    if (typeof U.DataTransfer != "function" || typeof U.ClipboardEvent != "function")
        return k(L, D, F, e.replaceInputValue === !0), {};
    let P = new U.DataTransfer;
    w(P, X, U);
    let W = new U.ClipboardEvent("paste", { bubbles: !0, cancelable: !0, clipboardData: P, composed: !0 });
    return L.dispatchEvent(W) && k(L, D, F, e.replaceInputValue === !0), {};
} let oe = e.protectedCredentialFieldPolicy; if (oe == null)
    throw new Error("Virtual clipboard is missing credential protection"); if ($ != null && a($, oe))
    return {}; let me = T(L, oe), se = new U.DataTransfer, Y = new U.ClipboardEvent(e.action, { bubbles: !0, cancelable: !0, clipboardData: se, composed: !0 }); if (!L.dispatchEvent(Y)) {
    let F = await v(se);
    if (F.length === 0)
        return {};
    for (let D of F.flatMap(P => P.entries)) {
        if (D.mime_type !== "text/html" || typeof D.text != "string" || !/<input[\s/>]/i.test(D.text))
            continue;
        let P = U.document.createElement("template");
        P.innerHTML = D.text, l(P.content, oe) && (D.text = P.innerHTML);
    }
    return { items: F };
} if (me == null || me.items.length === 0)
    return {}; if (e.action === "cut") {
    if (e.cutToken == null)
        throw new Error("Browser Use virtual cut is missing a commit token");
    I.__browserUsePendingCut = { commit: me.delete, token: e.cutToken };
} return { items: me.items, ...e.action === "cut" ? { cutToken: e.cutToken } : {} }; };

const _x = ({ cutToken: e }) => { let t = globalThis, r = t.__browserUsePendingCut; if (r?.token !== e)
    throw new Error("Browser Use virtual cut is no longer pending"); delete t.__browserUsePendingCut, r.commit(); };

const mW = 50;

const fW = 5e3;

const hW = "octopus.browser.actions";

const hI = new WeakSet;

const bn = new Map;

const yn = new Map;

function yI(e) { hI.has(e) || (hI.add(e), e.addTabCleanupHandler(t => { yn.delete(t); for (let r of bn.keys())
    r.startsWith(`${t}:`) && bn.delete(r); })); }

function wI(e, t) { let r = yn.get(e), n = r?.mainFrameKey === t ? r : { framesById: new Map, mainFrameKey: t, nextPublicId: 1, nodeByPublicId: new Map, publicIdByFrameLocalRef: new Map }; n.framesById.clear(), n.nodeByPublicId.clear(), n.publicIdByFrameLocalRef.size > fW && (n.publicIdByFrameLocalRef.clear(), n.nextPublicId = 1), yn.set(e, n); }

function _I(e, t) { let r = yn.get(e); r?.framesById.set(t.frameId, t); }

function vI(e, t, r, n) { let o = yn.get(e); if (o == null)
    throw new Error("DOM CUA snapshot state is missing"); let i = gW(t, r), s = o.publicIdByFrameLocalRef.get(i); return s == null && (s = String(o.nextPublicId++), o.publicIdByFrameLocalRef.set(i, s)), o.nodeByPublicId.set(s, { frameId: t.frameId, loaderId: t.loaderId, localRef: r, target: t.target, viewportClip: n }), s; }

function xI(e, t) { let r = yn.get(e)?.nodeByPublicId.get(t); if (r == null)
    throw new Error(`DOM node ${t} is stale or missing`); return r; }

async function ru(e, t, r, n, o, i) { let s = await gI(e, t, r, n, i.timeoutMs); try {
    return await bI(e, t, s, o, i);
}
catch (a) {
    if (!bW(a))
        throw a;
    return bn.delete(AI(t, r, n)), s = await gI(e, t, r, n, i.timeoutMs), await bI(e, t, s, o, i);
} }

async function SI(e, t, r, n = {}) { let { backendNodeId: o } = await e.callTarget(t, "DOM.getFrameOwner", { frameId: r }, { timeoutMs: n.timeoutMs }), { quads: i } = await e.callTarget(t, "DOM.getContentQuads", { backendNodeId: o }, { timeoutMs: n.timeoutMs }); return i[0] ?? null; }

async function CI(e, t, r, n) { let o = yn.get(e); if (o == null)
    throw new Error(`Missing DOM CUA frame ${r}`); let i = o.framesById.get(r); if (i == null)
    throw new Error(`Missing DOM CUA frame ${r}`); let s = n; for (; i?.parentFrameId != null;) {
    let a = o.framesById.get(i.parentFrameId);
    if (a == null)
        throw new Error(`Missing parent DOM CUA frame ${i.frameId}`);
    for (s = EI(s, i, await SI(t, a.target, i.frameId), r), i = a; i.parentFrameId != null;) {
        let l = o.framesById.get(i.parentFrameId);
        if (l == null)
            throw new Error(`Missing parent DOM CUA frame ${i.frameId}`);
        if (!II(i.target, l.target))
            break;
        i = l;
    }
} return s; }

async function TI(e, t, r, n) { let o = yn.get(e); if (o == null)
    throw new Error(`Missing DOM CUA frame ${r}`); let i = o.framesById.get(r); if (i == null)
    throw new Error(`Missing DOM CUA frame ${r}`); let s = n; for (; i.parentFrameId != null;) {
    let a = o.framesById.get(i.parentFrameId);
    if (a == null)
        throw new Error(`Missing parent DOM CUA frame ${i.frameId}`);
    if (!II(i.target, a.target))
        break;
    s = EI(s, i, await SI(t, a.target, i.frameId), r), i = a;
} return s; }

function db(e, t) { let r = { bottom: Math.min(e.bottom, t.bottom), left: Math.max(e.left, t.left), right: Math.min(e.right, t.right), top: Math.max(e.top, t.top) }; return r.right > r.left && r.bottom > r.top ? r : null; }

function pb(e, t) { let r = db(e, t); return r == null ? null : { bottom: r.bottom - e.top, left: r.left - e.left, right: r.right - e.left, top: r.top - e.top }; }

function EI(e, t, r, n) { let o = ro(e, t.size, r); if (o == null)
    throw new Error(`Missing frame geometry for DOM CUA frame ${n}`); return o; }

async function gI(e, t, r, n, o) { let i = AI(t, r, n), s = bn.get(i); if (s != null)
    return s; let l = { executionContextId: (await e.callTarget(t, "Page.createIsolatedWorld", { frameId: r, grantUniveralAccess: !1, worldName: hW }, { timeoutMs: o })).executionContextId }; if (bn.set(i, l), bn.size > mW) {
    let c = bn.keys().next().value;
    c != null && bn.delete(c);
} return l; }

async function bI(e, t, r, n, o) { return await e.callTarget(t, "Runtime.evaluate", { awaitPromise: !0, contextId: r.executionContextId, expression: n, returnByValue: o.returnByValue, timeout: o.timeoutMs }, { timeoutMs: o.timeoutMs }); }

function gW(e, t) { return `${e.frameId}:${e.loaderId ?? ""}:${t}`; }

function AI(e, t, r) { return `${cb(e)}:${t}:${r ?? ""}`; }

function cb(e) { return `${e.tabId}:${e.sessionId ?? ""}:${e.targetId ?? ""}`; }

function II(e, t) { return cb(e) === cb(t); }

function bW(e) { let t = e instanceof Error ? e.message : String(e); return /Cannot find context|Execution context was destroyed|context.*destroyed|context.*not found/i.test(t); }

const yW = new Set(["a", "button", "details", "input", "option", "select", "summary", "textarea"]);

const wW = ["aria-disabled", "aria-label", "contenteditable", "href", "name", "placeholder", "role", "title", "type", "value"];

const _W = ["checked", "disabled", "multiple", "readonly", "required", "selected"];

const vW = ["button", "checkbox", "combobox", "link", "menuitem", "option", "radio", "slider", "spinbutton", "switch", "tab", "textbox"];

const nu = 2e4;

const ou = 200;

const xW = 5e3;

const SW = 50;

const RI = 500;

const PI = 1e3;

const kI = "const __name = (target) => target;";

async function MI(e, t, r = {}) {
    yI(t);
    let n = r.timeoutMs == null ? void 0 : Date.now() + r.timeoutMs, { frameTree: o } = await t.call(e, "Page.getFrameTree", void 0, n == null ? {} : { deadlineMs: n, timeoutMs: r.timeoutMs }), i = IW(o.frame);
    r.reviewer == null && wI(e, i);
    let s = r.credentialFrameId === o.frame.id ? void 0 : r.credentialFrameId, a = { chars: 0, lines: [], ...r.reviewer == null ? {} : { reviewer: r.reviewer } }, l = await mb({ cdp: t, childFrameDeadline: Date.now() + PI, deadlineMs: n, frameReference: { frameId: o.frame.id, loaderId: o.frame.loaderId, target: { tabId: e } }, ...s == null ? {} : { maxChars: nu - xW, maxElements: ou - SW }, output: a, tabId: e, viewportClip: void 0 });
    if (l?.viewportClip == null || !fb(a))
        return a.lines.join(`
`);
    let c;
    if (s != null && (n == null || Date.now() < n)) {
        let m = await EW(t, l.frame, l.childFrames, s, n, a.reviewer != null), h = m == null ? null : pb(m.rect, l.viewportClip);
        if (m != null && h != null) {
            let y = await t.targetForFrameOrAttach(e, s, { deadlineMs: n, timeoutMs: gb(n) }, m.url == null ? {} : { url: m.url }).catch(() => null) ?? { tabId: e };
            (await mb({ cdp: t, childFrameDeadline: n ?? 1 / 0, deadlineMs: n, frameReference: { frameId: s, parentFrameId: o.frame.id, size: m.size, target: y }, output: a, tabId: e, useRequestDeadline: !0, viewportClip: h }))?.viewportClip != null && (c = s);
        }
    }
    let p = Math.min(Date.now() + PI, n ?? 1 / 0);
    return await OI({ childFrames: l.childFrames, cdp: t, childFrameDeadline: p, frameReference: l.frame, output: a, prioritizedFrameId: c, tabId: e, viewportClip: l.viewportClip }), a.lines.join(`
`);
}

function CW(e, t, r, n) {
    let o = { booleanAttributeNames: _W, codexOverlayRootId: Yv, interactiveRoleNames: vW, interactiveTags: [...yW], maxChars: e, maxElements: t, renderedAttributeNames: wW, reviewerOnly: n, viewportClip: r };
    return `(() => {
    ${kI}
    return (${MW.toString()})(
      ${JSON.stringify(o)},
      ${ai.toString()},
      ${JSON.stringify(on)},
    );
  })()`;
}

async function mb(e) { if (e.output.lines.length >= ou || e.output.chars >= nu || e.frameReference.parentFrameId != null && Date.now() >= e.childFrameDeadline)
    return null; let t = e.frameReference; e.output.reviewer == null && _I(e.tabId, t); let r = Math.min(ou - e.output.lines.length, e.maxElements ?? ou), n = Math.min(nu - e.output.chars, e.maxChars ?? nu), o = e.output.lines.length + r, i = e.output.chars + n, s = await TW(e.cdp, t, n, r, e.viewportClip, e.output.reviewer != null, t.parentFrameId == null || e.useRequestDeadline === !0 ? gb(e.deadlineMs) : hb(e.childFrameDeadline)).catch(l => { if (t.parentFrameId != null)
    return null; throw l; }); if (s == null)
    return null; let a = e.viewportClip == null ? s.viewport : db(e.viewportClip, s.viewport); if (a == null)
    return { childFrames: [], frame: t, viewportClip: a }; for (let l of s.items) {
    if (e.output.lines.length >= o || e.output.chars >= i)
        break;
    let c = e.output.reviewer == null ? vI(e.tabId, t, l.ref, a) : String(e.output.reviewer.nextNodeId++), p = l.line.replace(`node_id=${l.ref}`, `node_id=${c}`), m = p.length + (e.output.lines.length === 0 ? 0 : 1);
    if (e.output.chars + m > i)
        break;
    e.output.lines.push(p), e.output.chars += m, e.output.reviewer?.capturedFrameIds.add(t.frameId);
} return { childFrames: s.frameElements, frame: t, viewportClip: a }; }

async function OI(e) { if (fb(e.output))
    for (let t of e.childFrames) {
        if (!fb(e.output) || Date.now() >= e.childFrameDeadline)
            break;
        let r = pb(t.rect, e.viewportClip);
        if (r == null)
            continue;
        let n = await NI(e.cdp, e.frameReference, t.ref, hb(e.childFrameDeadline), e.output.reviewer != null).catch(() => null);
        if (n == null || n === e.prioritizedFrameId)
            continue;
        let o = await e.cdp.targetForFrameOrAttach(e.tabId, n, { timeoutMs: hb(e.childFrameDeadline) }, t.url == null ? {} : { url: t.url }).catch(() => null) ?? e.frameReference.target, i = await mb({ cdp: e.cdp, childFrameDeadline: e.childFrameDeadline, frameReference: { frameId: n, parentFrameId: e.frameReference.frameId, size: t.size, target: o }, output: e.output, tabId: e.tabId, viewportClip: r });
        i?.viewportClip != null && await OI({ childFrames: i.childFrames, cdp: e.cdp, childFrameDeadline: e.childFrameDeadline, frameReference: i.frame, output: e.output, prioritizedFrameId: e.prioritizedFrameId, tabId: e.tabId, viewportClip: i.viewportClip });
    } }

function fb(e) { return e.lines.length < ou && e.chars < nu; }

async function TW(e, t, r, n, o, i, s) { let a = await ru(e, t.target, t.frameId, t.loaderId, CW(r, n, o, i), { returnByValue: !0, timeoutMs: s }); if (a.exceptionDetails != null)
    throw new Error(FI(a.exceptionDetails)); return PW(a.result.value); }

async function NI(e, t, r, n, o) { let i = await ru(e, t.target, t.frameId, t.loaderId, AW(r, o), { returnByValue: !1, timeoutMs: n }); if (i.exceptionDetails != null)
    throw new Error(FI(i.exceptionDetails)); let s = i.result.objectId; if (typeof s != "string")
    return null; try {
    let { node: a } = await e.callTarget(t.target, "DOM.describeNode", { objectId: s }, { timeoutMs: n });
    return typeof a.frameId == "string" ? a.frameId : null;
}
finally {
    await e.callTarget(t.target, "Runtime.releaseObject", { objectId: s }, { timeoutMs: n }).catch(() => { });
} }

async function EW(e, t, r, n, o, i) { for (let s of r) {
    if (o != null && Date.now() >= o)
        break;
    if (await NI(e, t, s.ref, gb(o) ?? RI, i).catch(() => null) === n)
        return s;
} }

function AW(e, t) {
    return `(() => {
    ${kI}
    const state = globalThis.${t ? "__browserUseReviewerVisibleDomState" : "__browserUseVisibleDomState"};
    return state?.refToElement?.get(${JSON.stringify(e)}) ?? null;
  })()`;
}

function hb(e) { return Math.max(1, Math.min(RI, e - Date.now())); }

function gb(e) { return e == null ? void 0 : Math.max(1, e - Date.now()); }

function IW(e) { return `${e.id}:${e.loaderId ?? ""}`; }

function PW(e) { if (e == null || typeof e != "object")
    return null; let t = e; return !Array.isArray(t.items) || t.viewport == null ? null : { frameElements: Array.isArray(t.frameElements) ? t.frameElements.filter(RW) : [], items: t.items.filter(DW), viewport: t.viewport }; }

function RW(e) { if (e == null || typeof e != "object")
    return !1; let t = e; return typeof t.ref == "string" && BW(t.rect) && (t.size == null || kW(t.size)) && (t.url == null || typeof t.url == "string"); }

function kW(e) { if (e == null || typeof e != "object")
    return !1; let t = e; return typeof t.height == "number" && Number.isFinite(t.height) && t.height > 0 && typeof t.width == "number" && Number.isFinite(t.width) && t.width > 0; }

function DW(e) { if (e == null || typeof e != "object")
    return !1; let t = e; return typeof t.ref == "string" && typeof t.line == "string"; }

function BW(e) { if (e == null || typeof e != "object")
    return !1; let t = e; return typeof t.bottom == "number" && Number.isFinite(t.bottom) && typeof t.left == "number" && Number.isFinite(t.left) && typeof t.right == "number" && Number.isFinite(t.right) && typeof t.top == "number" && Number.isFinite(t.top); }

function MW(e, t, r) { let n = new Set(e.interactiveTags), o = new Set(e.interactiveRoleNames), i = [], s = [], a = 0, l = !1, c = globalThis, p = c[e.reviewerOnly ? "__browserUseReviewerVisibleDomState" : "__browserUseVisibleDomState"] ??= { elementToRef: new WeakMap, nextId: 1, refToElement: new Map }; p.refToElement.clear(); let m = window.visualViewport, h = m ? { bottom: m.offsetTop + m.height, left: m.offsetLeft, right: m.offsetLeft + m.width, top: m.offsetTop } : { bottom: window.innerHeight, left: 0, right: window.innerWidth, top: 0 }, y = e.viewportClip == null ? h : { bottom: Math.min(h.bottom, e.viewportClip.bottom), left: Math.max(h.left, e.viewportClip.left), right: Math.min(h.right, e.viewportClip.right), top: Math.max(h.top, e.viewportClip.top) }, w = D => (D.localName || D.nodeName || "").toLowerCase(), v = new Set(["noscript", "script", "style", "template"]), S = D => D.getAttribute("id") === e.codexOverlayRootId, R = D => { let P = w(D); if (D.getAttribute("aria-hidden") === "true" || D.hasAttribute("hidden") || P === "input" && D.getAttribute("type") === "hidden")
    return !1; let W = D.getAttribute("contenteditable"), V = D.getAttribute("role"); return n.has(P) || W != null && W.toLowerCase() !== "false" || D.hasAttribute("href") || D.hasAttribute("onclick") || V != null && o.has(V.trim().toLowerCase()) || Number(D.getAttribute("tabindex") ?? -1) >= 0 || e.reviewerOnly && Array.from(D.childNodes).some(ce => ce.nodeType === Node.TEXT_NODE && (ce.nodeValue ?? "").trim().length > 0); }, T = D => { let P = w(D); return P === "frame" || P === "iframe"; }, k = D => { let P = window.getComputedStyle(D); if (P.visibility !== "visible" || P.display === "none" || P.pointerEvents === "none" || Number(P.opacity) <= .01)
    return null; for (let W of D.getClientRects())
    if (!(W.width <= 0 || W.height <= 0 || W.right <= y.left || W.left >= y.right || W.bottom <= y.top || W.top >= y.bottom))
        return { bottom: W.bottom, left: W.left, right: W.right, top: W.top }; return null; }, I = D => k(D) != null, L = D => D.replace(/\s+/g, " ").trim(), $ = D => D.replace(/[\t\n\f\r]+/g, " ").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;"), U = D => { let P = [], W = 0, V = ce => { if (!(W >= 160)) {
    if (ce.nodeType === Node.TEXT_NODE) {
        let K = L(ce.nodeValue || "");
        K.length > 0 && (P.push(K), W += K.length + 1);
        return;
    }
    if (!(ce.nodeType === Node.ELEMENT_NODE && (v.has(w(ce)) || e.reviewerOnly && w(ce) === "textarea" || e.reviewerOnly && (ce.getAttribute("aria-hidden") === "true" || ce.hasAttribute("hidden"))))) {
        for (let K of ce.childNodes || []) {
            if (W >= 160)
                break;
            V(K);
        }
        if (ce.nodeType === Node.ELEMENT_NODE) {
            let K = ce;
            if (!K.shadowRoot)
                return;
            for (let fe of K.shadowRoot.childNodes) {
                if (W >= 160)
                    break;
                V(fe);
            }
        }
    }
} }; return V(D), L(P.join(" ")).slice(0, 160); }, X = D => { let P = p.elementToRef.get(D); return P == null && (P = String(p.nextId++), p.elementToRef.set(D, P)), P; }, oe = (D, P) => { let W = w(D), V = [`node_id=${P}`]; for (let K of e.renderedAttributeNames) {
    if (K === "value" && (t(D, r) || e.reviewerOnly && (W === "input" || W === "textarea")))
        continue;
    let fe = D.getAttribute(K);
    fe != null && fe !== "" && V.push(`${K}="${$(fe)}"`);
} for (let K of e.booleanAttributeNames)
    D.hasAttribute(K) && V.push(`${K}="true"`); let ce = U(D); return ce.length === 0 ? `<${W} ${V.join(" ")} />` : `<${W} ${V.join(" ")}>${$(ce)}</${W}>`; }, me = () => l || s.length >= e.maxElements || a >= e.maxChars, se = D => { if (me())
    return; let P = X(D), W = oe(D, P), V = W.length + (s.length === 0 ? 0 : 1); if (a + V > e.maxChars) {
    l = !0;
    return;
} s.push({ line: W, ref: P }), p.refToElement.set(P, D), a += V; }, Y = D => { if (i.length >= e.maxElements)
    return; let P = k(D); if (P == null)
    return; let W = X(D); if (i.some(Ne => Ne.ref === W))
    return; let V = D.src, ce = D, K = ce.clientWidth > 0 ? ce.clientWidth : P.right - P.left, fe = ce.clientHeight > 0 ? ce.clientHeight : P.bottom - P.top; i.push({ rect: P, ref: W, size: { height: fe, width: K }, ...V.length > 0 ? { url: V } : {} }), p.refToElement.set(W, D); }; if (e.reviewerOnly)
    for (let D of document.querySelectorAll("iframe, frame"))
        D.closest(`#${e.codexOverlayRootId}`) == null && Y(D); let F = D => { if (me())
    return; if (D.nodeType === Node.DOCUMENT_NODE) {
    let W = D.documentElement;
    W && F(W);
    return;
} if (D.nodeType === Node.DOCUMENT_FRAGMENT_NODE) {
    for (let W of D.children) {
        if (me())
            break;
        F(W);
    }
    return;
} if (D.nodeType !== Node.ELEMENT_NODE)
    return; let P = D; if (!S(P)) {
    T(P) && Y(P), R(P) && I(P) && se(P), P.shadowRoot && !me() && F(P.shadowRoot);
    for (let W of P.children) {
        if (me())
            break;
        F(W);
    }
} }; return F(document), { frameElements: i, items: s, viewport: h }; }

function FI(e) { let t = e.exception; return typeof t?.value == "string" ? t.value : t?.description ?? e.text ?? "Visible DOM evaluation failed"; }

function wr(e) { return e.sessionId != null || e.targetId != null; }

const nD = 'var PlaywrightInjected=(()=>{const dt={exports:{}};return(()=>{var lo=e=>{let t=!1,r;return function(){if(!t){t=!0;let i;for(const o in e){i=e[o];break}const s={exports:{}};i(s.exports,s),r=s.exports}return r}},Mn=(e,t)=>{for(var r in t)e[r]=t[r]},co=e=>({...e,default:e}),qn=e=>({...e,__esModule:!0}),Qt={};Mn(Qt,{InjectedScript:()=>to}),dt.exports=qn(Qt);function Dn(e,t){if(e.role!==t.role||e.name!==t.name||!Bn(e,t)||We(e)!==We(t))return!1;const r=Object.keys(e.props),n=Object.keys(t.props);return r.length===n.length&&r.every(i=>e.props[i]===t.props[i])}function We(e){return e.box.cursor==="pointer"}function Bn(e,t){return e.active===t.active&&e.checked===t.checked&&e.disabled===t.disabled&&e.expanded===t.expanded&&e.selected===t.selected&&e.level===t.level&&e.pressed===t.pressed}function Hn(e,t,r={}){var n;const i=new e.LineCounter,s={keepSourceTokens:!0,lineCounter:i,...r},o=e.parseDocument(t,s),a=[],l=p=>[i.linePos(p[0]),i.linePos(p[1])],c=p=>{a.push({message:p.message,range:[i.linePos(p.pos[0]),i.linePos(p.pos[1])]})},h=(p,d)=>{for(const g of d.items){if(g instanceof e.Scalar&&typeof g.value=="string"){const x=Kt.parse(g,s,a);x&&(p.children=p.children||[],p.children.push(x));continue}if(g instanceof e.YAMLMap){f(p,g);continue}a.push({message:"Sequence items should be strings or maps",range:l(g.range||d.range)})}},f=(p,d)=>{var g;for(const m of d.items){if(p.children=p.children||[],!(m.key instanceof e.Scalar&&typeof m.key.value=="string")){a.push({message:"Only string keys are supported",range:l(m.key.range||d.range)});continue}const x=m.key,y=m.value;if(x.value==="text"){if(!(y instanceof e.Scalar&&typeof y.value=="string")){a.push({message:"Text value should be a string",range:l(m.value.range||d.range)});continue}p.children.push({kind:"text",text:pt(y.value)});continue}if(x.value==="/children"){if(!(y instanceof e.Scalar&&typeof y.value=="string")||y.value!=="contain"&&y.value!=="equal"&&y.value!=="deep-equal"){a.push({message:\'Strict value should be "contain", "equal" or "deep-equal"\',range:l(m.value.range||d.range)});continue}p.containerMode=y.value;continue}if(x.value.startsWith("/")){if(!(y instanceof e.Scalar&&typeof y.value=="string")){a.push({message:"Property value should be a string",range:l(m.value.range||d.range)});continue}p.props=(g=p.props)!=null?g:{},p.props[x.value.slice(1)]=pt(y.value);continue}const L=Kt.parse(x,s,a);if(!L)continue;if(y instanceof e.Scalar){const A=typeof y.value;if(A!=="string"&&A!=="number"&&A!=="boolean"){a.push({message:"Node value should be a string or a sequence",range:l(m.value.range||d.range)});continue}p.children.push({...L,children:[{kind:"text",text:pt(String(y.value))}]});continue}if(y instanceof e.YAMLSeq){p.children.push(L),h(L,y);continue}a.push({message:"Map values should be strings or sequences",range:l(m.value.range||d.range)})}},u={kind:"role",role:"fragment"};return o.errors.forEach(c),a.length?{errors:a,fragment:u}:(o.contents instanceof e.YAMLSeq||a.push({message:\'Aria snapshot must be a YAML sequence, elements starting with " -"\',range:o.contents?l(o.contents.range):[{line:0,col:0},{line:0,col:0}]}),a.length?{errors:a,fragment:u}:(h(u,o.contents),a.length?{errors:a,fragment:Fn}:((n=u.children)==null?void 0:n.length)===1&&(!u.containerMode||u.containerMode==="contain")?{fragment:u.children[0],errors:[]}:{fragment:u,errors:[]}))}var Fn={kind:"role",role:"fragment"};function Yt(e){return e.replace(/[\\u200b\\u00ad]/g,"").replace(/[\\r\\n\\s\\t]+/g," ").trim()}function pt(e){return{raw:e,normalized:Yt(e)}}var Kt=class Pn{static parse(t,r,n){try{return new Pn(t.value)._parse()}catch(i){if(i instanceof Zt){const s=r.prettyErrors===!1?i.message:i.message+`:\n\n`+t.value+`\n`+" ".repeat(i.pos)+`^\n`;return n.push({message:s,range:[r.lineCounter.linePos(t.range[0]),r.lineCounter.linePos(t.range[0]+i.pos)]}),null}throw i}}constructor(t){this._input=t,this._pos=0,this._length=t.length}_peek(){return this._input[this._pos]||""}_next(){return this._pos<this._length?this._input[this._pos++]:null}_eof(){return this._pos>=this._length}_isWhitespace(){return!this._eof()&&/\\s/.test(this._peek())}_skipWhitespace(){for(;this._isWhitespace();)this._pos++}_readIdentifier(t){this._eof()&&this._throwError(`Unexpected end of input when expecting ${t}`);const r=this._pos;for(;!this._eof()&&/[a-zA-Z]/.test(this._peek());)this._pos++;return this._input.slice(r,this._pos)}_readString(){let t="",r=!1;for(;!this._eof();){const n=this._next();if(r)t+=n,r=!1;else if(n==="\\\\")r=!0;else{if(n===\'"\')return t;t+=n}}this._throwError("Unterminated string")}_throwError(t,r=0){throw new Zt(t,r||this._pos)}_readRegex(){let t="",r=!1,n=!1;for(;!this._eof();){const i=this._next();if(r)t+=i,r=!1;else if(i==="\\\\")r=!0,t+=i;else{if(i==="/"&&!n)return{pattern:t};i==="["?(n=!0,t+=i):i==="]"&&n?(t+=i,n=!1):t+=i}}this._throwError("Unterminated regex")}_readStringOrRegex(){const t=this._peek();return t===\'"\'?(this._next(),Yt(this._readString())):t==="/"?(this._next(),this._readRegex()):null}_readAttributes(t){let r=this._pos;for(;this._skipWhitespace(),this._peek()==="[";){this._next(),this._skipWhitespace(),r=this._pos;const n=this._readIdentifier("attribute");this._skipWhitespace();let i="";if(this._peek()==="=")for(this._next(),this._skipWhitespace(),r=this._pos;this._peek()!=="]"&&!this._isWhitespace()&&!this._eof();)i+=this._next();this._skipWhitespace(),this._peek()!=="]"&&this._throwError("Expected ]"),this._next(),this._applyAttribute(t,n,i||"true",r)}}_parse(){this._skipWhitespace();const t=this._readIdentifier("role");this._skipWhitespace();const r=this._readStringOrRegex()||"",n={kind:"role",role:t,name:r};return this._readAttributes(n),this._skipWhitespace(),this._eof()||this._throwError("Unexpected input"),n}_applyAttribute(t,r,n,i){if(r==="checked"){this._assert(n==="true"||n==="false"||n==="mixed",\'Value of "checked" attribute must be a boolean or "mixed"\',i),t.checked=n==="true"?!0:n==="false"?!1:"mixed";return}if(r==="disabled"){this._assert(n==="true"||n==="false",\'Value of "disabled" attribute must be a boolean\',i),t.disabled=n==="true";return}if(r==="expanded"){this._assert(n==="true"||n==="false",\'Value of "expanded" attribute must be a boolean\',i),t.expanded=n==="true";return}if(r==="active"){this._assert(n==="true"||n==="false",\'Value of "active" attribute must be a boolean\',i),t.active=n==="true";return}if(r==="level"){this._assert(!isNaN(Number(n)),\'Value of "level" attribute must be a number\',i),t.level=Number(n);return}if(r==="pressed"){this._assert(n==="true"||n==="false"||n==="mixed",\'Value of "pressed" attribute must be a boolean or "mixed"\',i),t.pressed=n==="true"?!0:n==="false"?!1:"mixed";return}if(r==="selected"){this._assert(n==="true"||n==="false",\'Value of "selected" attribute must be a boolean\',i),t.selected=n==="true";return}this._assert(!1,`Unsupported attribute [${r}]`,i)}_assert(t,r,n){t||this._throwError(r||"Assertion error",n)}},Zt=class extends Error{constructor(e,t){super(e),this.pos=t}};function Un(e,t){var r,n;function i(l,c,h){let f=1,u=h+f;for(const p of l.children||[])typeof p=="string"?(f++,u++):(f+=i(p,c,u),u+=f);if(!["none","presentation","fragment","iframe","generic"].includes(l.role)&&l.name){let p=c.get(l.role);p||(p=new Map,c.set(l.role,p));const d=p.get(l.name),g=f*100-h;(!d||d.sizeAndPosition<g)&&p.set(l.name,{node:l,sizeAndPosition:g})}return f}const s=new Map;e&&i(e,s,0);const o=new Map;i(t,o,0);const a=[];for(const[l,c]of o)for(const[h,f]of c)(r=s.get(l))!=null&&r.get(h)||a.push(f);return a.sort((l,c)=>c.sizeAndPosition-l.sizeAndPosition),(n=a[0])==null?void 0:n.node}var T=function(e,t,r){return e>=t&&e<=r};function P(e){return T(e,48,57)}function er(e){return P(e)||T(e,65,70)||T(e,97,102)}function jn(e){return T(e,65,90)}function Wn(e){return T(e,97,122)}function Vn(e){return jn(e)||Wn(e)}function zn(e){return e>=128}function Ve(e){return Vn(e)||zn(e)||e===95}function tr(e){return Ve(e)||P(e)||e===45}function Gn(e){return T(e,0,8)||e===11||T(e,14,31)||e===127}function re(e){return e===10}function V(e){return re(e)||e===9||e===32}var Jn=1114111,gt=class extends Error{constructor(e){super(e),this.name="InvalidCharacterError"}};function Xn(e){const t=[];for(let r=0;r<e.length;r++){let n=e.charCodeAt(r);if(n===13&&e.charCodeAt(r+1)===10&&(n=10,r++),(n===13||n===12)&&(n=10),n===0&&(n=65533),T(n,55296,56319)&&T(e.charCodeAt(r+1),56320,57343)){const i=n-55296,s=e.charCodeAt(r+1)-56320;n=Math.pow(2,16)+i*Math.pow(2,10)+s,r++}t.push(n)}return t}function k(e){if(e<=65535)return String.fromCharCode(e);e-=Math.pow(2,16);const t=Math.floor(e/Math.pow(2,10))+55296,r=e%Math.pow(2,10)+56320;return String.fromCharCode(t)+String.fromCharCode(r)}function rr(e){const t=Xn(e);let r=-1;const n=[];let i,s=0,o=0,a=0;const l=function(){s+=1,a=o,o=0},c={line:s,column:o},h=function(v){return v>=t.length?-1:t[v]},f=function(v){if(v===void 0&&(v=1),v>3)throw"Spec Error: no more than three codepoints of lookahead.";return h(r+v)},u=function(v){return v===void 0&&(v=1),r+=v,i=h(r),re(i)?l():o+=v,!0},p=function(){return r-=1,re(i)?(s-=1,o=a):o-=1,c.line=s,c.column=o,!0},d=function(v){return v===void 0&&(v=i),v===-1},g=function(){},m=function(){},w=function(){if(x(),u(),V(i)){for(;V(f());)u();return new Ge}else{if(i===34)return ge();if(i===35)if(tr(f())||J(f(1),f(2))){const v=new gr("");return R(f(1),f(2),f(3))&&(v.type="id"),v.value=ft(),v}else return new C(i);else return i===36?f()===61?(u(),new Zn):new C(i):i===39?ge():i===40?new fr:i===41?new bt:i===42?f()===61?(u(),new ei):new C(i):i===43?Gt()?(p(),y()):new C(i):i===44?new lr:i===45?Gt()?(p(),y()):f(1)===45&&f(2)===62?(u(2),new sr):D()?(p(),L()):new C(i):i===46?Gt()?(p(),y()):new C(i):i===58?new or:i===59?new ar:i===60?f(1)===33&&f(2)===45&&f(3)===45?(u(3),new ir):new C(i):i===64?R(f(1),f(2),f(3))?new pr(ft()):new C(i):i===91?new hr:i===92?b()?(p(),L()):(m(),new C(i)):i===93?new mt:i===94?f()===61?(u(),new Kn):new C(i):i===123?new cr:i===124?f()===61?(u(),new Yn):f()===124?(u(),new dr):new C(i):i===125?new ur:i===126?f()===61?(u(),new Qn):new C(i):P(i)?(p(),y()):Ve(i)?(p(),L()):d()?new Je:new C(i)}},x=function(){for(;f(1)===47&&f(2)===42;)for(u(2);;)if(u(),i===42&&f()===47){u();break}else if(d()){m();return}},y=function(){const v=io();if(R(f(1),f(2),f(3))){const E=new ti;return E.value=v.value,E.repr=v.repr,E.type=v.type,E.unit=ft(),E}else if(f()===37){u();const E=new vr;return E.value=v.value,E.repr=v.repr,E}else{const E=new br;return E.value=v.value,E.repr=v.repr,E.type=v.type,E}},L=function(){const v=ft();if(v.toLowerCase()==="url"&&f()===40){for(u();V(f(1))&&V(f(2));)u();return f()===34||f()===39?new me(v):V(f())&&(f(2)===34||f(2)===39)?new me(v):K()}else return f()===40?(u(),new me(v)):new vt(v)},ge=function(v){v===void 0&&(v=i);let E="";for(;u();){if(i===v||d())return new wt(E);if(re(i))return m(),p(),new nr;i===92?d(f())?g():re(f())?u():E+=k(A()):E+=k(i)}throw new Error("Internal error")},K=function(){const v=new mr("");for(;V(f());)u();if(d(f()))return v;for(;u();){if(i===41||d())return v;if(V(i)){for(;V(f());)u();return f()===41||d(f())?(u(),v):(Jt(),new ze)}else{if(i===34||i===39||i===40||Gn(i))return m(),Jt(),new ze;if(i===92)if(b())v.value+=k(A());else return m(),Jt(),new ze;else v.value+=k(i)}}throw new Error("Internal error")},A=function(){if(u(),er(i)){const v=[i];for(let B=0;B<5&&er(f());B++)u(),v.push(i);V(f())&&u();let E=parseInt(v.map(function(B){return String.fromCharCode(B)}).join(""),16);return E>Jn&&(E=65533),E}else return d()?65533:i},J=function(v,E){return!(v!==92||re(E))},b=function(){return J(i,f())},R=function(v,E,B){return v===45?Ve(E)||E===45||J(E,B):Ve(v)?!0:v===92?J(v,E):!1},D=function(){return R(i,f(1),f(2))},no=function(v,E,B){return v===43||v===45?!!(P(E)||E===46&&P(B)):v===46?!!P(E):!!P(v)},Gt=function(){return no(i,f(1),f(2))},ft=function(){let v="";for(;u();)if(tr(i))v+=k(i);else if(b())v+=k(A());else return p(),v;throw new Error("Internal parse error")},io=function(){let v="",E="integer";for((f()===43||f()===45)&&(u(),v+=k(i));P(f());)u(),v+=k(i);if(f(1)===46&&P(f(2)))for(u(),v+=k(i),u(),v+=k(i),E="number";P(f());)u(),v+=k(i);const B=f(1),Xt=f(2),oo=f(3);if((B===69||B===101)&&P(Xt))for(u(),v+=k(i),u(),v+=k(i),E="number";P(f());)u(),v+=k(i);else if((B===69||B===101)&&(Xt===43||Xt===45)&&P(oo))for(u(),v+=k(i),u(),v+=k(i),u(),v+=k(i),E="number";P(f());)u(),v+=k(i);const ao=so(v);return{type:E,value:ao,repr:v}},so=function(v){return+v},Jt=function(){for(;u();){if(i===41||d())return;b()&&A(),g()}};let On=0;for(;!d(f());)if(n.push(w()),On++,On>t.length*2)throw new Error("I\'m infinite-looping!");return n}var S=class{constructor(){this.tokenType=""}toJSON(){return{token:this.tokenType}}toString(){return this.tokenType}toSource(){return""+this}},nr=class extends S{constructor(){super(...arguments),this.tokenType="BADSTRING"}},ze=class extends S{constructor(){super(...arguments),this.tokenType="BADURL"}},Ge=class extends S{constructor(){super(...arguments),this.tokenType="WHITESPACE"}toString(){return"WS"}toSource(){return" "}},ir=class extends S{constructor(){super(...arguments),this.tokenType="CDO"}toSource(){return"<!--"}},sr=class extends S{constructor(){super(...arguments),this.tokenType="CDC"}toSource(){return"-->"}},or=class extends S{constructor(){super(...arguments),this.tokenType=":"}},ar=class extends S{constructor(){super(...arguments),this.tokenType=";"}},lr=class extends S{constructor(){super(...arguments),this.tokenType=","}},ne=class extends S{constructor(){super(...arguments),this.value="",this.mirror=""}},cr=class extends ne{constructor(){super(),this.tokenType="{",this.value="{",this.mirror="}"}},ur=class extends ne{constructor(){super(),this.tokenType="}",this.value="}",this.mirror="{"}},hr=class extends ne{constructor(){super(),this.tokenType="[",this.value="[",this.mirror="]"}},mt=class extends ne{constructor(){super(),this.tokenType="]",this.value="]",this.mirror="["}},fr=class extends ne{constructor(){super(),this.tokenType="(",this.value="(",this.mirror=")"}},bt=class extends ne{constructor(){super(),this.tokenType=")",this.value=")",this.mirror="("}},Qn=class extends S{constructor(){super(...arguments),this.tokenType="~="}},Yn=class extends S{constructor(){super(...arguments),this.tokenType="|="}},Kn=class extends S{constructor(){super(...arguments),this.tokenType="^="}},Zn=class extends S{constructor(){super(...arguments),this.tokenType="$="}},ei=class extends S{constructor(){super(...arguments),this.tokenType="*="}},dr=class extends S{constructor(){super(...arguments),this.tokenType="||"}},Je=class extends S{constructor(){super(...arguments),this.tokenType="EOF"}toSource(){return""}},C=class extends S{constructor(e){super(),this.tokenType="DELIM",this.value="",this.value=k(e)}toString(){return"DELIM("+this.value+")"}toJSON(){const e=this.constructor.prototype.constructor.prototype.toJSON.call(this);return e.value=this.value,e}toSource(){return this.value==="\\\\"?`\\\\\n`:this.value}},ie=class extends S{constructor(){super(...arguments),this.value=""}ASCIIMatch(e){return this.value.toLowerCase()===e.toLowerCase()}toJSON(){const e=this.constructor.prototype.constructor.prototype.toJSON.call(this);return e.value=this.value,e}},vt=class extends ie{constructor(e){super(),this.tokenType="IDENT",this.value=e}toString(){return"IDENT("+this.value+")"}toSource(){return be(this.value)}},me=class extends ie{constructor(e){super(),this.tokenType="FUNCTION",this.value=e,this.mirror=")"}toString(){return"FUNCTION("+this.value+")"}toSource(){return be(this.value)+"("}},pr=class extends ie{constructor(e){super(),this.tokenType="AT-KEYWORD",this.value=e}toString(){return"AT("+this.value+")"}toSource(){return"@"+be(this.value)}},gr=class extends ie{constructor(e){super(),this.tokenType="HASH",this.value=e,this.type="unrestricted"}toString(){return"HASH("+this.value+")"}toJSON(){const e=this.constructor.prototype.constructor.prototype.toJSON.call(this);return e.value=this.value,e.type=this.type,e}toSource(){return this.type==="id"?"#"+be(this.value):"#"+ri(this.value)}},wt=class extends ie{constructor(e){super(),this.tokenType="STRING",this.value=e}toString(){return\'"\'+wr(this.value)+\'"\'}},mr=class extends ie{constructor(e){super(),this.tokenType="URL",this.value=e}toString(){return"URL("+this.value+")"}toSource(){return\'url("\'+wr(this.value)+\'")\'}},br=class extends S{constructor(){super(),this.tokenType="NUMBER",this.type="integer",this.repr=""}toString(){return this.type==="integer"?"INT("+this.value+")":"NUMBER("+this.value+")"}toJSON(){const e=super.toJSON();return e.value=this.value,e.type=this.type,e.repr=this.repr,e}toSource(){return this.repr}},vr=class extends S{constructor(){super(),this.tokenType="PERCENTAGE",this.repr=""}toString(){return"PERCENTAGE("+this.value+")"}toJSON(){const e=this.constructor.prototype.constructor.prototype.toJSON.call(this);return e.value=this.value,e.repr=this.repr,e}toSource(){return this.repr+"%"}},ti=class extends S{constructor(){super(),this.tokenType="DIMENSION",this.type="integer",this.repr="",this.unit=""}toString(){return"DIM("+this.value+","+this.unit+")"}toJSON(){const e=this.constructor.prototype.constructor.prototype.toJSON.call(this);return e.value=this.value,e.type=this.type,e.repr=this.repr,e.unit=this.unit,e}toSource(){const e=this.repr;let t=be(this.unit);return t[0].toLowerCase()==="e"&&(t[1]==="-"||T(t.charCodeAt(1),48,57))&&(t="\\\\65 "+t.slice(1,t.length)),e+t}};function be(e){e=""+e;let t="";const r=e.charCodeAt(0);for(let n=0;n<e.length;n++){const i=e.charCodeAt(n);if(i===0)throw new gt("Invalid character: the input contains U+0000.");T(i,1,31)||i===127||n===0&&T(i,48,57)||n===1&&T(i,48,57)&&r===45?t+="\\\\"+i.toString(16)+" ":i>=128||i===45||i===95||T(i,48,57)||T(i,65,90)||T(i,97,122)?t+=e[n]:t+="\\\\"+e[n]}return t}function ri(e){e=""+e;let t="";for(let r=0;r<e.length;r++){const n=e.charCodeAt(r);if(n===0)throw new gt("Invalid character: the input contains U+0000.");n>=128||n===45||n===95||T(n,48,57)||T(n,65,90)||T(n,97,122)?t+=e[r]:t+="\\\\"+n.toString(16)+" "}return t}function wr(e){e=""+e;let t="";for(let r=0;r<e.length;r++){const n=e.charCodeAt(r);if(n===0)throw new gt("Invalid character: the input contains U+0000.");T(n,1,31)||n===127?t+="\\\\"+n.toString(16)+" ":n===34||n===92?t+="\\\\"+e[r]:t+=e[r]}return t}var M=class extends Error{};function ni(e,t){let r;try{r=rr(e),r[r.length-1]instanceof Je||r.push(new Je)}catch(b){const R=b.message+` while parsing css selector "${e}". Did you mean to CSS.escape it?`,D=(b.stack||"").indexOf(b.message);throw D!==-1&&(b.stack=b.stack.substring(0,D)+R+b.stack.substring(D+b.message.length)),b.message=R,b}const n=r.find(b=>b instanceof pr||b instanceof nr||b instanceof ze||b instanceof dr||b instanceof ir||b instanceof sr||b instanceof ar||b instanceof cr||b instanceof ur||b instanceof mr||b instanceof vr);if(n)throw new M(`Unsupported token "${n.toSource()}" while parsing css selector "${e}". Did you mean to CSS.escape it?`);let i=0;const s=new Set;function o(){return new M(`Unexpected token "${r[i].toSource()}" while parsing css selector "${e}". Did you mean to CSS.escape it?`)}function a(){for(;r[i]instanceof Ge;)i++}function l(b=i){return r[b]instanceof vt}function c(b=i){return r[b]instanceof wt}function h(b=i){return r[b]instanceof br}function f(b=i){return r[b]instanceof lr}function u(b=i){return r[b]instanceof fr}function p(b=i){return r[b]instanceof bt}function d(b=i){return r[b]instanceof me}function g(b=i){return r[b]instanceof C&&r[b].value==="*"}function m(b=i){return r[b]instanceof Je}function w(b=i){return r[b]instanceof C&&[">","+","~"].includes(r[b].value)}function x(b=i){return f(b)||p(b)||m(b)||w(b)||r[b]instanceof Ge}function y(){const b=[L()];for(;a(),!!f();)i++,b.push(L());return b}function L(){return a(),h()||c()?r[i++].value:ge()}function ge(){const b={simples:[]};for(a(),w()?b.simples.push({selector:{functions:[{name:"scope",args:[]}]},combinator:""}):b.simples.push({selector:K(),combinator:""});;){if(a(),w())b.simples[b.simples.length-1].combinator=r[i++].value,a();else if(x())break;b.simples.push({combinator:"",selector:K()})}return b}function K(){let b="";const R=[];for(;!x();)if(l()||g())b+=r[i++].toSource();else if(r[i]instanceof gr)b+=r[i++].toSource();else if(r[i]instanceof C&&r[i].value===".")if(i++,l())b+="."+r[i++].toSource();else throw o();else if(r[i]instanceof or)if(i++,l())if(!t.has(r[i].value.toLowerCase()))b+=":"+r[i++].toSource();else{const D=r[i++].value.toLowerCase();R.push({name:D,args:[]}),s.add(D)}else if(d()){const D=r[i++].value.toLowerCase();if(t.has(D)?(R.push({name:D,args:y()}),s.add(D)):b+=`:${D}(${A()})`,a(),!p())throw o();i++}else throw o();else if(r[i]instanceof hr){for(b+="[",i++;!(r[i]instanceof mt)&&!m();)b+=r[i++].toSource();if(!(r[i]instanceof mt))throw o();b+="]",i++}else throw o();if(!b&&!R.length)throw o();return{css:b||void 0,functions:R}}function A(){let b="",R=1;for(;!m()&&((u()||d())&&R++,p()&&R--,!!R);)b+=r[i++].toSource();return b}const J=y();if(!m())throw o();if(J.some(b=>typeof b!="object"||!("simples"in b)))throw new M(`Error while parsing css selector "${e}". Did you mean to CSS.escape it?`);return{selector:J,names:Array.from(s)}}var xt=new Set(["internal:has","internal:has-not","internal:and","internal:or","internal:chain","left-of","right-of","above","below","near"]),ii=new Set(["left-of","right-of","above","below","near"]),xr=new Set(["not","is","where","has","scope","light","visible","text","text-matches","text-is","has-text","above","below","right-of","left-of","near","nth-match"]);function yt(e){const t=ai(e),r=[];for(const n of t.parts){if(n.name==="css"||n.name==="css:light"){n.name==="css:light"&&(n.body=":light("+n.body+")");const i=ni(n.body,xr);r.push({name:"css",body:i.selector,source:n.body});continue}if(xt.has(n.name)){let i,s;try{const c=JSON.parse("["+n.body+"]");if(!Array.isArray(c)||c.length<1||c.length>2||typeof c[0]!="string")throw new M(`Malformed selector: ${n.name}=`+n.body);if(i=c[0],c.length===2){if(typeof c[1]!="number"||!ii.has(n.name))throw new M(`Malformed selector: ${n.name}=`+n.body);s=c[1]}}catch{throw new M(`Malformed selector: ${n.name}=`+n.body)}const o={name:n.name,source:n.body,body:{parsed:yt(i),distance:s}},a=[...o.body.parsed.parts].reverse().find(c=>c.name==="internal:control"&&c.body==="enter-frame"),l=a?o.body.parsed.parts.indexOf(a):-1;l!==-1&&si(o.body.parsed.parts.slice(0,l+1),r.slice(0,l+1))&&o.body.parsed.parts.splice(0,l+1),r.push(o);continue}r.push({...n,source:n.body})}if(xt.has(r[0].name))throw new M(`"${r[0].name}" selector cannot be first`);return{capture:t.capture,parts:r}}function si(e,t){return W({parts:e})===W({parts:t})}function W(e,t){return typeof e=="string"?e:e.parts.map((r,n)=>{let i=!0;!t&&n!==e.capture&&(r.name==="css"||r.name==="xpath"&&r.source.startsWith("//")||r.source.startsWith(".."))&&(i=!1);const s=i?r.name+"=":"";return`${n===e.capture?"*":""}${s}${r.source}`}).join(" >> ")}function oi(e,t){const r=(n,i)=>{for(const s of n.parts)t(s,i),xt.has(s.name)&&r(s.body.parsed,!0)};r(e,!1)}function ai(e){let t=0,r,n=0;const i={parts:[]},s=()=>{const a=e.substring(n,t).trim(),l=a.indexOf("=");let c,h;l!==-1&&a.substring(0,l).trim().match(/^[a-zA-Z_0-9-+:*]+$/)?(c=a.substring(0,l).trim(),h=a.substring(l+1)):a.length>1&&a[0]===\'"\'&&a[a.length-1]===\'"\'||a.length>1&&a[0]==="\'"&&a[a.length-1]==="\'"?(c="text",h=a):/^\\(*\\/\\//.test(a)||a.startsWith("..")?(c="xpath",h=a):(c="css",h=a);let f=!1;if(c[0]==="*"&&(f=!0,c=c.substring(1)),i.parts.push({name:c,body:h}),f){if(i.capture!==void 0)throw new M("Only one of the selectors can capture using * modifier");i.capture=i.parts.length-1}};if(!e.includes(">>"))return t=e.length,s(),i;const o=()=>{const l=e.substring(n,t).match(/^\\s*text\\s*=(.*)$/);return!!l&&!!l[1]};for(;t<e.length;){const a=e[t];a==="\\\\"&&t+1<e.length?t+=2:a===r?(r=void 0,t++):!r&&(a===\'"\'||a==="\'"||a==="`")&&!o()?(r=a,t++):!r&&a===">"&&e[t+1]===">"?(s(),t+=2,n=t):t++}return s(),i}function ve(e,t){let r=0,n=e.length===0;const i=()=>e[r]||"",s=()=>{const m=i();return++r,n=r>=e.length,m},o=m=>{throw n?new M(`Unexpected end of selector while parsing selector \\`${e}\\``):new M(`Error while parsing selector \\`${e}\\` - unexpected symbol "${i()}" at position ${r}`+(m?" during "+m:""))};function a(){for(;!n&&/\\s/.test(i());)s()}function l(m){return m>="\\x80"||m>="0"&&m<="9"||m>="A"&&m<="Z"||m>="a"&&m<="z"||m>="0"&&m<="9"||m==="_"||m==="-"}function c(){let m="";for(a();!n&&l(i());)m+=s();return m}function h(m){let w=s();for(w!==m&&o("parsing quoted string");!n&&i()!==m;)i()==="\\\\"&&s(),w+=s();return i()!==m&&o("parsing quoted string"),w+=s(),w}function f(){s()!=="/"&&o("parsing regular expression");let m="",w=!1;for(;!n;){if(i()==="\\\\")m+=s(),n&&o("parsing regular expression");else if(w&&i()==="]")w=!1;else if(!w&&i()==="[")w=!0;else if(!w&&i()==="/")break;m+=s()}s()!=="/"&&o("parsing regular expression");let x="";for(;!n&&i().match(/[dgimsuy]/);)x+=s();try{return new RegExp(m,x)}catch(y){throw new M(`Error while parsing selector \\`${e}\\`: ${y.message}`)}}function u(){let m="";return a(),i()==="\'"||i()===\'"\'?m=h(i()).slice(1,-1):m=c(),m||o("parsing property path"),m}function p(){a();let m="";return n||(m+=s()),!n&&m!=="="&&(m+=s()),["=","*=","^=","$=","|=","~="].includes(m)||o("parsing operator"),m}function d(){s();const m=[];for(m.push(u()),a();i()===".";)s(),m.push(u()),a();if(i()==="]")return s(),{name:m.join("."),jsonPath:m,op:"<truthy>",value:null,caseSensitive:!1};const w=p();let x,y=!0;if(a(),i()==="/"){if(w!=="=")throw new M(`Error while parsing selector \\`${e}\\` - cannot use ${w} in attribute with regular expression`);x=f()}else if(i()==="\'"||i()===\'"\')x=h(i()).slice(1,-1),a(),i()==="i"||i()==="I"?(y=!1,s()):(i()==="s"||i()==="S")&&(y=!0,s());else{for(x="";!n&&(l(i())||i()==="+"||i()===".");)x+=s();x==="true"?x=!0:x==="false"?x=!1:t||(x=+x,Number.isNaN(x)&&o("parsing attribute value"))}if(a(),i()!=="]"&&o("parsing attribute value"),s(),w!=="="&&typeof x!="string")throw new M(`Error while parsing selector \\`${e}\\` - cannot use ${w} in attribute with non-string matching value - ${x}`);return{name:m.join("."),jsonPath:m,op:w,value:x,caseSensitive:y}}const g={name:"",attributes:[]};for(g.name=c(),a();i()==="[";)g.attributes.push(d()),a();if(n||o(void 0),!g.name&&!g.attributes.length)throw new M(`Error while parsing selector \\`${e}\\` - selector cannot be empty`);return g}function Xe(e,t="\'"){const r=JSON.stringify(e),n=r.substring(1,r.length-1).replace(/\\\\"/g,\'"\');if(t==="\'")return t+n.replace(/[\']/g,"\\\\\'")+t;if(t===\'"\')return t+n.replace(/["]/g,\'\\\\"\')+t;if(t==="`")return t+n.replace(/[`]/g,"\\\\`")+t;throw new Error("Invalid escape char")}function Qe(e){return e.charAt(0).toUpperCase()+e.substring(1)}function yr(e){return e.replace(/([a-z0-9])([A-Z])/g,"$1_$2").replace(/([A-Z])([A-Z][a-z])/g,"$1_$2").toLowerCase()}function se(e){return`"${e.replace(/["\\\\]/g,t=>"\\\\"+t)}"`}var we;function li(){we=new Map}function O(e){let t=we?.get(e);return t===void 0&&(t=e.replace(/[\\u200b\\u00ad]/g,"").trim().replace(/\\s+/g," "),we?.set(e,t)),t}function Ye(e){return e.replace(/(^|[^\\\\])(\\\\\\\\)*\\\\([\'"`])/g,"$1$2$3")}function Er(e){return e.unicode||e.unicodeSets?String(e):String(e).replace(/(^|[^\\\\])(\\\\\\\\)*(["\'`])/g,"$1$2\\\\$3").replace(/>>/g,"\\\\>\\\\>")}function H(e,t){return typeof e!="string"?Er(e):`${JSON.stringify(e)}${t?"s":"i"}`}function F(e,t){return typeof e!="string"?Er(e):`"${e.replace(/\\\\/g,"\\\\\\\\").replace(/["]/g,\'\\\\"\')}"${t?"s":"i"}`}function ci(e,t,r=""){if(e.length<=t)return e;const n=[...e];return n.length>t?n.slice(0,t-r.length).join("")+r:n.join("")}function Sr(e,t){return ci(e,t,"\\u2026")}function Ke(e){return e.replace(/[.*+?^${}()|[\\]\\\\]/g,"\\\\$&")}function ui(e,t){const r=e.length,n=t.length;let i=0,s=0;const o=Array(r+1).fill(null).map(()=>Array(n+1).fill(0));for(let a=1;a<=r;a++)for(let l=1;l<=n;l++)e[a-1]===t[l-1]&&(o[a][l]=o[a-1][l-1]+1,o[a][l]>i&&(i=o[a][l],s=a));return e.slice(s-i,s)}var uo=new RegExp("([\\\\u001B\\\\u009B][[\\\\]()#;?]*(?:(?:(?:[a-zA-Z\\\\d]*(?:;[-a-zA-Z\\\\d\\\\/#&.:=?%@~_]*)*)?\\\\u0007)|(?:(?:\\\\d{1,4}(?:;\\\\d{0,4})*)?[\\\\dA-PR-TZcf-ntqry=><~])))","g");function oe(e,t,r=!1){return hi(e,t,r,1)[0]}function hi(e,t,r=!1,n=20,i){try{return ae(new vi[e](i),yt(t),r,n)}catch{return[t]}}function ae(e,t,r=!1,n=20){const i=[...t.parts],s=[];let o=r?"frame-locator":"page";for(let a=0;a<i.length;a++){const l=i[a],c=o;if(o="locator",l.name==="internal:describe")continue;if(l.name==="nth"){l.body==="0"?s.push([e.generateLocator(c,"first",""),e.generateLocator(c,"nth","0")]):l.body==="-1"?s.push([e.generateLocator(c,"last",""),e.generateLocator(c,"nth","-1")]):s.push([e.generateLocator(c,"nth",l.body)]);continue}if(l.name==="visible"){s.push([e.generateLocator(c,"visible",l.body),e.generateLocator(c,"default",`visible=${l.body}`)]);continue}if(l.name==="internal:text"){const{exact:d,text:g}=xe(l.body);s.push([e.generateLocator(c,"text",g,{exact:d})]);continue}if(l.name==="internal:has-text"){const{exact:d,text:g}=xe(l.body);if(!d){s.push([e.generateLocator(c,"has-text",g,{exact:d})]);continue}}if(l.name==="internal:has-not-text"){const{exact:d,text:g}=xe(l.body);if(!d){s.push([e.generateLocator(c,"has-not-text",g,{exact:d})]);continue}}if(l.name==="internal:has"){const d=ae(e,l.body.parsed,!1,n);s.push(d.map(g=>e.generateLocator(c,"has",g)));continue}if(l.name==="internal:has-not"){const d=ae(e,l.body.parsed,!1,n);s.push(d.map(g=>e.generateLocator(c,"hasNot",g)));continue}if(l.name==="internal:and"){const d=ae(e,l.body.parsed,!1,n);s.push(d.map(g=>e.generateLocator(c,"and",g)));continue}if(l.name==="internal:or"){const d=ae(e,l.body.parsed,!1,n);s.push(d.map(g=>e.generateLocator(c,"or",g)));continue}if(l.name==="internal:chain"){const d=ae(e,l.body.parsed,!1,n);s.push(d.map(g=>e.generateLocator(c,"chain",g)));continue}if(l.name==="internal:label"){const{exact:d,text:g}=xe(l.body);s.push([e.generateLocator(c,"label",g,{exact:d})]);continue}if(l.name==="internal:role"){const d=ve(l.body,!0),g={attrs:[]};for(const m of d.attributes)m.name==="name"?(g.exact=m.caseSensitive,g.name=m.value):(m.name==="level"&&typeof m.value=="string"&&(m.value=+m.value),g.attrs.push({name:m.name==="include-hidden"?"includeHidden":m.name,value:m.value}));s.push([e.generateLocator(c,"role",d.name,g)]);continue}if(l.name==="internal:testid"){const d=ve(l.body,!0),{value:g}=d.attributes[0];s.push([e.generateLocator(c,"test-id",g)]);continue}if(l.name==="internal:attr"){const d=ve(l.body,!0),{name:g,value:m,caseSensitive:w}=d.attributes[0],x=m,y=!!w;if(g==="placeholder"){s.push([e.generateLocator(c,"placeholder",x,{exact:y})]);continue}if(g==="alt"){s.push([e.generateLocator(c,"alt",x,{exact:y})]);continue}if(g==="title"){s.push([e.generateLocator(c,"title",x,{exact:y})]);continue}}if(l.name==="internal:control"&&l.body==="enter-frame"){const d=s[s.length-1],g=i[a-1],m=d.map(w=>e.chainLocators([w,e.generateLocator(c,"frame","")]));["xpath","css"].includes(g.name)&&m.push(e.generateLocator(c,"frame-locator",W({parts:[g]})),e.generateLocator(c,"frame-locator",W({parts:[g]},!0))),d.splice(0,d.length,...m),o="frame-locator";continue}const h=i[a+1],f=W({parts:[l]}),u=e.generateLocator(c,"default",f);if(h&&["internal:has-text","internal:has-not-text"].includes(h.name)){const{exact:d,text:g}=xe(h.body);if(!d){const m=e.generateLocator("locator",h.name==="internal:has-text"?"has-text":"has-not-text",g,{exact:d}),w={};h.name==="internal:has-text"?w.hasText=g:w.hasNotText=g;const x=e.generateLocator(c,"default",f,w);s.push([e.chainLocators([u,m]),x]),a++;continue}}let p;if(["xpath","css"].includes(l.name)){const d=W({parts:[l]},!0);p=e.generateLocator(c,"default",d)}s.push([u,p].filter(Boolean))}return fi(e,s,n)}function fi(e,t,r){const n=t.map(()=>""),i=[],s=o=>{if(o===t.length)return i.push(e.chainLocators(n)),i.length<r;for(const a of t[o])if(n[o]=a,!s(o+1))return!1;return!0};return s(0),i}function xe(e){let t=!1;const r=e.match(/^\\/(.*)\\/([igm]*)$/);return r?{text:new RegExp(r[1],r[2])}:(e.endsWith(\'"\')?(e=JSON.parse(e),t=!0):e.endsWith(\'"s\')?(e=JSON.parse(e.substring(0,e.length-1)),t=!0):e.endsWith(\'"i\')&&(e=JSON.parse(e.substring(0,e.length-1)),t=!1),{exact:t,text:e})}var di=class{constructor(e){this.preferredQuote=e}generateLocator(e,t,r,n={}){switch(t){case"default":return n.hasText!==void 0?`locator(${this.quote(r)}, { hasText: ${this.toHasText(n.hasText)} })`:n.hasNotText!==void 0?`locator(${this.quote(r)}, { hasNotText: ${this.toHasText(n.hasNotText)} })`:`locator(${this.quote(r)})`;case"frame-locator":return`frameLocator(${this.quote(r)})`;case"frame":return"contentFrame()";case"nth":return`nth(${r})`;case"first":return"first()";case"last":return"last()";case"visible":return`filter({ visible: ${r==="true"?"true":"false"} })`;case"role":const i=[];N(n.name)?i.push(`name: ${this.regexToSourceString(n.name)}`):typeof n.name=="string"&&(i.push(`name: ${this.quote(n.name)}`),n.exact&&i.push("exact: true"));for(const{name:o,value:a}of n.attrs)i.push(`${o}: ${typeof a=="string"?this.quote(a):a}`);const s=i.length?`, { ${i.join(", ")} }`:"";return`getByRole(${this.quote(r)}${s})`;case"has-text":return`filter({ hasText: ${this.toHasText(r)} })`;case"has-not-text":return`filter({ hasNotText: ${this.toHasText(r)} })`;case"has":return`filter({ has: ${r} })`;case"hasNot":return`filter({ hasNot: ${r} })`;case"and":return`and(${r})`;case"or":return`or(${r})`;case"chain":return`locator(${r})`;case"test-id":return`getByTestId(${this.toTestIdValue(r)})`;case"text":return this.toCallWithExact("getByText",r,!!n.exact);case"alt":return this.toCallWithExact("getByAltText",r,!!n.exact);case"placeholder":return this.toCallWithExact("getByPlaceholder",r,!!n.exact);case"label":return this.toCallWithExact("getByLabel",r,!!n.exact);case"title":return this.toCallWithExact("getByTitle",r,!!n.exact);default:throw new Error("Unknown selector kind "+t)}}chainLocators(e){return e.join(".")}regexToSourceString(e){return Ye(String(e))}toCallWithExact(e,t,r){return N(t)?`${e}(${this.regexToSourceString(t)})`:r?`${e}(${this.quote(t)}, { exact: true })`:`${e}(${this.quote(t)})`}toHasText(e){return N(e)?this.regexToSourceString(e):this.quote(e)}toTestIdValue(e){return N(e)?this.regexToSourceString(e):this.quote(e)}quote(e){var t;return Xe(e,(t=this.preferredQuote)!=null?t:"\'")}},pi=class{generateLocator(e,t,r,n={}){switch(t){case"default":return n.hasText!==void 0?`locator(${this.quote(r)}, has_text=${this.toHasText(n.hasText)})`:n.hasNotText!==void 0?`locator(${this.quote(r)}, has_not_text=${this.toHasText(n.hasNotText)})`:`locator(${this.quote(r)})`;case"frame-locator":return`frame_locator(${this.quote(r)})`;case"frame":return"content_frame";case"nth":return`nth(${r})`;case"first":return"first";case"last":return"last";case"visible":return`filter(visible=${r==="true"?"True":"False"})`;case"role":const i=[];N(n.name)?i.push(`name=${this.regexToString(n.name)}`):typeof n.name=="string"&&(i.push(`name=${this.quote(n.name)}`),n.exact&&i.push("exact=True"));for(const{name:o,value:a}of n.attrs){let l=typeof a=="string"?this.quote(a):a;typeof a=="boolean"&&(l=a?"True":"False"),i.push(`${yr(o)}=${l}`)}const s=i.length?`, ${i.join(", ")}`:"";return`get_by_role(${this.quote(r)}${s})`;case"has-text":return`filter(has_text=${this.toHasText(r)})`;case"has-not-text":return`filter(has_not_text=${this.toHasText(r)})`;case"has":return`filter(has=${r})`;case"hasNot":return`filter(has_not=${r})`;case"and":return`and_(${r})`;case"or":return`or_(${r})`;case"chain":return`locator(${r})`;case"test-id":return`get_by_test_id(${this.toTestIdValue(r)})`;case"text":return this.toCallWithExact("get_by_text",r,!!n.exact);case"alt":return this.toCallWithExact("get_by_alt_text",r,!!n.exact);case"placeholder":return this.toCallWithExact("get_by_placeholder",r,!!n.exact);case"label":return this.toCallWithExact("get_by_label",r,!!n.exact);case"title":return this.toCallWithExact("get_by_title",r,!!n.exact);default:throw new Error("Unknown selector kind "+t)}}chainLocators(e){return e.join(".")}regexToString(e){const t=e.flags.includes("i")?", re.IGNORECASE":"";return`re.compile(r"${Ye(e.source).replace(/\\\\\\//,"/").replace(/"/g,\'\\\\"\')}"${t})`}toCallWithExact(e,t,r){return N(t)?`${e}(${this.regexToString(t)})`:r?`${e}(${this.quote(t)}, exact=True)`:`${e}(${this.quote(t)})`}toHasText(e){return N(e)?this.regexToString(e):`${this.quote(e)}`}toTestIdValue(e){return N(e)?this.regexToString(e):this.quote(e)}quote(e){return Xe(e,\'"\')}},gi=class{generateLocator(e,t,r,n={}){let i;switch(e){case"page":i="Page";break;case"frame-locator":i="FrameLocator";break;case"locator":i="Locator";break}switch(t){case"default":return n.hasText!==void 0?`locator(${this.quote(r)}, new ${i}.LocatorOptions().setHasText(${this.toHasText(n.hasText)}))`:n.hasNotText!==void 0?`locator(${this.quote(r)}, new ${i}.LocatorOptions().setHasNotText(${this.toHasText(n.hasNotText)}))`:`locator(${this.quote(r)})`;case"frame-locator":return`frameLocator(${this.quote(r)})`;case"frame":return"contentFrame()";case"nth":return`nth(${r})`;case"first":return"first()";case"last":return"last()";case"visible":return`filter(new ${i}.FilterOptions().setVisible(${r==="true"?"true":"false"}))`;case"role":const s=[];N(n.name)?s.push(`.setName(${this.regexToString(n.name)})`):typeof n.name=="string"&&(s.push(`.setName(${this.quote(n.name)})`),n.exact&&s.push(".setExact(true)"));for(const{name:a,value:l}of n.attrs)s.push(`.set${Qe(a)}(${typeof l=="string"?this.quote(l):l})`);const o=s.length?`, new ${i}.GetByRoleOptions()${s.join("")}`:"";return`getByRole(AriaRole.${yr(r).toUpperCase()}${o})`;case"has-text":return`filter(new ${i}.FilterOptions().setHasText(${this.toHasText(r)}))`;case"has-not-text":return`filter(new ${i}.FilterOptions().setHasNotText(${this.toHasText(r)}))`;case"has":return`filter(new ${i}.FilterOptions().setHas(${r}))`;case"hasNot":return`filter(new ${i}.FilterOptions().setHasNot(${r}))`;case"and":return`and(${r})`;case"or":return`or(${r})`;case"chain":return`locator(${r})`;case"test-id":return`getByTestId(${this.toTestIdValue(r)})`;case"text":return this.toCallWithExact(i,"getByText",r,!!n.exact);case"alt":return this.toCallWithExact(i,"getByAltText",r,!!n.exact);case"placeholder":return this.toCallWithExact(i,"getByPlaceholder",r,!!n.exact);case"label":return this.toCallWithExact(i,"getByLabel",r,!!n.exact);case"title":return this.toCallWithExact(i,"getByTitle",r,!!n.exact);default:throw new Error("Unknown selector kind "+t)}}chainLocators(e){return e.join(".")}regexToString(e){const t=e.flags.includes("i")?", Pattern.CASE_INSENSITIVE":"";return`Pattern.compile(${this.quote(Ye(e.source))}${t})`}toCallWithExact(e,t,r,n){return N(r)?`${t}(${this.regexToString(r)})`:n?`${t}(${this.quote(r)}, new ${e}.${Qe(t)}Options().setExact(true))`:`${t}(${this.quote(r)})`}toHasText(e){return N(e)?this.regexToString(e):this.quote(e)}toTestIdValue(e){return N(e)?this.regexToString(e):this.quote(e)}quote(e){return Xe(e,\'"\')}},mi=class{generateLocator(e,t,r,n={}){switch(t){case"default":return n.hasText!==void 0?`Locator(${this.quote(r)}, new() { ${this.toHasText(n.hasText)} })`:n.hasNotText!==void 0?`Locator(${this.quote(r)}, new() { ${this.toHasNotText(n.hasNotText)} })`:`Locator(${this.quote(r)})`;case"frame-locator":return`FrameLocator(${this.quote(r)})`;case"frame":return"ContentFrame";case"nth":return`Nth(${r})`;case"first":return"First";case"last":return"Last";case"visible":return`Filter(new() { Visible = ${r==="true"?"true":"false"} })`;case"role":const i=[];N(n.name)?i.push(`NameRegex = ${this.regexToString(n.name)}`):typeof n.name=="string"&&(i.push(`Name = ${this.quote(n.name)}`),n.exact&&i.push("Exact = true"));for(const{name:o,value:a}of n.attrs)i.push(`${Qe(o)} = ${typeof a=="string"?this.quote(a):a}`);const s=i.length?`, new() { ${i.join(", ")} }`:"";return`GetByRole(AriaRole.${Qe(r)}${s})`;case"has-text":return`Filter(new() { ${this.toHasText(r)} })`;case"has-not-text":return`Filter(new() { ${this.toHasNotText(r)} })`;case"has":return`Filter(new() { Has = ${r} })`;case"hasNot":return`Filter(new() { HasNot = ${r} })`;case"and":return`And(${r})`;case"or":return`Or(${r})`;case"chain":return`Locator(${r})`;case"test-id":return`GetByTestId(${this.toTestIdValue(r)})`;case"text":return this.toCallWithExact("GetByText",r,!!n.exact);case"alt":return this.toCallWithExact("GetByAltText",r,!!n.exact);case"placeholder":return this.toCallWithExact("GetByPlaceholder",r,!!n.exact);case"label":return this.toCallWithExact("GetByLabel",r,!!n.exact);case"title":return this.toCallWithExact("GetByTitle",r,!!n.exact);default:throw new Error("Unknown selector kind "+t)}}chainLocators(e){return e.join(".")}regexToString(e){const t=e.flags.includes("i")?", RegexOptions.IgnoreCase":"";return`new Regex(${this.quote(Ye(e.source))}${t})`}toCallWithExact(e,t,r){return N(t)?`${e}(${this.regexToString(t)})`:r?`${e}(${this.quote(t)}, new() { Exact = true })`:`${e}(${this.quote(t)})`}toHasText(e){return N(e)?`HasTextRegex = ${this.regexToString(e)}`:`HasText = ${this.quote(e)}`}toTestIdValue(e){return N(e)?this.regexToString(e):this.quote(e)}toHasNotText(e){return N(e)?`HasNotTextRegex = ${this.regexToString(e)}`:`HasNotText = ${this.quote(e)}`}quote(e){return Xe(e,\'"\')}},bi=class{generateLocator(e,t,r,n={}){return JSON.stringify({kind:t,body:r,options:n})}chainLocators(e){const t=e.map(r=>JSON.parse(r));for(let r=0;r<t.length-1;++r)t[r].next=t[r+1];return JSON.stringify(t[0])}},vi={javascript:di,python:pi,java:gi,csharp:mi,jsonl:bi};function N(e){return e instanceof RegExp}function wi(e){return _r(e)?"\'"+e.replace(/\'/g,"\'\'")+"\'":e}function Et(e){return _r(e)?\'"\'+e.replace(/[\\\\"\\x00-\\x1f\\x7f-\\x9f]/g,t=>{switch(t){case"\\\\":return"\\\\\\\\";case\'"\':return\'\\\\"\';case"\\b":return"\\\\b";case"\\f":return"\\\\f";case`\n`:return"\\\\n";case"\\r":return"\\\\r";case"	":return"\\\\t";default:return"\\\\x"+t.charCodeAt(0).toString(16).padStart(2,"0")}})+\'"\':e}function _r(e){return!!(e.length===0||/^\\s|\\s$/.test(e)||/[\\x00-\\x08\\x0b\\x0c\\x0e-\\x1f\\x7f-\\x9f]/.test(e)||/^-/.test(e)||/[\\n:](\\s|$)/.test(e)||/\\s#/.test(e)||/[\\n\\r]/.test(e)||/^[&*\\],?!>|@"\'#%]/.test(e)||/[{}`]/.test(e)||/^\\[/.test(e)||!isNaN(Number(e))||["y","n","yes","no","true","false","on","off","null"].includes(e.toLowerCase()))}var Tr={};function xi(e){Tr=e}function St(e,t){for(;t;){if(e.contains(t))return!0;t=kr(t)}return!1}function $(e){if(e.parentElement)return e.parentElement;if(e.parentNode&&e.parentNode.nodeType===11&&e.parentNode.host)return e.parentNode.host}function Ar(e){let t=e;for(;t.parentNode;)t=t.parentNode;if(t.nodeType===11||t.nodeType===9)return t}function kr(e){for(;e.parentElement;)e=e.parentElement;return $(e)}function ye(e,t,r){for(;e;){const n=e.closest(t);if(r&&n!==r&&n?.contains(r))return;if(n)return n;e=kr(e)}}function X(e,t){const r=t==="::before"?Se:t==="::after"?_e:Ee;if(r&&r.has(e))return r.get(e);const n=e.ownerDocument&&e.ownerDocument.defaultView?e.ownerDocument.defaultView.getComputedStyle(e,t):void 0;return r?.set(e,n),n}function Cr(e,t){if(t=t??X(e),!t)return!0;if(Element.prototype.checkVisibility&&Tr.browserNameForWorkarounds!=="webkit"){if(!e.checkVisibility())return!1}else{const r=e.closest("details,summary");if(r!==e&&r?.nodeName==="DETAILS"&&!r.open)return!1}return t.visibility==="visible"}function Ze(e){const t=X(e);if(!t)return{visible:!0,inline:!1};const r=t.cursor;if(t.display==="contents"){for(let i=e.firstChild;i;i=i.nextSibling){if(i.nodeType===1&&Q(i))return{visible:!0,inline:!1,cursor:r};if(i.nodeType===3&&Nr(i))return{visible:!0,inline:!0,cursor:r}}return{visible:!1,inline:!1,cursor:r}}if(!Cr(e,t))return{cursor:r,visible:!1,inline:!1};const n=e.getBoundingClientRect();return{cursor:r,visible:n.width>0&&n.height>0,inline:t.display==="inline"}}function Q(e){return Ze(e).visible}function Nr(e){const t=e.ownerDocument.createRange();t.selectNode(e);const r=t.getBoundingClientRect();return r.width>0&&r.height>0}function _(e){const t=e.tagName;return typeof t=="string"?t.toUpperCase():e instanceof HTMLFormElement?"FORM":e.tagName.toUpperCase()}var Ee,Se,_e,Ir=0;function _t(){++Ir,Ee??(Ee=new Map),Se??(Se=new Map),_e??(_e=new Map)}function Tt(){--Ir||(Ee=void 0,Se=void 0,_e=void 0)}function $r(e){return e.hasAttribute("aria-label")||e.hasAttribute("aria-labelledby")}var Lr="article:not([role]), aside:not([role]), main:not([role]), nav:not([role]), section:not([role]), [role=article], [role=complementary], [role=main], [role=navigation], [role=region]",yi=[["aria-atomic",void 0],["aria-busy",void 0],["aria-controls",void 0],["aria-current",void 0],["aria-describedby",void 0],["aria-details",void 0],["aria-dropeffect",void 0],["aria-flowto",void 0],["aria-grabbed",void 0],["aria-hidden",void 0],["aria-keyshortcuts",void 0],["aria-label",["caption","code","deletion","emphasis","generic","insertion","paragraph","presentation","strong","subscript","superscript"]],["aria-labelledby",["caption","code","deletion","emphasis","generic","insertion","paragraph","presentation","strong","subscript","superscript"]],["aria-live",void 0],["aria-owns",void 0],["aria-relevant",void 0],["aria-roledescription",["generic"]]];function Rr(e,t){return yi.some(([r,n])=>!n?.includes(t||"")&&e.hasAttribute(r))}function Or(e){return!Number.isNaN(Number(String(e.getAttribute("tabindex"))))}function Ei(e){return!Kr(e)&&(Si(e)||Or(e))}function Si(e){const t=_(e);return["BUTTON","DETAILS","SELECT","TEXTAREA"].includes(t)?!0:t==="A"||t==="AREA"?e.hasAttribute("href"):t==="INPUT"?!e.hidden:!1}var Pr={A:e=>e.hasAttribute("href")?"link":null,AREA:e=>e.hasAttribute("href")?"link":null,ARTICLE:()=>"article",ASIDE:()=>"complementary",BLOCKQUOTE:()=>"blockquote",BUTTON:()=>"button",CAPTION:()=>"caption",CODE:()=>"code",DATALIST:()=>"listbox",DD:()=>"definition",DEL:()=>"deletion",DETAILS:()=>"group",DFN:()=>"term",DIALOG:()=>"dialog",DT:()=>"term",EM:()=>"emphasis",FIELDSET:()=>"group",FIGURE:()=>"figure",FOOTER:e=>ye(e,Lr)?null:"contentinfo",FORM:e=>$r(e)?"form":null,H1:()=>"heading",H2:()=>"heading",H3:()=>"heading",H4:()=>"heading",H5:()=>"heading",H6:()=>"heading",HEADER:e=>ye(e,Lr)?null:"banner",HR:()=>"separator",HTML:()=>"document",IMG:e=>e.getAttribute("alt")===""&&!e.getAttribute("title")&&!Rr(e)&&!Or(e)?"presentation":"img",INPUT:e=>{const t=e.type.toLowerCase();if(t==="search")return e.hasAttribute("list")?"combobox":"searchbox";if(["email","tel","text","url",""].includes(t)){const r=le(e,e.getAttribute("list"))[0];return r&&_(r)==="DATALIST"?"combobox":"textbox"}return t==="hidden"?null:t==="file"?"button":Bi[t]||"textbox"},INS:()=>"insertion",LI:()=>"listitem",MAIN:()=>"main",MARK:()=>"mark",MATH:()=>"math",MENU:()=>"list",METER:()=>"meter",NAV:()=>"navigation",OL:()=>"list",OPTGROUP:()=>"group",OPTION:()=>"option",OUTPUT:()=>"status",P:()=>"paragraph",PROGRESS:()=>"progressbar",SEARCH:()=>"search",SECTION:e=>$r(e)?"region":null,SELECT:e=>e.hasAttribute("multiple")||e.size>1?"listbox":"combobox",STRONG:()=>"strong",SUB:()=>"subscript",SUP:()=>"superscript",SVG:()=>"img",TABLE:()=>"table",TBODY:()=>"rowgroup",TD:e=>{const t=ye(e,"table"),r=t?At(t):"";return r==="grid"||r==="treegrid"?"gridcell":"cell"},TEXTAREA:()=>"textbox",TFOOT:()=>"rowgroup",TH:e=>{const t=e.getAttribute("scope");if(t==="col"||t==="colgroup")return"columnheader";if(t==="row"||t==="rowgroup")return"rowheader";const r=e.nextElementSibling,n=e.previousElementSibling,i=e.parentElement&&_(e.parentElement)==="TR"?e.parentElement:void 0;if(!r&&!n){if(i){const s=ye(i,"table");if(s&&s.rows.length<=1)return null}return"columnheader"}return Mr(r)&&Mr(n)?"columnheader":qr(r)||qr(n)?"rowheader":"columnheader"},THEAD:()=>"rowgroup",TIME:()=>"time",TR:()=>"row",UL:()=>"list"};function Mr(e){return!!e&&_(e)==="TH"}function qr(e){var t;return!e||_(e)!=="TD"?!1:!!((t=e.textContent)!=null&&t.trim()||e.children.length>0)}var _i={DD:["DL","DIV"],DIV:["DL"],DT:["DL","DIV"],LI:["OL","UL"],TBODY:["TABLE"],TD:["TR"],TFOOT:["TABLE"],TH:["TR"],THEAD:["TABLE"],TR:["THEAD","TBODY","TFOOT","TABLE"]};function Dr(e){var t;const r=((t=Pr[_(e)])==null?void 0:t.call(Pr,e))||"";if(!r)return null;let n=e;for(;n;){const i=$(n),s=_i[_(n)];if(!s||!i||!s.includes(_(i)))break;const o=At(i);if((o==="none"||o==="presentation")&&!Br(i,o))return o;n=i}return r}var Ti=["alert","alertdialog","application","article","banner","blockquote","button","caption","cell","checkbox","code","columnheader","combobox","complementary","contentinfo","definition","deletion","dialog","directory","document","emphasis","feed","figure","form","generic","grid","gridcell","group","heading","img","insertion","link","list","listbox","listitem","log","main","mark","marquee","math","meter","menu","menubar","menuitem","menuitemcheckbox","menuitemradio","navigation","none","note","option","paragraph","presentation","progressbar","radio","radiogroup","region","row","rowgroup","rowheader","scrollbar","search","searchbox","separator","slider","spinbutton","status","strong","subscript","superscript","switch","tab","table","tablist","tabpanel","term","textbox","time","timer","toolbar","tooltip","tree","treegrid","treeitem"];function At(e){return(e.getAttribute("role")||"").split(" ").map(r=>r.trim()).find(r=>Ti.includes(r))||null}function Br(e,t){return Rr(e,t)||Ei(e)}function I(e){const t=At(e);if(!t)return Dr(e);if(t==="none"||t==="presentation"){const r=Dr(e);if(Br(e,r))return r}return t}function Hr(e){return e===null?void 0:e.toLowerCase()==="true"}function Fr(e){return["STYLE","SCRIPT","NOSCRIPT","TEMPLATE"].includes(_(e))}function U(e){if(Fr(e))return!0;const t=X(e),r=e.nodeName==="SLOT";if(t?.display==="contents"&&!r){for(let i=e.firstChild;i;i=i.nextSibling)if(i.nodeType===1&&!U(i)||i.nodeType===3&&Nr(i))return!1;return!0}return!(e.nodeName==="OPTION"&&!!e.closest("select"))&&!r&&!Cr(e,t)?!0:Ur(e)}function Ur(e){let t=Y?.get(e);if(t===void 0){if(t=!1,e.parentElement&&e.parentElement.shadowRoot&&!e.assignedSlot&&(t=!0),!t){const r=X(e);t=!r||r.display==="none"||Hr(e.getAttribute("aria-hidden"))===!0}if(!t){const r=$(e);r&&(t=Ur(r))}Y?.set(e,t)}return t}function le(e,t){if(!t)return[];const r=Ar(e);if(!r)return[];try{const n=t.split(" ").filter(s=>!!s),i=[];for(const s of n){const o=r.querySelector("#"+CSS.escape(s));o&&!i.includes(o)&&i.push(o)}return i}catch{return[]}}function z(e){return e.trim()}function Te(e){return e.split("\\xA0").map(t=>t.replace(/\\r\\n/g,`\n`).replace(/[\\u200b\\u00ad]/g,"").replace(/\\s\\s*/g," ")).join("\\xA0").trim()}function jr(e,t){const r=[...e.querySelectorAll(t)];for(const n of le(e,e.getAttribute("aria-owns")))n.matches(t)&&r.push(n),r.push(...n.querySelectorAll(t));return r}function Ae(e,t){const r=t==="::before"?Oe:t==="::after"?Pe:Re;if(r?.has(e))return r?.get(e);const n=X(e,t);let i;if(n){const s=n.content;s&&s!=="none"&&s!=="normal"&&n.display!=="none"&&n.visibility!=="hidden"&&(i=Ai(e,s,!!t))}return t&&i!==void 0&&(n?.display||"inline")!=="inline"&&(i=" "+i+" "),r&&r.set(e,i),i}function Ai(e,t,r){if(!(!t||t==="none"||t==="normal"))try{let n=rr(t).filter(a=>!(a instanceof Ge));const i=n.findIndex(a=>a instanceof C&&a.value==="/");if(i!==-1)n=n.slice(i+1);else if(!r)return;const s=[];let o=0;for(;o<n.length;)if(n[o]instanceof wt)s.push(n[o].value),o++;else if(o+2<n.length&&n[o]instanceof me&&n[o].value==="attr"&&n[o+1]instanceof vt&&n[o+2]instanceof bt){const a=n[o+1].value;s.push(e.getAttribute(a)||""),o+=3}else return;return s.join("")}catch{}}function Wr(e){const t=e.getAttribute("aria-labelledby");if(t===null)return null;const r=le(e,t);return r.length?r:null}function ki(e,t){const r=["button","cell","checkbox","columnheader","gridcell","heading","link","menuitem","menuitemcheckbox","menuitemradio","option","radio","row","rowheader","switch","tab","tooltip","treeitem"].includes(e),n=t&&["","caption","code","contentinfo","definition","deletion","emphasis","insertion","list","listitem","mark","none","paragraph","presentation","region","row","rowgroup","section","strong","subscript","superscript","table","term","time"].includes(e);return r||n}function ke(e,t){const r=t?Ie:Ne;let n=r?.get(e);return n===void 0&&(n="",["caption","code","definition","deletion","emphasis","generic","insertion","mark","paragraph","presentation","strong","subscript","suggestion","superscript","term","time"].includes(I(e)||"")||(n=Te(j(e,{includeHidden:t,visitedElements:new Set,embeddedInTargetElement:"self"}))),r?.set(e,n)),n}function Vr(e,t){const r=t?Le:$e;let n=r?.get(e);if(n===void 0){if(n="",e.hasAttribute("aria-describedby")){const i=le(e,e.getAttribute("aria-describedby"));n=Te(i.map(s=>j(s,{includeHidden:t,visitedElements:new Set,embeddedInDescribedBy:{element:s,hidden:U(s)}})).join(" "))}else e.hasAttribute("aria-description")?n=Te(e.getAttribute("aria-description")||""):n=Te(e.getAttribute("title")||"");r?.set(e,n)}return n}function Ci(e){const t=e.getAttribute("aria-invalid");return!t||t.trim()===""||t.toLocaleLowerCase()==="false"?"false":t==="true"||t==="grammar"||t==="spelling"?t:"true"}function Ni(e){if("validity"in e){const t=e.validity;return t?.valid===!1}return!1}function Ii(e){const t=Z;let r=Z?.get(e);if(r===void 0){r="";const n=Ci(e)!=="false",i=Ni(e);if(n||i){const s=e.getAttribute("aria-errormessage");r=le(e,s).map(l=>Te(j(l,{visitedElements:new Set,embeddedInDescribedBy:{element:l,hidden:U(l)}}))).join(" ").trim()}t?.set(e,r)}return r}function j(e,t){var r,n,i,s;if(t.visitedElements.has(e))return"";const o={...t,embeddedInTargetElement:t.embeddedInTargetElement==="self"?"descendant":t.embeddedInTargetElement};if(!t.includeHidden){const u=!!((r=t.embeddedInLabelledBy)!=null&&r.hidden)||!!((n=t.embeddedInDescribedBy)!=null&&n.hidden)||!!((i=t.embeddedInNativeTextAlternative)!=null&&i.hidden)||!!((s=t.embeddedInLabel)!=null&&s.hidden);if(Fr(e)||!u&&U(e))return t.visitedElements.add(e),""}const a=Wr(e);if(!t.embeddedInLabelledBy){const u=(a||[]).map(p=>j(p,{...t,embeddedInLabelledBy:{element:p,hidden:U(p)},embeddedInDescribedBy:void 0,embeddedInTargetElement:void 0,embeddedInLabel:void 0,embeddedInNativeTextAlternative:void 0})).join(" ");if(u)return u}const l=I(e)||"",c=_(e);if(t.embeddedInLabel||t.embeddedInLabelledBy||t.embeddedInTargetElement==="descendant"){const u=[...e.labels||[]].includes(e),p=(a||[]).includes(e);if(!u&&!p){if(l==="textbox")return t.visitedElements.add(e),c==="INPUT"||c==="TEXTAREA"?e.value:e.textContent||"";if(["combobox","listbox"].includes(l)){t.visitedElements.add(e);let d;if(c==="SELECT")d=[...e.selectedOptions],!d.length&&e.options.length&&d.push(e.options[0]);else{const g=l==="combobox"?jr(e,"*").find(m=>I(m)==="listbox"):e;d=g?jr(g,\'[aria-selected="true"]\').filter(m=>I(m)==="option"):[]}return!d.length&&c==="INPUT"?e.value:d.map(g=>j(g,o)).join(" ")}if(["progressbar","scrollbar","slider","spinbutton","meter"].includes(l))return t.visitedElements.add(e),e.hasAttribute("aria-valuetext")?e.getAttribute("aria-valuetext")||"":e.hasAttribute("aria-valuenow")?e.getAttribute("aria-valuenow")||"":e.getAttribute("value")||"";if(["menu"].includes(l))return t.visitedElements.add(e),""}}const h=e.getAttribute("aria-label")||"";if(z(h))return t.visitedElements.add(e),h;if(!["presentation","none"].includes(l)){if(c==="INPUT"&&["button","submit","reset"].includes(e.type)){t.visitedElements.add(e);const u=e.value||"";return z(u)?u:e.type==="submit"?"Submit":e.type==="reset"?"Reset":e.getAttribute("title")||""}if(c==="INPUT"&&e.type==="file"){t.visitedElements.add(e);const u=e.labels||[];return u.length&&!t.embeddedInLabelledBy?Ce(u,t):"Choose File"}if(c==="INPUT"&&e.type==="image"){t.visitedElements.add(e);const u=e.labels||[];if(u.length&&!t.embeddedInLabelledBy)return Ce(u,t);const p=e.getAttribute("alt")||"";if(z(p))return p;const d=e.getAttribute("title")||"";return z(d)?d:"Submit"}if(!a&&c==="BUTTON"){t.visitedElements.add(e);const u=e.labels||[];if(u.length)return Ce(u,t)}if(!a&&c==="OUTPUT"){t.visitedElements.add(e);const u=e.labels||[];return u.length?Ce(u,t):e.getAttribute("title")||""}if(!a&&(c==="TEXTAREA"||c==="SELECT"||c==="INPUT")){t.visitedElements.add(e);const u=e.labels||[];if(u.length)return Ce(u,t);const p=c==="INPUT"&&["text","password","search","tel","email","url"].includes(e.type)||c==="TEXTAREA",d=e.getAttribute("placeholder")||"",g=e.getAttribute("title")||"";return!p||g?g:d}if(!a&&c==="FIELDSET"){t.visitedElements.add(e);for(let p=e.firstElementChild;p;p=p.nextElementSibling)if(_(p)==="LEGEND")return j(p,{...o,embeddedInNativeTextAlternative:{element:p,hidden:U(p)}});return e.getAttribute("title")||""}if(!a&&c==="FIGURE"){t.visitedElements.add(e);for(let p=e.firstElementChild;p;p=p.nextElementSibling)if(_(p)==="FIGCAPTION")return j(p,{...o,embeddedInNativeTextAlternative:{element:p,hidden:U(p)}});return e.getAttribute("title")||""}if(c==="IMG"){t.visitedElements.add(e);const u=e.getAttribute("alt")||"";return z(u)?u:e.getAttribute("title")||""}if(c==="TABLE"){t.visitedElements.add(e);for(let p=e.firstElementChild;p;p=p.nextElementSibling)if(_(p)==="CAPTION")return j(p,{...o,embeddedInNativeTextAlternative:{element:p,hidden:U(p)}});const u=e.getAttribute("summary")||"";if(u)return u}if(c==="AREA"){t.visitedElements.add(e);const u=e.getAttribute("alt")||"";return z(u)?u:e.getAttribute("title")||""}if(c==="SVG"||e.ownerSVGElement){t.visitedElements.add(e);for(let u=e.firstElementChild;u;u=u.nextElementSibling)if(_(u)==="TITLE"&&u.ownerSVGElement)return j(u,{...o,embeddedInLabelledBy:{element:u,hidden:U(u)}})}if(e.ownerSVGElement&&c==="A"){const u=e.getAttribute("xlink:title")||"";if(z(u))return t.visitedElements.add(e),u}}const f=c==="SUMMARY"&&!["presentation","none"].includes(l);if(ki(l,t.embeddedInTargetElement==="descendant")||f||t.embeddedInLabelledBy||t.embeddedInDescribedBy||t.embeddedInLabel||t.embeddedInNativeTextAlternative){t.visitedElements.add(e);const u=$i(e,o);if(t.embeddedInTargetElement==="self"?z(u):u)return u}if(!["presentation","none"].includes(l)||c==="IFRAME"){t.visitedElements.add(e);const u=e.getAttribute("title")||"";if(z(u))return u}return t.visitedElements.add(e),""}function $i(e,t){const r=[],n=(s,o)=>{var a;if(!(o&&s.assignedSlot))if(s.nodeType===1){const l=((a=X(s))==null?void 0:a.display)||"inline";let c=j(s,t);(l!=="inline"||s.nodeName==="BR")&&(c=" "+c+" "),r.push(c)}else s.nodeType===3&&r.push(s.textContent||"")};r.push(Ae(e,"::before")||"");const i=Ae(e);if(i!==void 0)r.push(i);else{const s=e.nodeName==="SLOT"?e.assignedNodes():[];if(s.length)for(const o of s)n(o,!1);else{for(let o=e.firstChild;o;o=o.nextSibling)n(o,!0);if(e.shadowRoot)for(let o=e.shadowRoot.firstChild;o;o=o.nextSibling)n(o,!0);for(const o of le(e,e.getAttribute("aria-owns")))n(o,!0)}}return r.push(Ae(e,"::after")||""),r.join("")}var kt=["gridcell","option","row","tab","rowheader","columnheader","treeitem"];function zr(e){return _(e)==="OPTION"?e.selected:kt.includes(I(e)||"")?Hr(e.getAttribute("aria-selected"))===!0:!1}var Ct=["checkbox","menuitemcheckbox","option","radio","switch","menuitemradio","treeitem"];function Gr(e){const t=Nt(e,!0);return t==="error"?!1:t}function Li(e){return Nt(e,!0)}function Ri(e){return Nt(e,!1)}function Nt(e,t){const r=_(e);if(t&&r==="INPUT"&&e.indeterminate)return"mixed";if(r==="INPUT"&&["checkbox","radio"].includes(e.type))return e.checked;if(Ct.includes(I(e)||"")){const n=e.getAttribute("aria-checked");return n==="true"?!0:t&&n==="mixed"?"mixed":!1}return"error"}var Oi=["checkbox","combobox","grid","gridcell","listbox","radiogroup","slider","spinbutton","textbox","columnheader","rowheader","searchbox","switch","treegrid"];function Pi(e){const t=_(e);return["INPUT","TEXTAREA","SELECT"].includes(t)?e.hasAttribute("readonly"):Oi.includes(I(e)||"")?e.getAttribute("aria-readonly")==="true":e.isContentEditable?!1:"error"}var It=["button"];function Jr(e){if(It.includes(I(e)||"")){const t=e.getAttribute("aria-pressed");if(t==="true")return!0;if(t==="mixed")return"mixed"}return!1}var $t=["application","button","checkbox","combobox","gridcell","link","listbox","menuitem","row","rowheader","tab","treeitem","columnheader","menuitemcheckbox","menuitemradio","rowheader","switch"];function Xr(e){if(_(e)==="DETAILS")return e.open;if($t.includes(I(e)||"")){const t=e.getAttribute("aria-expanded");return t===null?void 0:t==="true"}}var Lt=["heading","listitem","row","treeitem"];function Qr(e){const t={H1:1,H2:2,H3:3,H4:4,H5:5,H6:6}[_(e)];if(t)return t;if(Lt.includes(I(e)||"")){const r=e.getAttribute("aria-level"),n=r===null?Number.NaN:Number(r);if(Number.isInteger(n)&&n>=1)return n}return 0}var Yr=["application","button","composite","gridcell","group","input","link","menuitem","scrollbar","separator","tab","checkbox","columnheader","combobox","grid","listbox","menu","menubar","menuitemcheckbox","menuitemradio","option","radio","radiogroup","row","rowheader","searchbox","select","slider","spinbutton","switch","tablist","textbox","toolbar","tree","treegrid","treeitem"];function et(e){return Kr(e)||Zr(e)}function Kr(e){return["BUTTON","INPUT","SELECT","TEXTAREA","OPTION","OPTGROUP"].includes(_(e))&&(e.hasAttribute("disabled")||Mi(e)||qi(e))}function Mi(e){return _(e)==="OPTION"&&!!e.closest("OPTGROUP[DISABLED]")}function qi(e){const t=e?.closest("FIELDSET[DISABLED]");if(!t)return!1;const r=t.querySelector(":scope > LEGEND");return!r||!r.contains(e)}function Zr(e,t=!1){if(!e)return!1;if(t||Yr.includes(I(e)||"")){const r=(e.getAttribute("aria-disabled")||"").toLowerCase();return r==="true"?!0:r==="false"?!1:Zr($(e),!0)}return!1}function Ce(e,t){return[...e].map(r=>j(r,{...t,embeddedInLabel:{element:r,hidden:U(r)},embeddedInNativeTextAlternative:void 0,embeddedInLabelledBy:void 0,embeddedInDescribedBy:void 0,embeddedInTargetElement:void 0})).filter(r=>!!r).join(" ")}function Di(e){const t=Me;let r=e,n;const i=[];for(;r;r=$(r)){const s=t.get(r);if(s!==void 0){n=s;break}i.push(r);const o=X(r);if(!o){n=!0;break}const a=o.pointerEvents;if(a){n=a!=="none";break}}n===void 0&&(n=!0);for(const s of i)t.set(s,n);return n}var Ne,Ie,$e,Le,Z,Y,Re,Oe,Pe,Me,en=0;function tt(){_t(),++en,Ne??(Ne=new Map),Ie??(Ie=new Map),$e??($e=new Map),Le??(Le=new Map),Z??(Z=new Map),Y??(Y=new Map),Re??(Re=new Map),Oe??(Oe=new Map),Pe??(Pe=new Map),Me??(Me=new Map)}function rt(){--en||(Ne=void 0,Ie=void 0,$e=void 0,Le=void 0,Z=void 0,Y=void 0,Re=void 0,Oe=void 0,Pe=void 0,Me=void 0),Tt()}var Bi={button:"button",checkbox:"checkbox",image:"button",number:"spinbutton",radio:"radio",range:"slider",reset:"button",submit:"button"},Hi=0;function tn(e){return e.mode==="ai"?{visibility:"ariaOrVisible",refs:"interactable",refPrefix:e.refPrefix,includeGenericRole:!0,renderActive:!e.doNotRenderActive,renderCursorPointer:!0}:e.mode==="autoexpect"?{visibility:"ariaAndVisible",refs:"none"}:e.mode==="codegen"?{visibility:"aria",refs:"none",renderStringsAsRegex:!0}:{visibility:"aria",refs:"none"}}function qe(e,t){const r=tn(t),n=new Set,i={root:{role:"fragment",name:"",children:[],props:{},box:Ze(e),receivesPointerEvents:!0},elements:new Map,refs:new Map,iframeRefs:[]};Mt(i.root,e);const s=(a,l,c)=>{if(n.has(l))return;if(n.add(l),l.nodeType===Node.TEXT_NODE&&l.nodeValue){if(!c)return;const g=l.nodeValue;a.role!=="textbox"&&g&&a.children.push(l.nodeValue||"");return}if(l.nodeType!==Node.ELEMENT_NODE)return;const h=l,f=!U(h);let u=f;if(r.visibility==="ariaOrVisible"&&(u=f||Q(h)),r.visibility==="ariaAndVisible"&&(u=f&&Q(h)),r.visibility==="aria"&&!u)return;const p=[];if(h.hasAttribute("aria-owns")){const g=h.getAttribute("aria-owns").split(/\\s+/);for(const m of g){const w=e.ownerDocument.getElementById(m);w&&p.push(w)}}const d=u?Fi(h,r):null;d&&(d.ref&&(i.elements.set(d.ref,h),i.refs.set(h,d.ref),d.role==="iframe"&&i.iframeRefs.push(d.ref)),a.children.push(d)),o(d||a,h,p,u)};function o(a,l,c,h){var f;const p=(((f=X(l))==null?void 0:f.display)||"inline")!=="inline"||l.nodeName==="BR"?" ":"";p&&a.children.push(p),a.children.push(Ae(l,"::before")||"");const d=l.nodeName==="SLOT"?l.assignedNodes():[];if(d.length)for(const g of d)s(a,g,h);else{for(let g=l.firstChild;g;g=g.nextSibling)g.assignedSlot||s(a,g,h);if(l.shadowRoot)for(let g=l.shadowRoot.firstChild;g;g=g.nextSibling)s(a,g,h)}for(const g of c)s(a,g,h);if(a.children.push(Ae(l,"::after")||""),p&&a.children.push(p),a.children.length===1&&a.name===a.children[0]&&(a.children=[]),a.role==="link"&&l.hasAttribute("href")){const g=l.getAttribute("href");a.props.url=g}if(a.role==="textbox"&&l.hasAttribute("placeholder")&&l.getAttribute("placeholder")!==a.name){const g=l.getAttribute("placeholder");a.props.placeholder=g}}tt();try{s(i.root,e,!0)}finally{rt()}return ji(i.root),Ui(i.root),i}function rn(e,t){var r;if(t.refs==="none"||t.refs==="interactable"&&(!e.box.visible||!e.receivesPointerEvents))return;const n=Pt(e);let i=n._ariaRef;(!i||i.role!==e.role||i.name!==e.name)&&(i={role:e.role,name:e.name,ref:((r=t.refPrefix)!=null?r:"")+"e"+ ++Hi},n._ariaRef=i),e.ref=i.ref}function Fi(e,t){var r;const n=e.ownerDocument.activeElement===e;if(e.nodeName==="IFRAME"){const h={role:"iframe",name:"",children:[],props:{},box:Ze(e),receivesPointerEvents:!0,active:n};return Mt(h,e),rn(h,t),h}const i=t.includeGenericRole?"generic":null,s=(r=I(e))!=null?r:i;if(!s||s==="presentation"||s==="none")return null;const o=O(ke(e,!1)||""),a=Di(e),l=Ze(e);if(s==="generic"&&l.inline&&e.childNodes.length===1&&e.childNodes[0].nodeType===Node.TEXT_NODE)return null;const c={role:s,name:o,children:[],props:{},box:l,receivesPointerEvents:a,active:n};return Mt(c,e),rn(c,t),Ct.includes(s)&&(c.checked=Gr(e)),Yr.includes(s)&&(c.disabled=et(e)),$t.includes(s)&&(c.expanded=Xr(e)),Lt.includes(s)&&(c.level=Qr(e)),It.includes(s)&&(c.pressed=Jr(e)),kt.includes(s)&&(c.selected=zr(e)),(e instanceof HTMLInputElement||e instanceof HTMLTextAreaElement)&&e.type!=="checkbox"&&e.type!=="radio"&&e.type!=="file"&&(c.children=[(function(f,u){if(f.tagName.toLowerCase()!=="input")return!1;if(f.getAttribute("type")?.toLowerCase()==="hidden")return!0;let p="";for(const d of u.attributes)p+=` ${f.getAttribute(d)??""}`;return new RegExp(u.pattern,"i").test(p)})(e,{attributes:["type","autocomplete","id","name","placeholder","aria-label","title"],pattern:"user[-_ ]?name|e[-_ ]?mail|one[-_ ]?time[-_ ]?code|password|passcode|passwd|\\\\botp\\\\b|\\\\b(?:2fa|mfa)\\\\b|phone|mobile|\\\\btel\\\\b"})?"":e.value]),c}function Ui(e){const t=r=>{const n=[];for(const s of r.children||[]){if(typeof s=="string"){n.push(s);continue}const o=t(s);n.push(...o)}return r.role==="generic"&&!r.name&&n.length<=1&&n.every(s=>typeof s!="string"&&!!s.ref)?n:(r.children=n,[r])};t(e)}function ji(e){const t=(n,i)=>{if(!n.length)return;const s=O(n.join(""));s&&i.push(s),n.length=0},r=n=>{const i=[],s=[];for(const o of n.children||[])typeof o=="string"?s.push(o):(t(s,i),r(o),i.push(o));t(s,i),n.children=i.length?i:[],n.children.length===1&&n.children[0]===n.name&&(n.children=[])};r(e)}function Wi(e,t){return t?e?typeof t=="string"?e===t:!!e.match(new RegExp(t.pattern)):!1:!0}function nn(e,t){if(!t?.normalized)return!0;if(!e)return!1;if(e===t.normalized||e===t.raw)return!0;const r=Vi(t);return r?!!e.match(r):!1}var Rt=Symbol("cachedRegex");function Vi(e){if(e[Rt]!==void 0)return e[Rt];const{raw:t}=e,r=t.startsWith("/")&&t.endsWith("/")&&t.length>1;let n;try{n=r?new RegExp(t.slice(1,-1)):null}catch{n=null}return e[Rt]=n,n}function zi(e,t){const r=qe(e,{mode:"default"});return{matches:an(r.root,t,!1,!1),received:{raw:De(r,{mode:"default"}).text,regex:De(r,{mode:"codegen"}).text}}}function Gi(e,t){const r=qe(e,{mode:"default"}).root;return an(r,t,!0,!1).map(i=>Pt(i))}function Ot(e,t,r){var n;return typeof e=="string"&&t.kind==="text"?nn(e,t.text):e===null||typeof e!="object"||t.kind!=="role"||t.role!=="fragment"&&t.role!==e.role||t.checked!==void 0&&t.checked!==e.checked||t.disabled!==void 0&&t.disabled!==e.disabled||t.expanded!==void 0&&t.expanded!==e.expanded||t.level!==void 0&&t.level!==e.level||t.pressed!==void 0&&t.pressed!==e.pressed||t.selected!==void 0&&t.selected!==e.selected||!Wi(e.name,t.name)||!nn(e.props.url,(n=t.props)==null?void 0:n.url)?!1:t.containerMode==="contain"?on(e.children||[],t.children||[]):t.containerMode==="equal"?sn(e.children||[],t.children||[],!1):t.containerMode==="deep-equal"||r?sn(e.children||[],t.children||[],!0):on(e.children||[],t.children||[])}function sn(e,t,r){if(t.length!==e.length)return!1;for(let n=0;n<t.length;++n)if(!Ot(e[n],t[n],r))return!1;return!0}function on(e,t){if(t.length>e.length)return!1;const r=e.slice(),n=t.slice();for(const i of n){let s=r.shift();for(;s&&!Ot(s,i,!1);)s=r.shift();if(!s)return!1}return!0}function an(e,t,r,n){const i=[],s=(o,a)=>{if(Ot(o,t,n)){const l=typeof o=="string"?a:o;return l&&i.push(l),!r}if(typeof o=="string")return!1;for(const l of o.children||[])if(s(l,o))return!0;return!1};return s(e,null),i}function ln(e,t=new Map){e?.ref&&t.set(e.ref,e);for(const r of e?.children||[])typeof r!="string"&&ln(r,t);return t}function Ji(e,t){var r;const n=ln(t?.root),i=new Map,s=(o,a)=>{let l=o.children.length===a?.children.length&&Dn(o,a),c=l;for(let h=0;h<o.children.length;h++){const f=o.children[h],u=a?.children[h];if(typeof f=="string")l&&(l=f===u),c&&(c=f===u);else{let p=typeof u!="string"?u:void 0;f.ref&&(p=n.get(f.ref));const d=s(f,p);(!p||!d&&!f.ref||p!==u)&&(c=!1),l&&(l=d&&p===u)}}return i.set(o,l?"same":c?"skip":"changed"),l};return s(e.root,n.get((r=t?.root)==null?void 0:r.ref)),i}function Xi(e,t){const r=[],n=i=>{const s=t.get(i);if(s!=="same")if(s==="skip")for(const o of i.children)typeof o!="string"&&n(o);else r.push(i)};for(const i of e)typeof i=="string"?r.push(i):n(i);return r}function nt(e){return"  ".repeat(e)}function De(e,t,r){const n=tn(t),i=[],s={},o=n.renderStringsAsRegex?Yi:()=>!0,a=n.renderStringsAsRegex?Qi:d=>d;let l=e.root.role==="fragment"?e.root.children:[e.root];const c=Ji(e,r);r&&(l=Xi(l,c));const h=(d,g)=>{if(t.depth&&g>t.depth)return;const m=Et(a(d));m&&i.push(nt(g)+"- text: "+m)},f=(d,g)=>{let m=d.role;if(d.name&&d.name.length<=900){const w=a(d.name);if(w){const x=w.startsWith("/")&&w.endsWith("/")?w:JSON.stringify(w);m+=" "+x}}return d.checked==="mixed"&&(m+=" [checked=mixed]"),d.checked===!0&&(m+=" [checked]"),d.disabled&&(m+=" [disabled]"),d.expanded&&(m+=" [expanded]"),d.active&&n.renderActive&&(m+=" [active]"),d.level&&(m+=` [level=${d.level}]`),d.pressed==="mixed"&&(m+=" [pressed=mixed]"),d.pressed===!0&&(m+=" [pressed]"),d.selected===!0&&(m+=" [selected]"),d.ref&&(m+=` [ref=${d.ref}]`,g&&We(d)&&(m+=" [cursor=pointer]")),m},u=d=>d?.children.length===1&&typeof d.children[0]=="string"&&!Object.keys(d.props).length?d.children[0]:void 0,p=(d,g,m)=>{if(t.depth&&g>t.depth)return;if(d.role==="iframe"&&d.ref&&(s[d.ref]=g),c.get(d)==="same"&&d.ref){i.push(nt(g)+`- ref=${d.ref} [unchanged]`);return}const w=!!r&&!g,x=nt(g)+"- "+(w?"<changed> ":"")+wi(f(d,m)),y=u(d),L=!!t.depth&&g===t.depth;if(!y&&(!d.children.length||L)&&!Object.keys(d.props).length)i.push(x);else if(y!==void 0)o(d,y)?i.push(x+": "+Et(a(y))):i.push(x);else{i.push(x+":");for(const[A,J]of Object.entries(d.props))i.push(nt(g+1)+"- /"+A+": "+Et(J));const K=!!d.ref&&m&&We(d);for(const A of d.children)typeof A=="string"?h(o(d,A)?A:"",g+1):p(A,g+1,m&&!K)}};for(const d of l)typeof d=="string"?h(d,0):p(d,0,!!n.renderCursorPointer);return{text:i.join(`\n`),iframeDepths:s}}function Qi(e){const t=[{regex:/\\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\\b/,replacement:"[0-9a-fA-F-]+"},{regex:/\\b[\\d,.]+[bkmBKM]+\\b/,replacement:"[\\\\d,.]+[bkmBKM]+"},{regex:/\\b\\d+[hmsp]+\\b/,replacement:"\\\\d+[hmsp]+"},{regex:/\\b[\\d,.]+[hmsp]+\\b/,replacement:"[\\\\d,.]+[hmsp]+"},{regex:/\\b\\d+,\\d+\\b/,replacement:"\\\\d+,\\\\d+"},{regex:/\\b\\d+\\.\\d{2,}\\b/,replacement:"\\\\d+\\\\.\\\\d+"},{regex:/\\b\\d{2,}\\.\\d+\\b/,replacement:"\\\\d+\\\\.\\\\d+"},{regex:/\\b\\d{2,}\\b/,replacement:"\\\\d+"}];let r="",n=0;const i=new RegExp(t.map(s=>"("+s.regex.source+")").join("|"),"g");return e.replace(i,(s,...o)=>{const a=o[o.length-2],l=o.slice(0,-2);r+=Ke(e.slice(n,a));for(let c=0;c<l.length;c++)if(l[c]){const{replacement:h}=t[c];r+=h;break}return n=a+s.length,s}),r?(r+=Ke(e.slice(n)),String(new RegExp(r))):e}function Yi(e,t){if(!t.length)return!1;if(!e.name)return!0;if(e.name.length>t.length)return!1;const r=t.length<=200&&e.name.length<=200?ui(t,e.name):"";let n=t;for(;r&&n.includes(r);)n=n.replace(r,"");return n.trim().length/t.length>.1}var cn=Symbol("element");function Pt(e){return e[cn]}function Mt(e,t){e[cn]=t}function Ki(e,t){const r=Un(e,t);return r?Pt(r):void 0}var un=`:host{font-size:13px;font-family:system-ui,Ubuntu,Droid Sans,sans-serif;color:#333}svg{position:absolute;height:0}x-pw-tooltip{backdrop-filter:blur(5px);background-color:#fff;border-radius:6px;box-shadow:0 .5rem 1.2rem #0000004d;display:none;font-size:12.8px;font-weight:400;left:0;line-height:1.5;max-width:600px;position:absolute;top:0;padding:0;flex-direction:column;overflow:hidden}x-pw-tooltip-line{display:flex;max-width:600px;padding:6px;user-select:none;cursor:pointer}x-pw-tooltip-footer{display:flex;max-width:600px;padding:6px;user-select:none;color:#777}x-pw-dialog{background-color:#fff;pointer-events:auto;border-radius:6px;box-shadow:0 .5rem 1.2rem #0000004d;display:flex;flex-direction:column;position:absolute;z-index:10;font-size:13px}x-pw-dialog:not(.autosize){width:400px;height:150px}x-pw-dialog-body{display:flex;flex-direction:column;flex:auto}x-pw-dialog-body label{margin:5px 8px;display:flex;flex-direction:row;align-items:center}x-pw-highlight{position:absolute;top:0;left:0;width:0;height:0}x-pw-action-point{position:absolute;width:20px;height:20px;background:red;border-radius:10px;margin:-10px 0 0 -10px;z-index:2}x-pw-title{position:absolute;backdrop-filter:blur(5px);background-color:#00000080;color:#fff;border-radius:6px;padding:6px;font-size:24px;line-height:1.4;white-space:nowrap;user-select:none;z-index:3}x-pw-user-overlays,x-pw-user-overlay{position:absolute;inset:0}@keyframes pw-fade-out{0%{opacity:1}to{opacity:0}}x-pw-separator{height:1px;margin:6px 9px;background:#949494e5}x-pw-tool-gripper{height:28px;width:24px;margin:2px 0;cursor:grab}x-pw-tool-gripper:active{cursor:grabbing}x-pw-tool-gripper>x-div{width:16px;height:16px;margin:6px 4px;clip-path:url(#icon-gripper);background-color:#555}x-pw-tools-list>label{display:flex;align-items:center;margin:0 10px;user-select:none}x-pw-tools-list{display:flex;width:100%;border-bottom:1px solid #dddddd}x-pw-tool-item{pointer-events:auto;height:28px;width:28px;border-radius:3px}x-pw-tool-item:not(.disabled){cursor:pointer}x-pw-tool-item:not(.disabled):hover{background-color:#dbdbdb}x-pw-tool-item.toggled{background-color:#8acae480}x-pw-tool-item.toggled:not(.disabled):hover{background-color:#8acae4c4}x-pw-tool-item>x-div{width:16px;height:16px;margin:6px;background-color:#3a3a3a}x-pw-tool-item.disabled>x-div{background-color:#61616180;cursor:default}x-pw-tool-item.record.toggled{background-color:transparent}x-pw-tool-item.record.toggled:not(.disabled):hover{background-color:#dbdbdb}x-pw-tool-item.record.toggled>x-div{background-color:#a1260d}x-pw-tool-item.record.disabled.toggled>x-div{opacity:.8}x-pw-tool-item.accept>x-div{background-color:#388a34}x-pw-tool-item.record>x-div{clip-path:url(#icon-circle-large-filled)}x-pw-tool-item.record.toggled>x-div{clip-path:url(#icon-stop-circle)}x-pw-tool-item.pick-locator>x-div{clip-path:url(#icon-inspect)}x-pw-tool-item.text>x-div{clip-path:url(#icon-whole-word)}x-pw-tool-item.visibility>x-div{clip-path:url(#icon-eye)}x-pw-tool-item.value>x-div{clip-path:url(#icon-symbol-constant)}x-pw-tool-item.snapshot>x-div{clip-path:url(#icon-gist)}x-pw-tool-item.accept>x-div{clip-path:url(#icon-check)}x-pw-tool-item.cancel>x-div{clip-path:url(#icon-close)}x-pw-tool-item.succeeded>x-div{clip-path:url(#icon-pass);background-color:#388a34!important}x-pw-overlay{position:absolute;top:0;max-width:min-content;z-index:2147483647;background:transparent;pointer-events:auto}x-pw-overlay x-pw-tools-list{background-color:#fffd;box-shadow:#0000001a 0 5px 5px;border-radius:3px;border-bottom:none}x-pw-overlay x-pw-tool-item{margin:2px}textarea.text-editor{font-family:system-ui,Ubuntu,Droid Sans,sans-serif;flex:auto;border:none;margin:6px 10px;color:#333;outline:1px solid transparent!important;resize:none;padding:0;font-size:13px}textarea.text-editor.does-not-match{outline:1px solid red!important}x-div{display:block}x-spacer{flex:auto}*{box-sizing:border-box}*[hidden]{display:none!important}x-locator-editor{flex:none;width:100%;height:60px;padding:4px;border-bottom:1px solid #dddddd;outline:1px solid transparent}x-locator-editor.does-not-match{outline:1px solid red}.CodeMirror{width:100%!important;height:100%!important}x-pw-action-list{flex:auto;display:flex;flex-direction:column;user-select:none}x-pw-action-item{padding:6px 10px;cursor:pointer;overflow:hidden}x-pw-action-item:hover{background-color:#f2f2f2}x-pw-action-item:last-child{border-bottom-left-radius:6px;border-bottom-right-radius:6px}\n`,it=class{constructor(e){this._renderedEntries=[],this._userOverlays=new Map,this._userOverlayHidden=!1,this._language="javascript",this._injectedScript=e;const t=e.document;if(this._isUnderTest=e.isUnderTest,this._glassPaneElement=t.createElement("x-pw-glass"),this._glassPaneElement.setAttribute("popover","manual"),this._glassPaneElement.style.inset="0",this._glassPaneElement.style.width="100%",this._glassPaneElement.style.height="100%",this._glassPaneElement.style.maxWidth="none",this._glassPaneElement.style.maxHeight="none",this._glassPaneElement.style.padding="0",this._glassPaneElement.style.margin="0",this._glassPaneElement.style.border="none",this._glassPaneElement.style.overflow="visible",this._glassPaneElement.style.pointerEvents="none",this._glassPaneElement.style.display="flex",this._glassPaneElement.style.backgroundColor="transparent",this._actionPointElement=t.createElement("x-pw-action-point"),this._actionPointElement.setAttribute("hidden","true"),this._titleElement=t.createElement("x-pw-title"),this._titleElement.setAttribute("hidden","true"),this._userOverlayContainer=t.createElement("x-pw-user-overlays"),this._userOverlayContainer.setAttribute("hidden","true"),this._glassPaneShadow=this._glassPaneElement.attachShadow({mode:this._isUnderTest?"open":"closed"}),typeof this._glassPaneShadow.adoptedStyleSheets.push=="function"){const r=new this._injectedScript.window.CSSStyleSheet;r.replaceSync(un),this._glassPaneShadow.adoptedStyleSheets.push(r)}else{const r=this._injectedScript.document.createElement("style");r.textContent=un,this._glassPaneShadow.appendChild(r)}this._glassPaneShadow.appendChild(this._actionPointElement),this._glassPaneShadow.appendChild(this._titleElement),this._glassPaneShadow.appendChild(this._userOverlayContainer)}install(){this._injectedScript.document.documentElement&&((!this._injectedScript.document.documentElement.contains(this._glassPaneElement)||this._glassPaneElement.nextElementSibling)&&this._injectedScript.document.documentElement.appendChild(this._glassPaneElement),this._bringToFront())}_bringToFront(){this._glassPaneElement.hidePopover(),this._glassPaneElement.showPopover()}setLanguage(e){this._language=e}runHighlightOnRaf(e){this._rafRequest&&this._injectedScript.utils.builtins.cancelAnimationFrame(this._rafRequest);const t=this._injectedScript.querySelectorAll(e,this._injectedScript.document.documentElement),r=oe(this._language,W(e)),n=t.length>1?"#f6b26b7f":"#6fa8dc7f";this.updateHighlight(t.map((i,s)=>{const o=t.length>1?` [${s+1} of ${t.length}]`:"";return{element:i,color:n,tooltipText:r+o}})),this._rafRequest=this._injectedScript.utils.builtins.requestAnimationFrame(()=>this.runHighlightOnRaf(e))}uninstall(){this._rafRequest&&this._injectedScript.utils.builtins.cancelAnimationFrame(this._rafRequest),this._glassPaneElement.remove()}showActionPoint(e,t,r){this._actionPointElement.style.top=t+"px",this._actionPointElement.style.left=e+"px",this._actionPointElement.hidden=!1,r?this._actionPointElement.style.animation=`pw-fade-out ${r}ms ease-out forwards`:this._actionPointElement.style.animation=""}hideActionPoint(){this._actionPointElement.hidden=!0}showActionTitle(e,t,r,n){if(this._titleElement.textContent=e,this._titleElement.hidden=!1,t){const i=t/4;this._titleElement.style.animation=`pw-fade-out ${i}ms ease-out ${t-i}ms forwards`}else this._titleElement.style.animation="";switch(this._titleElement.style.top="",this._titleElement.style.bottom="",this._titleElement.style.left="",this._titleElement.style.right="",this._titleElement.style.transform="",r){case"top-left":this._titleElement.style.top="6px",this._titleElement.style.left="6px";break;case"top":this._titleElement.style.top="6px",this._titleElement.style.left="50%",this._titleElement.style.transform="translateX(-50%)";break;case"bottom-left":this._titleElement.style.bottom="6px",this._titleElement.style.left="6px";break;case"bottom":this._titleElement.style.bottom="6px",this._titleElement.style.left="50%",this._titleElement.style.transform="translateX(-50%)";break;case"bottom-right":this._titleElement.style.bottom="6px",this._titleElement.style.right="6px";break;default:this._titleElement.style.top="6px",this._titleElement.style.right="6px";break}n&&(this._titleElement.style.fontSize=n+"px")}hideActionTitle(){this._titleElement.hidden=!0}addUserOverlay(e,t){const r=this._injectedScript.document.createElement("div");r.className="x-pw-user-overlay",r.innerHTML=t;for(const n of r.querySelectorAll("script"))n.remove();for(const n of r.querySelectorAll("*"))for(const i of[...n.attributes])i.name.startsWith("on")&&n.removeAttribute(i.name);return this._userOverlays.set(e,r),this._userOverlayContainer.appendChild(r),this._userOverlayContainer.hidden=this._userOverlayHidden,e}getUserOverlay(e){return this._userOverlays.get(e)}removeUserOverlay(e){const t=this._userOverlays.get(e);t&&(t.remove(),this._userOverlays.delete(e)),this._userOverlays.size===0&&(this._userOverlayContainer.hidden=!0)}setUserOverlaysVisible(e){this._userOverlayHidden=!e,this._userOverlayContainer.hidden=!e||this._userOverlays.size===0}clearHighlight(){var e,t;for(const r of this._renderedEntries)(e=r.highlightElement)==null||e.remove(),(t=r.tooltipElement)==null||t.remove();this._renderedEntries=[]}maskElements(e,t){this.updateHighlight(e.map(r=>({element:r,color:t})))}updateHighlight(e){if(!this._highlightIsUpToDate(e)){this.clearHighlight();for(const t of e){const r=this._createHighlightElement();this._glassPaneShadow.appendChild(r);let n;if(t.tooltipText){n=this._injectedScript.document.createElement("x-pw-tooltip"),this._glassPaneShadow.appendChild(n),n.style.top="0",n.style.left="0",n.style.display="flex";const i=this._injectedScript.document.createElement("x-pw-tooltip-line");i.textContent=t.tooltipText,n.appendChild(i)}this._renderedEntries.push({targetElement:t.element,box:hn(t.box),color:t.color,borderColor:t.borderColor,fadeDuration:t.fadeDuration,cssStyle:t.cssStyle,tooltipElement:n,highlightElement:r})}for(const t of this._renderedEntries){if(!t.box&&!t.targetElement||(t.box=t.box||t.targetElement.getBoundingClientRect(),!t.tooltipElement))continue;const{anchorLeft:r,anchorTop:n}=this.tooltipPosition(t.box,t.tooltipElement);t.tooltipTop=n,t.tooltipLeft=r}for(const t of this._renderedEntries){t.tooltipElement&&(t.tooltipElement.style.top=t.tooltipTop+"px",t.tooltipElement.style.left=t.tooltipLeft+"px");const r=t.box;t.highlightElement.style.backgroundColor=t.color,t.highlightElement.style.left=r.x+"px",t.highlightElement.style.top=r.y+"px",t.highlightElement.style.width=r.width+"px",t.highlightElement.style.height=r.height+"px",t.highlightElement.style.display="block",t.borderColor&&(t.highlightElement.style.border="2px solid "+t.borderColor),t.fadeDuration&&(t.highlightElement.style.animation=`pw-fade-out ${t.fadeDuration}ms ease-out forwards`),t.cssStyle&&(t.highlightElement.style.cssText+=";"+t.cssStyle),this._isUnderTest&&console.error("Highlight box for test: "+JSON.stringify({x:r.x,y:r.y,width:r.width,height:r.height}))}}}firstBox(){var e;return(e=this._renderedEntries[0])==null?void 0:e.box}firstTooltipBox(){const e=this._renderedEntries[0];if(!(!e||!e.tooltipElement||e.tooltipLeft===void 0||e.tooltipTop===void 0))return{x:e.tooltipLeft,y:e.tooltipTop,left:e.tooltipLeft,top:e.tooltipTop,width:e.tooltipElement.offsetWidth,height:e.tooltipElement.offsetHeight,bottom:e.tooltipTop+e.tooltipElement.offsetHeight,right:e.tooltipLeft+e.tooltipElement.offsetWidth,toJSON:()=>{}}}tooltipPosition(e,t){const r=t.offsetWidth,n=t.offsetHeight,i=this._glassPaneElement.offsetWidth,s=this._glassPaneElement.offsetHeight;let o=Math.max(5,e.left);o+r>i-5&&(o=i-r-5);let a=Math.max(0,e.bottom)+5;return a+n>s-5&&(Math.max(0,e.top)>n+5?a=Math.max(0,e.top)-n-5:a=s-5-n),{anchorLeft:o,anchorTop:a}}_highlightIsUpToDate(e){if(e.length!==this._renderedEntries.length)return!1;for(let t=0;t<this._renderedEntries.length;++t){if(e[t].element!==this._renderedEntries[t].targetElement||e[t].color!==this._renderedEntries[t].color)return!1;const r=this._renderedEntries[t].box;if(!r)return!1;const n=e[t].box?hn(e[t].box):e[t].element.getBoundingClientRect();if(n.top!==r.top||n.right!==r.right||n.bottom!==r.bottom||n.left!==r.left)return!1}return!0}_createHighlightElement(){return this._injectedScript.document.createElement("x-pw-highlight")}appendChild(e){this._glassPaneShadow.appendChild(e)}onGlassPaneClick(e){this._glassPaneElement.style.pointerEvents="auto",this._glassPaneElement.style.backgroundColor="rgba(0, 0, 0, 0.3)",this._glassPaneElement.addEventListener("click",e)}offGlassPaneClick(e){this._glassPaneElement.style.pointerEvents="none",this._glassPaneElement.style.backgroundColor="transparent",this._glassPaneElement.removeEventListener("click",e)}};function hn(e){if(e)return new DOMRect(e.x,e.y,e.width,e.height)}function Zi(e,t,r){const n=e.left-t.right;if(!(n<0||r!==void 0&&n>r))return n+Math.max(t.bottom-e.bottom,0)+Math.max(e.top-t.top,0)}function es(e,t,r){const n=t.left-e.right;if(!(n<0||r!==void 0&&n>r))return n+Math.max(t.bottom-e.bottom,0)+Math.max(e.top-t.top,0)}function ts(e,t,r){const n=t.top-e.bottom;if(!(n<0||r!==void 0&&n>r))return n+Math.max(e.left-t.left,0)+Math.max(t.right-e.right,0)}function rs(e,t,r){const n=e.top-t.bottom;if(!(n<0||r!==void 0&&n>r))return n+Math.max(e.left-t.left,0)+Math.max(t.right-e.right,0)}function ns(e,t,r){const n=r===void 0?50:r;let i=0;return e.left-t.right>=0&&(i+=e.left-t.right),t.left-e.right>=0&&(i+=t.left-e.right),t.top-e.bottom>=0&&(i+=t.top-e.bottom),e.top-t.bottom>=0&&(i+=e.top-t.bottom),i>n?void 0:i}var is=["left-of","right-of","above","below","near"];function fn(e,t,r,n){const i=t.getBoundingClientRect(),s={"left-of":es,"right-of":Zi,above:ts,below:rs,near:ns}[e];let o;for(const a of r){if(a===t)continue;const l=s(i,a.getBoundingClientRect(),n);l!==void 0&&(o===void 0||l<o)&&(o=l)}return o}function ss(e,t){const r=typeof e=="string"&&!t.caseSensitive?e.toUpperCase():e,n=typeof t.value=="string"&&!t.caseSensitive?t.value.toUpperCase():t.value;return t.op==="<truthy>"?!!r:t.op==="="?n instanceof RegExp?typeof r=="string"&&!!r.match(n):r===n:typeof r!="string"||typeof n!="string"?!1:t.op==="*="?r.includes(n):t.op==="^="?r.startsWith(n):t.op==="$="?r.endsWith(n):t.op==="|="?r===n||r.startsWith(n+"-"):t.op==="~="?r.split(" ").includes(n):!1}function qt(e){const t=e.ownerDocument;return e.nodeName==="SCRIPT"||e.nodeName==="NOSCRIPT"||e.nodeName==="STYLE"||t.head&&t.head.contains(e)}function q(e,t){let r=e.get(t);if(r===void 0){if(r={full:"",normalized:"",immediate:[]},!qt(t)){let n="";if(t instanceof HTMLInputElement&&(t.type==="submit"||t.type==="button"))r={full:t.value,normalized:O(t.value),immediate:[t.value]};else{for(let i=t.firstChild;i;i=i.nextSibling)if(i.nodeType===Node.TEXT_NODE)r.full+=i.nodeValue||"",n+=i.nodeValue||"";else{if(i.nodeType===Node.COMMENT_NODE)continue;n&&r.immediate.push(n),n="",i.nodeType===Node.ELEMENT_NODE&&(r.full+=q(e,i).full)}n&&r.immediate.push(n),t.shadowRoot&&(r.full+=q(e,t.shadowRoot).full),r.full&&(r.normalized=O(r.full))}}e.set(t,r)}return r}function st(e,t,r){if(qt(t)||!r(q(e,t)))return"none";for(let n=t.firstChild;n;n=n.nextSibling)if(n.nodeType===Node.ELEMENT_NODE&&r(q(e,n)))return"selfAndChildren";return t.shadowRoot&&r(q(e,t.shadowRoot))?"selfAndChildren":"self"}function dn(e,t){const r=Wr(t);if(r)return r.map(s=>q(e,s));const n=t.getAttribute("aria-label");if(n!==null&&n.trim())return[{full:n,normalized:O(n),immediate:[n]}];const i=t.nodeName==="INPUT"&&t.type!=="hidden";if(["BUTTON","METER","OUTPUT","PROGRESS","SELECT","TEXTAREA"].includes(t.nodeName)||i){const s=t.labels;if(s)return[...s].map(o=>q(e,o))}return[]}var pn=["selected","checked","pressed","expanded","level","disabled","name","include-hidden"];pn.sort();function Be(e,t,r){if(!t.includes(r))throw new Error(`"${e}" attribute is only supported for roles: ${t.slice().sort().map(n=>`"${n}"`).join(", ")}`)}function ce(e,t){if(e.op!=="<truthy>"&&!t.includes(e.value))throw new Error(`"${e.name}" must be one of ${t.map(r=>JSON.stringify(r)).join(", ")}`)}function ue(e,t){if(!t.includes(e.op))throw new Error(`"${e.name}" does not support "${e.op}" matcher`)}function os(e,t){const r={role:t};for(const n of e)switch(n.name){case"checked":{Be(n.name,Ct,t),ce(n,[!0,!1,"mixed"]),ue(n,["<truthy>","="]),r.checked=n.op==="<truthy>"?!0:n.value;break}case"pressed":{Be(n.name,It,t),ce(n,[!0,!1,"mixed"]),ue(n,["<truthy>","="]),r.pressed=n.op==="<truthy>"?!0:n.value;break}case"selected":{Be(n.name,kt,t),ce(n,[!0,!1]),ue(n,["<truthy>","="]),r.selected=n.op==="<truthy>"?!0:n.value;break}case"expanded":{Be(n.name,$t,t),ce(n,[!0,!1]),ue(n,["<truthy>","="]),r.expanded=n.op==="<truthy>"?!0:n.value;break}case"level":{if(Be(n.name,Lt,t),typeof n.value=="string"&&(n.value=+n.value),n.op!=="="||typeof n.value!="number"||Number.isNaN(n.value))throw new Error(\'"level" attribute must be compared to a number\');r.level=n.value;break}case"disabled":{ce(n,[!0,!1]),ue(n,["<truthy>","="]),r.disabled=n.op==="<truthy>"?!0:n.value;break}case"name":{if(n.op==="<truthy>")throw new Error(\'"name" attribute must have a value\');if(typeof n.value!="string"&&!(n.value instanceof RegExp))throw new Error(\'"name" attribute must be a string or a regular expression\');r.name=n.value,r.nameOp=n.op,r.exact=n.caseSensitive;break}case"include-hidden":{ce(n,[!0,!1]),ue(n,["<truthy>","="]),r.includeHidden=n.op==="<truthy>"?!0:n.value;break}default:throw new Error(`Unknown attribute "${n.name}", must be one of ${pn.map(i=>`"${i}"`).join(", ")}.`)}return r}function as(e,t,r){const n=[],i=o=>{if(I(o)===t.role&&!(t.selected!==void 0&&zr(o)!==t.selected)&&!(t.checked!==void 0&&Gr(o)!==t.checked)&&!(t.pressed!==void 0&&Jr(o)!==t.pressed)&&!(t.expanded!==void 0&&Xr(o)!==t.expanded)&&!(t.level!==void 0&&Qr(o)!==t.level)&&!(t.disabled!==void 0&&et(o)!==t.disabled)&&!(!t.includeHidden&&U(o))){if(t.name!==void 0){const a=O(ke(o,!!t.includeHidden));if(typeof t.name=="string"&&(t.name=O(t.name)),r&&!t.exact&&t.nameOp==="="&&(t.nameOp="*="),!ss(a,{name:"",jsonPath:[],op:t.nameOp||"=",value:t.name,caseSensitive:!!t.exact}))return}n.push(o)}},s=o=>{const a=[];o.shadowRoot&&a.push(o.shadowRoot);for(const l of o.querySelectorAll("*"))i(l),l.shadowRoot&&a.push(l.shadowRoot);a.forEach(s)};return s(e),n}function gn(e){return{queryAll:(t,r)=>{const n=ve(r,!0),i=n.name.toLowerCase();if(!i)throw new Error("Role must not be empty");const s=os(n.attributes,i);tt();try{return as(t,s,e)}finally{rt()}}}}var ls=class{constructor(){this._retainCacheCounter=0,this._cacheText=new Map,this._cacheQueryCSS=new Map,this._cacheMatches=new Map,this._cacheQuery=new Map,this._cacheMatchesSimple=new Map,this._cacheMatchesParents=new Map,this._cacheCallMatches=new Map,this._cacheCallQuery=new Map,this._cacheQuerySimple=new Map,this._engines=new Map,this._engines.set("not",hs),this._engines.set("is",He),this._engines.set("where",He),this._engines.set("has",cs),this._engines.set("scope",us),this._engines.set("light",fs),this._engines.set("visible",ds),this._engines.set("text",ps),this._engines.set("text-is",gs),this._engines.set("text-matches",ms),this._engines.set("has-text",bs),this._engines.set("right-of",Fe("right-of")),this._engines.set("left-of",Fe("left-of")),this._engines.set("above",Fe("above")),this._engines.set("below",Fe("below")),this._engines.set("near",Fe("near")),this._engines.set("nth-match",vs);const e=[...this._engines.keys()];e.sort();const t=[...xr];if(t.sort(),e.join("|")!==t.join("|"))throw new Error(`Please keep customCSSNames in sync with evaluator engines: ${e.join("|")} vs ${t.join("|")}`)}begin(){++this._retainCacheCounter}end(){--this._retainCacheCounter,this._retainCacheCounter||(this._cacheQueryCSS.clear(),this._cacheMatches.clear(),this._cacheQuery.clear(),this._cacheMatchesSimple.clear(),this._cacheMatchesParents.clear(),this._cacheCallMatches.clear(),this._cacheCallQuery.clear(),this._cacheQuerySimple.clear(),this._cacheText.clear())}_cached(e,t,r,n){e.has(t)||e.set(t,[]);const i=e.get(t),s=i.find(a=>r.every((l,c)=>a.rest[c]===l));if(s)return s.result;const o=n();return i.push({rest:r,result:o}),o}_checkSelector(e){if(!(typeof e=="object"&&e&&(Array.isArray(e)||"simples"in e&&e.simples.length)))throw new Error(`Malformed selector "${e}"`);return e}matches(e,t,r){const n=this._checkSelector(t);this.begin();try{return this._cached(this._cacheMatches,e,[n,r.scope,r.pierceShadow,r.originalScope],()=>Array.isArray(n)?this._matchesEngine(He,e,n,r):(this._hasScopeClause(n)&&(r=this._expandContextForScopeMatching(r)),this._matchesSimple(e,n.simples[n.simples.length-1].selector,r)?this._matchesParents(e,n,n.simples.length-2,r):!1))}finally{this.end()}}query(e,t){const r=this._checkSelector(t);this.begin();try{return this._cached(this._cacheQuery,r,[e.scope,e.pierceShadow,e.originalScope],()=>{if(Array.isArray(r))return this._queryEngine(He,e,r);this._hasScopeClause(r)&&(e=this._expandContextForScopeMatching(e));const n=this._scoreMap;this._scoreMap=new Map;let i=this._querySimple(e,r.simples[r.simples.length-1].selector);return i=i.filter(s=>this._matchesParents(s,r,r.simples.length-2,e)),this._scoreMap.size&&i.sort((s,o)=>{const a=this._scoreMap.get(s),l=this._scoreMap.get(o);return a===l?0:a===void 0?1:l===void 0?-1:a-l}),this._scoreMap=n,i})}finally{this.end()}}_markScore(e,t){this._scoreMap&&this._scoreMap.set(e,t)}_hasScopeClause(e){return e.simples.some(t=>t.selector.functions.some(r=>r.name==="scope"))}_expandContextForScopeMatching(e){if(e.scope.nodeType!==1)return e;const t=$(e.scope);return t?{...e,scope:t,originalScope:e.originalScope||e.scope}:e}_matchesSimple(e,t,r){return this._cached(this._cacheMatchesSimple,e,[t,r.scope,r.pierceShadow,r.originalScope],()=>{if(e===r.scope||t.css&&!this._matchesCSS(e,t.css))return!1;for(const n of t.functions)if(!this._matchesEngine(this._getEngine(n.name),e,n.args,r))return!1;return!0})}_querySimple(e,t){return t.functions.length?this._cached(this._cacheQuerySimple,t,[e.scope,e.pierceShadow,e.originalScope],()=>{let r=t.css;const n=t.functions;r==="*"&&n.length&&(r=void 0);let i,s=-1;r!==void 0?i=this._queryCSS(e,r):(s=n.findIndex(o=>this._getEngine(o.name).query!==void 0),s===-1&&(s=0),i=this._queryEngine(this._getEngine(n[s].name),e,n[s].args));for(let o=0;o<n.length;o++){if(o===s)continue;const a=this._getEngine(n[o].name);a.matches!==void 0&&(i=i.filter(l=>this._matchesEngine(a,l,n[o].args,e)))}for(let o=0;o<n.length;o++){if(o===s)continue;const a=this._getEngine(n[o].name);a.matches===void 0&&(i=i.filter(l=>this._matchesEngine(a,l,n[o].args,e)))}return i}):this._queryCSS(e,t.css||"*")}_matchesParents(e,t,r,n){return r<0?!0:this._cached(this._cacheMatchesParents,e,[t,r,n.scope,n.pierceShadow,n.originalScope],()=>{const{selector:i,combinator:s}=t.simples[r];if(s===">"){const o=ot(e,n);return!o||!this._matchesSimple(o,i,n)?!1:this._matchesParents(o,t,r-1,n)}if(s==="+"){const o=Dt(e,n);return!o||!this._matchesSimple(o,i,n)?!1:this._matchesParents(o,t,r-1,n)}if(s===""){let o=ot(e,n);for(;o;){if(this._matchesSimple(o,i,n)){if(this._matchesParents(o,t,r-1,n))return!0;if(t.simples[r-1].combinator==="")break}o=ot(o,n)}return!1}if(s==="~"){let o=Dt(e,n);for(;o;){if(this._matchesSimple(o,i,n)){if(this._matchesParents(o,t,r-1,n))return!0;if(t.simples[r-1].combinator==="~")break}o=Dt(o,n)}return!1}if(s===">="){let o=e;for(;o;){if(this._matchesSimple(o,i,n)){if(this._matchesParents(o,t,r-1,n))return!0;if(t.simples[r-1].combinator==="")break}o=ot(o,n)}return!1}throw new Error(`Unsupported combinator "${s}"`)})}_matchesEngine(e,t,r,n){if(e.matches)return this._callMatches(e,t,r,n);if(e.query)return this._callQuery(e,r,n).includes(t);throw new Error(\'Selector engine should implement "matches" or "query"\')}_queryEngine(e,t,r){if(e.query)return this._callQuery(e,r,t);if(e.matches)return this._queryCSS(t,"*").filter(n=>this._callMatches(e,n,r,t));throw new Error(\'Selector engine should implement "matches" or "query"\')}_callMatches(e,t,r,n){return this._cached(this._cacheCallMatches,t,[e,n.scope,n.pierceShadow,n.originalScope,...r],()=>e.matches(t,r,n,this))}_callQuery(e,t,r){return this._cached(this._cacheCallQuery,e,[r.scope,r.pierceShadow,r.originalScope,...t],()=>e.query(r,t,this))}_matchesCSS(e,t){return e.matches(t)}_queryCSS(e,t){return this._cached(this._cacheQueryCSS,t,[e.scope,e.pierceShadow,e.originalScope],()=>{let r=[];function n(i){if(r=r.concat([...i.querySelectorAll(t)]),!!e.pierceShadow){i.shadowRoot&&n(i.shadowRoot);for(const s of i.querySelectorAll("*"))s.shadowRoot&&n(s.shadowRoot)}}return n(e.scope),r})}_getEngine(e){const t=this._engines.get(e);if(!t)throw new Error(`Unknown selector engine "${e}"`);return t}},He={matches(e,t,r,n){if(t.length===0)throw new Error(\'"is" engine expects non-empty selector list\');return t.some(i=>n.matches(e,i,r))},query(e,t,r){if(t.length===0)throw new Error(\'"is" engine expects non-empty selector list\');let n=[];for(const i of t)n=n.concat(r.query(e,i));return t.length===1?n:mn(n)}},cs={matches(e,t,r,n){if(t.length===0)throw new Error(\'"has" engine expects non-empty selector list\');return n.query({...r,scope:e},t).length>0}},us={matches(e,t,r,n){if(t.length!==0)throw new Error(\'"scope" engine expects no arguments\');const i=r.originalScope||r.scope;return i.nodeType===9?e===i.documentElement:e===i},query(e,t,r){if(t.length!==0)throw new Error(\'"scope" engine expects no arguments\');const n=e.originalScope||e.scope;if(n.nodeType===9){const i=n.documentElement;return i?[i]:[]}return n.nodeType===1?[n]:[]}},hs={matches(e,t,r,n){if(t.length===0)throw new Error(\'"not" engine expects non-empty selector list\');return!n.matches(e,t,r)}},fs={query(e,t,r){return r.query({...e,pierceShadow:!1},t)},matches(e,t,r,n){return n.matches(e,t,{...r,pierceShadow:!1})}},ds={matches(e,t,r,n){if(t.length)throw new Error(\'"visible" engine expects no arguments\');return Q(e)}},ps={matches(e,t,r,n){if(t.length!==1||typeof t[0]!="string")throw new Error(\'"text" engine expects a single string\');const i=O(t[0]).toLowerCase(),s=o=>o.normalized.toLowerCase().includes(i);return st(n._cacheText,e,s)==="self"}},gs={matches(e,t,r,n){if(t.length!==1||typeof t[0]!="string")throw new Error(\'"text-is" engine expects a single string\');const i=O(t[0]),s=o=>!i&&!o.immediate.length?!0:o.immediate.some(a=>O(a)===i);return st(n._cacheText,e,s)!=="none"}},ms={matches(e,t,r,n){if(t.length===0||typeof t[0]!="string"||t.length>2||t.length===2&&typeof t[1]!="string")throw new Error(\'"text-matches" engine expects a regexp body and optional regexp flags\');const i=new RegExp(t[0],t.length===2?t[1]:void 0),s=o=>i.test(o.full);return st(n._cacheText,e,s)==="self"}},bs={matches(e,t,r,n){if(t.length!==1||typeof t[0]!="string")throw new Error(\'"has-text" engine expects a single string\');if(qt(e))return!1;const i=O(t[0]).toLowerCase();return(o=>o.normalized.toLowerCase().includes(i))(q(n._cacheText,e))}};function Fe(e){return{matches(t,r,n,i){const s=r.length&&typeof r[r.length-1]=="number"?r[r.length-1]:void 0,o=s===void 0?r:r.slice(0,r.length-1);if(r.length<1+(s===void 0?0:1))throw new Error(`"${e}" engine expects a selector list and optional maximum distance in pixels`);const a=i.query(n,o),l=fn(e,t,a,s);return l===void 0?!1:(i._markScore(t,l),!0)}}}var vs={query(e,t,r){let n=t[t.length-1];if(t.length<2)throw new Error(\'"nth-match" engine expects non-empty selector list and an index argument\');if(typeof n!="number"||n<1)throw new Error(\'"nth-match" engine expects a one-based index as the last argument\');const i=He.query(e,t.slice(0,t.length-1),r);return n--,n<i.length?[i[n]]:[]}};function ot(e,t){if(e!==t.scope)return t.pierceShadow?$(e):e.parentElement||void 0}function Dt(e,t){if(e!==t.scope)return e.previousElementSibling||void 0}function mn(e){const t=new Map,r=[],n=[];function i(o){let a=t.get(o);if(a)return a;const l=$(o);return l?i(l).children.push(o):r.push(o),a={children:[],taken:!1},t.set(o,a),a}for(const o of e)i(o).taken=!0;function s(o){const a=t.get(o);if(a.taken&&n.push(o),a.children.length>1){const l=new Set(a.children);a.children=[];let c=o.firstElementChild;for(;c&&a.children.length<l.size;)l.has(c)&&a.children.push(c),c=c.nextElementSibling;for(c=o.shadowRoot?o.shadowRoot.firstElementChild:null;c&&a.children.length<l.size;)l.has(c)&&a.children.push(c),c=c.nextElementSibling}a.children.forEach(s)}return r.forEach(s),n}var bn=10,he=bn/2,vn=1,ws=2,xs=10,ys=50,wn=100,xn=120,yn=140,En=160,at=180,Sn=200,_n=250,Es=xn+he,Ss=yn+he,_s=wn+he,Ts=En+he,As=at+he,ks=Sn+he,Cs=300,Ns=500,Tn=510,Bt=520,An=530,Ht=1e4,Is=1e7,$s=1e3;function kn(e,t,r){var n;e._evaluator.begin();const i={allowText:new Map,disallowText:new Map};tt(),_t();try{let s=[];if(r.forTextExpect){let l=Ue(e,t.ownerDocument.documentElement,r);for(let c=t;c;c=$(c)){const h=ee(i,e,c,{...r,noText:!0});if(!h)continue;if(te(h)<=$s){l=h;break}}s=[lt(l)]}else{if(!t.matches("input,textarea,select")&&!t.isContentEditable){const l=ye(t,"button,select,input,[role=button],[role=checkbox],[role=radio],a,[role=link]",r.root);l&&Q(l)&&(t=l)}if(r.multiple){const l=ee(i,e,t,r),c=ee(i,e,t,{...r,noText:!0});let h=[l,c];if(i.allowText.clear(),i.disallowText.clear(),l&&Ft(l)&&h.push(ee(i,e,t,{...r,noCSSId:!0})),c&&Ft(c)&&h.push(ee(i,e,t,{...r,noText:!0,noCSSId:!0})),h=h.filter(Boolean),!h.length){const f=Ue(e,t,r);h.push(f),Ft(f)&&h.push(Ue(e,t,{...r,noCSSId:!0}))}s=[...new Set(h.map(f=>lt(f)))]}else{const l=ee(i,e,t,r)||Ue(e,t,r);s=[lt(l)]}}const o=s[0],a=e.parseSelector(o);return{selector:o,selectors:s,elements:e.querySelectorAll(a,(n=r.root)!=null?n:t.ownerDocument)}}finally{Tt(),rt(),e._evaluator.end()}}function ee(e,t,r,n){var i;if(n.root&&!St(n.root,r))throw new Error("Target element must belong to the root\'s subtree");if(r===n.root)return[{engine:"css",selector:":scope",score:1}];if(r.ownerDocument.documentElement===r)return[{engine:"css",selector:"html",score:1}];let s=null;const o=l=>{(!s||te(l)<te(s))&&(s=l)},a=[];if(!n.noText)for(const l of Rs(t,r,!n.isRecursive))a.push({candidate:l,isTextCandidate:!0});for(const l of Ls(t,r,n))n.omitInternalEngines&&l.engine.startsWith("internal:")||a.push({candidate:[l],isTextCandidate:!1});a.sort((l,c)=>te(l.candidate)-te(c.candidate));for(const{candidate:l,isTextCandidate:c}of a){const h=t.querySelectorAll(t.parseSelector(lt(l)),(i=n.root)!=null?i:r.ownerDocument);if(!h.includes(r))continue;if(h.length===1){o(l);break}const f=h.indexOf(r);if(!(f>5)&&(o([...l,{engine:"nth",selector:String(f),score:Ht}]),!n.isRecursive))for(let u=$(r);u&&u!==n.root;u=$(u)){const p=h.filter(L=>St(u,L)&&L!==u),d=p.indexOf(r);if(p.length>5||d===-1||d===f&&p.length>1)continue;const g=p.length===1?l:[...l,{engine:"nth",selector:String(d),score:Ht}];if(s&&te([{engine:"",selector:"",score:1},...g])>=te(s))continue;const w=!!n.noText||c,x=w?e.disallowText:e.allowText;let y=x.get(u);y===void 0&&(y=ee(e,t,u,{...n,isRecursive:!0,noText:w})||Ue(t,u,n),x.set(u,y)),y&&o([...y,...g])}}return s}function Ls(e,t,r){const n=[];{for(const o of["data-testid","data-test-id","data-test"])o!==r.testIdAttributeName&&t.getAttribute(o)&&n.push({engine:"css",selector:`[${o}=${se(t.getAttribute(o))}]`,score:ws});if(!r.noCSSId){const o=t.getAttribute("id");o&&!Os(o)&&n.push({engine:"css",selector:Cn(o),score:Ns})}n.push({engine:"css",selector:G(t),score:An})}if(t.nodeName==="IFRAME"){for(const o of["name","title"])t.getAttribute(o)&&n.push({engine:"css",selector:`${G(t)}[${o}=${se(t.getAttribute(o))}]`,score:xs});return t.getAttribute(r.testIdAttributeName)&&n.push({engine:"css",selector:`[${r.testIdAttributeName}=${se(t.getAttribute(r.testIdAttributeName))}]`,score:vn}),Ut([n]),n}if(t.getAttribute(r.testIdAttributeName)&&n.push({engine:"internal:testid",selector:`[${r.testIdAttributeName}=${F(t.getAttribute(r.testIdAttributeName),!0)}]`,score:vn}),t.nodeName==="INPUT"||t.nodeName==="TEXTAREA"){const o=t;if(o.placeholder){n.push({engine:"internal:attr",selector:`[placeholder=${F(o.placeholder,!0)}]`,score:Es});for(const a of fe(o.placeholder))n.push({engine:"internal:attr",selector:`[placeholder=${F(a.text,!1)}]`,score:xn-a.scoreBonus})}}const i=dn(e._evaluator._cacheText,t);for(const o of i){const a=o.normalized;n.push({engine:"internal:label",selector:H(a,!0),score:Ss});for(const l of fe(a))n.push({engine:"internal:label",selector:H(l.text,!1),score:yn-l.scoreBonus})}const s=I(t);return s&&!["none","presentation"].includes(s)&&n.push({engine:"internal:role",selector:s,score:Tn}),t.getAttribute("name")&&["BUTTON","FORM","FIELDSET","FRAME","IFRAME","INPUT","KEYGEN","OBJECT","OUTPUT","SELECT","TEXTAREA","MAP","META","PARAM"].includes(t.nodeName)&&n.push({engine:"css",selector:`${G(t)}[name=${se(t.getAttribute("name"))}]`,score:Bt}),["INPUT","TEXTAREA"].includes(t.nodeName)&&t.getAttribute("type")!=="hidden"&&t.getAttribute("type")&&n.push({engine:"css",selector:`${G(t)}[type=${se(t.getAttribute("type"))}]`,score:Bt}),["INPUT","TEXTAREA","SELECT"].includes(t.nodeName)&&t.getAttribute("type")!=="hidden"&&n.push({engine:"css",selector:G(t),score:Bt+1}),Ut([n]),n}function Rs(e,t,r){if(t.nodeName==="SELECT")return[];const n=[],i=t.getAttribute("title");if(i){n.push([{engine:"internal:attr",selector:`[title=${F(i,!0)}]`,score:ks}]);for(const c of fe(i))n.push([{engine:"internal:attr",selector:`[title=${F(c.text,!1)}]`,score:Sn-c.scoreBonus}])}const s=t.getAttribute("alt");if(s&&["APPLET","AREA","IMG","INPUT"].includes(t.nodeName)){n.push([{engine:"internal:attr",selector:`[alt=${F(s,!0)}]`,score:Ts}]);for(const c of fe(s))n.push([{engine:"internal:attr",selector:`[alt=${F(c.text,!1)}]`,score:En-c.scoreBonus}])}const o=q(e._evaluator._cacheText,t).normalized,a=o?fe(o):[];if(o){if(r){o.length<=80&&n.push([{engine:"internal:text",selector:H(o,!0),score:As}]);for(const h of a)n.push([{engine:"internal:text",selector:H(h.text,!1),score:at-h.scoreBonus}])}const c={engine:"css",selector:G(t),score:An};for(const h of a)n.push([c,{engine:"internal:has-text",selector:H(h.text,!1),score:at-h.scoreBonus}]);if(r&&o.length<=80){const h=new RegExp("^"+Ke(o)+"$");n.push([c,{engine:"internal:has-text",selector:H(h,!1),score:_n}])}}const l=I(t);if(l&&!["none","presentation"].includes(l)){const c=ke(t,!1);if(c&&!c.match(/^\\p{Co}+$/u)){const h={engine:"internal:role",selector:`${l}[name=${F(c,!0)}]`,score:_s};n.push([h]);for(const f of fe(c))n.push([{engine:"internal:role",selector:`${l}[name=${F(f.text,!1)}]`,score:wn-f.scoreBonus}])}else{const h={engine:"internal:role",selector:`${l}`,score:Tn};for(const f of a)n.push([h,{engine:"internal:has-text",selector:H(f.text,!1),score:at-f.scoreBonus}]);if(r&&o.length<=80){const f=new RegExp("^"+Ke(o)+"$");n.push([h,{engine:"internal:has-text",selector:H(f,!1),score:_n}])}}}return Ut(n),n}function Cn(e){return/^[a-zA-Z][a-zA-Z0-9\\-\\_]+$/.test(e)?"#"+e:`[id=${se(e)}]`}function Ft(e){return e.some(t=>t.engine==="css"&&(t.selector.startsWith("#")||t.selector.startsWith(\'[id="\')))}function Ue(e,t,r){var n;const i=(n=r.root)!=null?n:t.ownerDocument,s=[];function o(l){const c=s.slice();l&&c.unshift(l);const h=c.join(" > "),f=e.parseSelector(h);return e.querySelector(f,i,!1)===t?h:void 0}function a(l){const c={engine:"css",selector:l,score:Is},h=e.parseSelector(l),f=e.querySelectorAll(h,i);if(f.length===1)return[c];const u={engine:"nth",selector:String(f.indexOf(t)),score:Ht};return[c,u]}for(let l=t;l&&l!==i;l=$(l)){let c="";if(l.id&&!r.noCSSId){const u=Cn(l.id),p=o(u);if(p)return a(p);c=u}const h=l.parentNode,f=[...l.classList].map(Ps);for(let u=0;u<f.length;++u){const p="."+f.slice(0,u+1).join("."),d=o(p);if(d)return a(d);!c&&h&&h.querySelectorAll(p).length===1&&(c=p)}if(h){const u=[...h.children],p=l.nodeName,g=u.filter(w=>w.nodeName===p).indexOf(l)===0?G(l):`${G(l)}:nth-child(${1+u.indexOf(l)})`,m=o(g);if(m)return a(m);c||(c=g)}else c||(c=G(l));s.unshift(c)}return a(o())}function Ut(e){for(const t of e)for(const r of t)r.score>ys&&r.score<Cs&&(r.score+=Math.min(bn,r.selector.length/10|0))}function lt(e){const t=[];let r="";for(const{engine:n,selector:i}of e)t.length&&(r!=="css"||n!=="css"||i.startsWith(":nth-match("))&&t.push(">>"),r=n,n==="css"?t.push(i):t.push(`${n}=${i}`);return t.join(" ")}function te(e){let t=0;for(let r=0;r<e.length;r++)t+=e[r].score*(e.length-r);return t}function Os(e){let t,r=0;for(let n=0;n<e.length;++n){const i=e[n];let s;if(!(i==="-"||i==="_")){if(i>="a"&&i<="z"?s="lower":i>="A"&&i<="Z"?s="upper":i>="0"&&i<="9"?s="digit":s="other",s==="lower"&&t==="upper"){t=s;continue}t&&t!==s&&++r,t=s}}return r>=e.length/4}function ct(e,t){if(e.length<=t)return e;e=e.substring(0,t);const r=e.match(/^(.*)\\b(.+?)$/);return r?r[1].trimEnd():""}function fe(e){let t=[];{const r=e.match(/^([\\d.,]+)[^.,\\w]/),n=r?r[1].length:0;if(n){const i=ct(e.substring(n).trimStart(),80);t.push({text:i,scoreBonus:i.length<=30?2:1})}}{const r=e.match(/[^.,\\w]([\\d.,]+)$/),n=r?r[1].length:0;if(n){const i=ct(e.substring(0,e.length-n).trimEnd(),80);t.push({text:i,scoreBonus:i.length<=30?2:1})}}return e.length<=30?t.push({text:e,scoreBonus:0}):(t.push({text:ct(e,80),scoreBonus:0}),t.push({text:ct(e,30),scoreBonus:1})),t=t.filter(r=>r.text),t.length||t.push({text:e.substring(0,80),scoreBonus:0}),t}function G(e){return e.nodeName.toLocaleLowerCase().replace(/[:\\.]/g,t=>"\\\\"+t)}function Ps(e){let t="";for(let r=0;r<e.length;r++)t+=Ms(e,r);return t}function Ms(e,t){const r=e.charCodeAt(t);return r===0?"\\uFFFD":r>=1&&r<=31||r>=48&&r<=57&&(t===0||t===1&&e.charCodeAt(0)===45)?"\\\\"+r.toString(16)+" ":t===0&&r===45&&e.length===1?"\\\\"+e.charAt(t):r>=128||r===45||r===95||r>=48&&r<=57||r>=65&&r<=90||r>=97&&r<=122?e.charAt(t):"\\\\"+e.charAt(t)}var Nn={queryAll(e,t){t.startsWith("/")&&e.nodeType!==Node.DOCUMENT_NODE&&(t="."+t);const r=[],n=e.ownerDocument||e;if(!n)return r;const i=n.evaluate(t,e,null,XPathResult.ORDERED_NODE_ITERATOR_TYPE);for(let s=i.iterateNext();s;s=i.iterateNext())s.nodeType===Node.ELEMENT_NODE&&r.push(s);return r}};function jt(e,t,r){return`internal:attr=[${e}=${F(t,r?.exact||!1)}]`}function qs(e,t){return`internal:testid=[${e}=${F(t,!0)}]`}function Ds(e,t){return"internal:label="+H(e,!!t?.exact)}function Bs(e,t){return jt("alt",e,t)}function Hs(e,t){return jt("title",e,t)}function Fs(e,t){return jt("placeholder",e,t)}function Us(e,t){return"internal:text="+H(e,!!t?.exact)}function js(e,t={}){const r=[];return t.checked!==void 0&&r.push(["checked",String(t.checked)]),t.disabled!==void 0&&r.push(["disabled",String(t.disabled)]),t.selected!==void 0&&r.push(["selected",String(t.selected)]),t.expanded!==void 0&&r.push(["expanded",String(t.expanded)]),t.includeHidden!==void 0&&r.push(["include-hidden",String(t.includeHidden)]),t.level!==void 0&&r.push(["level",String(t.level)]),t.name!==void 0&&r.push(["name",F(t.name,!!t.exact)]),t.pressed!==void 0&&r.push(["pressed",String(t.pressed)]),`internal:role=${e}${r.map(([n,i])=>`[${n}=${i}]`).join("")}`}var de=Symbol("selector"),Ws=class je{constructor(t,r,n){if(n?.hasText&&(r+=` >> internal:has-text=${H(n.hasText,!1)}`),n?.hasNotText&&(r+=` >> internal:has-not-text=${H(n.hasNotText,!1)}`),n?.has&&(r+=" >> internal:has="+JSON.stringify(n.has[de])),n?.hasNot&&(r+=" >> internal:has-not="+JSON.stringify(n.hasNot[de])),n?.visible!==void 0&&(r+=` >> visible=${n.visible?"true":"false"}`),this[de]=r,r){const o=t.parseSelector(r);this.element=t.querySelector(o,t.document,!1),this.elements=t.querySelectorAll(o,t.document)}const i=r,s=this;s.locator=(o,a)=>new je(t,i?i+" >> "+o:o,a),s.getByTestId=o=>s.locator(qs(t.testIdAttributeNameForStrictErrorAndConsoleCodegen(),o)),s.getByAltText=(o,a)=>s.locator(Bs(o,a)),s.getByLabel=(o,a)=>s.locator(Ds(o,a)),s.getByPlaceholder=(o,a)=>s.locator(Fs(o,a)),s.getByText=(o,a)=>s.locator(Us(o,a)),s.getByTitle=(o,a)=>s.locator(Hs(o,a)),s.getByRole=(o,a={})=>s.locator(js(o,a)),s.filter=o=>new je(t,r,o),s.first=()=>s.locator("nth=0"),s.last=()=>s.locator("nth=-1"),s.nth=o=>s.locator(`nth=${o}`),s.and=o=>new je(t,i+" >> internal:and="+JSON.stringify(o[de])),s.or=o=>new je(t,i+" >> internal:or="+JSON.stringify(o[de]))}},Vs=Ws,zs=class{constructor(e){this._injectedScript=e}install(){this._injectedScript.window.playwright||(this._injectedScript.window.playwright={$:(e,t)=>this._querySelector(e,!!t),$$:e=>this._querySelectorAll(e),inspect:e=>this._inspect(e),selector:e=>this._selector(e),generateLocator:(e,t)=>this._generateLocator(e,t),ariaSnapshot:(e,t)=>this._injectedScript.ariaSnapshot(e||this._injectedScript.document.body,t||{mode:"default"}),resume:()=>this._resume(),...new Vs(this._injectedScript,"")},delete this._injectedScript.window.playwright.filter,delete this._injectedScript.window.playwright.first,delete this._injectedScript.window.playwright.last,delete this._injectedScript.window.playwright.nth,delete this._injectedScript.window.playwright.and,delete this._injectedScript.window.playwright.or)}_querySelector(e,t){if(typeof e!="string")throw new Error("Usage: playwright.query(\'Playwright >> selector\').");const r=this._injectedScript.parseSelector(e);return this._injectedScript.querySelector(r,this._injectedScript.document,t)}_querySelectorAll(e){if(typeof e!="string")throw new Error("Usage: playwright.$$(\'Playwright >> selector\').");const t=this._injectedScript.parseSelector(e);return this._injectedScript.querySelectorAll(t,this._injectedScript.document)}_inspect(e){if(typeof e!="string")throw new Error("Usage: playwright.inspect(\'Playwright >> selector\').");this._injectedScript.window.inspect(this._querySelector(e,!1))}_selector(e){if(!(e instanceof Element))throw new Error("Usage: playwright.selector(element).");return this._injectedScript.generateSelectorSimple(e)}_generateLocator(e,t){if(!(e instanceof Element))throw new Error("Usage: playwright.locator(element).");const r=this._injectedScript.generateSelectorSimple(e);return oe(t||"javascript",r)}_resume(){if(!this._injectedScript.window.__pw_resume)return!1;this._injectedScript.window.__pw_resume().catch(()=>{})}};function Gs(e){try{return e instanceof RegExp||Object.prototype.toString.call(e)==="[object RegExp]"}catch{return!1}}function Js(e){try{return e instanceof Date||Object.prototype.toString.call(e)==="[object Date]"}catch{return!1}}function Xs(e){try{return e instanceof URL||Object.prototype.toString.call(e)==="[object URL]"}catch{return!1}}function Qs(e){var t;try{return e instanceof Error||e&&((t=Object.getPrototypeOf(e))==null?void 0:t.name)==="Error"}catch{return!1}}function Ys(e,t){try{return e instanceof t||Object.prototype.toString.call(e)===`[object ${t.name}]`}catch{return!1}}function Ks(e){try{return e instanceof ArrayBuffer||Object.prototype.toString.call(e)==="[object ArrayBuffer]"}catch{return!1}}var In={i8:Int8Array,ui8:Uint8Array,ui8c:Uint8ClampedArray,i16:Int16Array,ui16:Uint16Array,i32:Int32Array,ui32:Uint32Array,f32:Float32Array,f64:Float64Array,bi64:BigInt64Array,bui64:BigUint64Array};function $n(e){if("toBase64"in e)return e.toBase64();const t=Array.from(new Uint8Array(e.buffer,e.byteOffset,e.byteLength)).map(r=>String.fromCharCode(r)).join("");return btoa(t)}function Ln(e,t){const r=atob(e),n=new Uint8Array(r.length);for(let i=0;i<r.length;i++)n[i]=r.charCodeAt(i);return new t(n.buffer)}function Wt(e,t=[],r=new Map){if(!Object.is(e,void 0)){if(typeof e=="object"&&e){if("ref"in e)return r.get(e.ref);if("v"in e)return e.v==="undefined"?void 0:e.v==="null"?null:e.v==="NaN"?NaN:e.v==="Infinity"?1/0:e.v==="-Infinity"?-1/0:e.v==="-0"?-0:void 0;if("d"in e)return new Date(e.d);if("u"in e)return new URL(e.u);if("bi"in e)return BigInt(e.bi);if("e"in e){const n=new Error(e.e.m);return n.name=e.e.n,n.stack=e.e.s,n}if("r"in e)return new RegExp(e.r.p,e.r.f);if("a"in e){const n=[];r.set(e.id,n);for(const i of e.a)n.push(Wt(i,t,r));return n}if("o"in e){const n={};r.set(e.id,n);for(const{k:i,v:s}of e.o)i!=="__proto__"&&(n[i]=Wt(s,t,r));return n}if("h"in e)return t[e.h];if("ta"in e)return Ln(e.ta.b,In[e.ta.k]);if("ab"in e)return Ln(e.ab.b,Uint8Array).buffer}return e}}function Zs(e,t){return Vt(e,t,{visited:new Map,lastId:0})}function Vt(e,t,r){if(e&&typeof e=="object"){if(typeof globalThis.Window=="function"&&e instanceof globalThis.Window)return"ref: <Window>";if(typeof globalThis.Document=="function"&&e instanceof globalThis.Document)return"ref: <Document>";if(typeof globalThis.Node=="function"&&e instanceof globalThis.Node)return"ref: <Node>"}return Rn(e,t,r)}function Rn(e,t,r){var n;const i=t(e);if("fallThrough"in i)e=i.fallThrough;else return i;if(typeof e=="symbol")return{v:"undefined"};if(Object.is(e,void 0))return{v:"undefined"};if(Object.is(e,null))return{v:"null"};if(Object.is(e,NaN))return{v:"NaN"};if(Object.is(e,1/0))return{v:"Infinity"};if(Object.is(e,-1/0))return{v:"-Infinity"};if(Object.is(e,-0))return{v:"-0"};if(typeof e=="boolean"||typeof e=="number"||typeof e=="string")return e;if(typeof e=="bigint")return{bi:e.toString()};if(Qs(e)){let o;return(n=e.stack)!=null&&n.startsWith(e.name+": "+e.message)?o=e.stack:o=`${e.name}: ${e.message}\n${e.stack}`,{e:{n:e.name,m:e.message,s:o}}}if(Js(e))return{d:e.toJSON()};if(Xs(e))return{u:e.toJSON()};if(Gs(e))return{r:{p:e.source,f:e.flags}};for(const[o,a]of Object.entries(In))if(Ys(e,a))return{ta:{b:$n(e),k:o}};if(Ks(e))return{ab:{b:$n(new Uint8Array(e))}};const s=r.visited.get(e);if(s)return{ref:s};if(Array.isArray(e)){const o=[],a=++r.lastId;r.visited.set(e,a);for(let l=0;l<e.length;++l)o.push(Vt(e[l],t,r));return{a:o,id:a}}if(typeof e=="object"){const o=[],a=++r.lastId;r.visited.set(e,a);for(const c of Object.keys(e)){let h;try{h=e[c]}catch{continue}c==="toJSON"&&typeof h=="function"?o.push({k:c,v:{o:[],id:0}}):o.push({k:c,v:Vt(h,t,r)})}let l;try{o.length===0&&e.toJSON&&typeof e.toJSON=="function"&&(l={value:e.toJSON()})}catch{}return l?Rn(l.value,t,r):{o,id:a}}}var eo=class{constructor(e,t){var r,n,i,s,o,a,l,c;this.global=e,this.isUnderTest=t,e.__pwClock?this.builtins=e.__pwClock.builtins:this.builtins={setTimeout:(r=e.setTimeout)==null?void 0:r.bind(e),clearTimeout:(n=e.clearTimeout)==null?void 0:n.bind(e),setInterval:(i=e.setInterval)==null?void 0:i.bind(e),clearInterval:(s=e.clearInterval)==null?void 0:s.bind(e),requestAnimationFrame:(o=e.requestAnimationFrame)==null?void 0:o.bind(e),cancelAnimationFrame:(a=e.cancelAnimationFrame)==null?void 0:a.bind(e),requestIdleCallback:(l=e.requestIdleCallback)==null?void 0:l.bind(e),cancelIdleCallback:(c=e.cancelIdleCallback)==null?void 0:c.bind(e),performance:e.performance,Intl:e.Intl,Date:e.Date,AbortSignal:e.AbortSignal},this.isUnderTest&&(e.builtins=this.builtins)}evaluate(e,t,r,n,...i){const s=i.slice(0,n),o=i.slice(n),a=[];for(let c=0;c<s.length;c++)a[c]=Wt(s[c],o);let l=this.global.eval(r);return e===!0?l=l(...a):e===!1?l=l:typeof l=="function"&&(l=l(...a)),t?this._promiseAwareJsonValueNoThrow(l):l}jsonValue(e,t){if(t!==void 0)return Zs(t,r=>({fallThrough:r}))}_promiseAwareJsonValueNoThrow(e){const t=r=>{try{return this.jsonValue(!0,r)}catch{return}};return e&&typeof e=="object"&&typeof e.then=="function"?(async()=>{const r=await e;return t(r)})():t(e)}},to=class{constructor(e,t){this._testIdAttributeNameForStrictErrorAndConsoleCodegen="data-testid",this._lastAriaSnapshotForTrack=new Map,this.utils={asLocator:oe,cacheNormalizedWhitespaces:li,elementText:q,getAriaRole:I,getElementAccessibleDescription:Vr,getElementAccessibleName:ke,isElementVisible:Q,isInsideScope:St,normalizeWhiteSpace:O,parseAriaSnapshot:Hn,generateAriaTree:qe,findNewElement:Ki,builtins:null},this.window=e,this.document=e.document,this.isUnderTest=t.isUnderTest,this.utils.builtins=new eo(e,t.isUnderTest).builtins,this._sdkLanguage=t.sdkLanguage,this._testIdAttributeNameForStrictErrorAndConsoleCodegen=t.testIdAttributeName,this._evaluator=new ls,this.consoleApi=new zs(this),this.onGlobalListenersRemoved=new Set,this._autoClosingTags=new Set(["AREA","BASE","BR","COL","COMMAND","EMBED","HR","IMG","INPUT","KEYGEN","LINK","MENUITEM","META","PARAM","SOURCE","TRACK","WBR"]),this._booleanAttributes=new Set(["checked","selected","disabled","readonly","multiple"]),this._eventTypes=new Map([["auxclick","mouse"],["click","mouse"],["dblclick","mouse"],["mousedown","mouse"],["mouseeenter","mouse"],["mouseleave","mouse"],["mousemove","mouse"],["mouseout","mouse"],["mouseover","mouse"],["mouseup","mouse"],["mouseleave","mouse"],["mousewheel","mouse"],["keydown","keyboard"],["keyup","keyboard"],["keypress","keyboard"],["textInput","keyboard"],["touchstart","touch"],["touchmove","touch"],["touchend","touch"],["touchcancel","touch"],["pointerover","pointer"],["pointerout","pointer"],["pointerenter","pointer"],["pointerleave","pointer"],["pointerdown","pointer"],["pointerup","pointer"],["pointermove","pointer"],["pointercancel","pointer"],["gotpointercapture","pointer"],["lostpointercapture","pointer"],["focus","focus"],["blur","focus"],["drag","drag"],["dragstart","drag"],["dragend","drag"],["dragover","drag"],["dragenter","drag"],["dragleave","drag"],["dragexit","drag"],["drop","drag"],["wheel","wheel"],["deviceorientation","deviceorientation"],["deviceorientationabsolute","deviceorientation"],["devicemotion","devicemotion"]]),this._hoverHitTargetInterceptorEvents=new Set(["mousemove"]),this._tapHitTargetInterceptorEvents=new Set(["pointerdown","pointerup","touchstart","touchend","touchcancel"]),this._mouseHitTargetInterceptorEvents=new Set(["mousedown","mouseup","pointerdown","pointerup","click","auxclick","dblclick","contextmenu"]),this._allHitTargetInterceptorEvents=new Set([...this._hoverHitTargetInterceptorEvents,...this._tapHitTargetInterceptorEvents,...this._mouseHitTargetInterceptorEvents]),this._engines=new Map,this._engines.set("xpath",Nn),this._engines.set("xpath:light",Nn),this._engines.set("role",gn(!1)),this._engines.set("text",this._createTextEngine(!0,!1)),this._engines.set("text:light",this._createTextEngine(!1,!1)),this._engines.set("id",this._createAttributeEngine("id",!0)),this._engines.set("id:light",this._createAttributeEngine("id",!1)),this._engines.set("data-testid",this._createAttributeEngine("data-testid",!0)),this._engines.set("data-testid:light",this._createAttributeEngine("data-testid",!1)),this._engines.set("data-test-id",this._createAttributeEngine("data-test-id",!0)),this._engines.set("data-test-id:light",this._createAttributeEngine("data-test-id",!1)),this._engines.set("data-test",this._createAttributeEngine("data-test",!0)),this._engines.set("data-test:light",this._createAttributeEngine("data-test",!1)),this._engines.set("css",this._createCSSEngine()),this._engines.set("nth",{queryAll:()=>[]}),this._engines.set("visible",this._createVisibleEngine()),this._engines.set("internal:control",this._createControlEngine()),this._engines.set("internal:has",this._createHasEngine()),this._engines.set("internal:has-not",this._createHasNotEngine()),this._engines.set("internal:and",{queryAll:()=>[]}),this._engines.set("internal:or",{queryAll:()=>[]}),this._engines.set("internal:chain",this._createInternalChainEngine()),this._engines.set("internal:label",this._createInternalLabelEngine()),this._engines.set("internal:text",this._createTextEngine(!0,!0)),this._engines.set("internal:has-text",this._createInternalHasTextEngine()),this._engines.set("internal:has-not-text",this._createInternalHasNotTextEngine()),this._engines.set("internal:attr",this._createNamedAttributeEngine()),this._engines.set("internal:testid",this._createNamedAttributeEngine()),this._engines.set("internal:role",gn(!0)),this._engines.set("internal:describe",this._createDescribeEngine()),this._engines.set("aria-ref",this._createAriaRefEngine());for(const{name:r,source:n}of t.customEngines)this._engines.set(r,this.eval(n));this._stableRafCount=t.stableRafCount,this._browserName=t.browserName,this._isUtilityWorld=!!t.isUtilityWorld,xi({browserNameForWorkarounds:t.browserName}),this._setupGlobalListenersRemovalDetection(),this._setupHitTargetInterceptors(),this.isUnderTest&&(this.window.__injectedScript=this)}eval(e){return this.window.eval(e)}testIdAttributeNameForStrictErrorAndConsoleCodegen(){return this._testIdAttributeNameForStrictErrorAndConsoleCodegen}parseSelector(e){const t=yt(e);return oi(t,r=>{if(!this._engines.has(r.name))throw this.createStacklessError(`Unknown engine "${r.name}" while parsing selector ${e}`)}),t}generateSelector(e,t){return kn(this,e,t)}generateSelectorSimple(e,t){return kn(this,e,{...t,testIdAttributeName:this._testIdAttributeNameForStrictErrorAndConsoleCodegen}).selector}querySelector(e,t,r){const n=this.querySelectorAll(e,t);if(r&&n.length>1)throw this.strictModeViolationError(e,n);return this.checkDeprecatedSelectorUsage(e,n),n[0]}_queryNth(e,t){const r=[...e];let n=+t.body;return n===-1&&(n=r.length-1),new Set(r.slice(n,n+1))}_queryLayoutSelector(e,t,r){const n=t.name,i=t.body,s=[],o=this.querySelectorAll(i.parsed,r);for(const a of e){const l=fn(n,a,o,i.distance);l!==void 0&&s.push({element:a,score:l})}return s.sort((a,l)=>a.score-l.score),new Set(s.map(a=>a.element))}ariaSnapshot(e,t){return this.incrementalAriaSnapshot(e,t).full}incrementalAriaSnapshot(e,t){if(e.nodeType!==Node.ELEMENT_NODE)throw this.createStacklessError("Can only capture aria snapshot of Element nodes.");const r=qe(e,t),n=De(r,t);let i;if(t.track){const s=this._lastAriaSnapshotForTrack.get(t.track);s&&(i=De(r,t,s).text),this._lastAriaSnapshotForTrack.set(t.track,r)}return this._lastAriaSnapshotForQuery=r,{full:n.text,incremental:i,iframeRefs:r.iframeRefs,iframeDepths:n.iframeDepths}}ariaSnapshotForRecorder(){const e=qe(this.document.body,{mode:"ai"}),{text:t}=De(e,{mode:"ai"});return{ariaSnapshot:t,refs:e.refs}}getAllElementsMatchingExpectAriaTemplate(e,t){return Gi(e.documentElement,t)}querySelectorAll(e,t){if(e.capture!==void 0){if(e.parts.some(n=>n.name==="nth"))throw this.createStacklessError("Can\'t query n-th element in a request with the capture.");const r={parts:e.parts.slice(0,e.capture+1)};if(e.capture<e.parts.length-1){const n={parts:e.parts.slice(e.capture+1)},i={name:"internal:has",body:{parsed:n},source:W(n)};r.parts.push(i)}return this.querySelectorAll(r,t)}if(!t.querySelectorAll)throw this.createStacklessError("Node is not queryable.");if(e.capture!==void 0)throw this.createStacklessError("Internal error: there should not be a capture in the selector.");if(t.nodeType===11&&e.parts.length===1&&e.parts[0].name==="css"&&e.parts[0].source===":scope")return[t];this._evaluator.begin();try{let r=new Set([t]);for(const n of e.parts)if(n.name==="nth")r=this._queryNth(r,n);else if(n.name==="internal:and"){const i=this.querySelectorAll(n.body.parsed,t);r=new Set(i.filter(s=>r.has(s)))}else if(n.name==="internal:or"){const i=this.querySelectorAll(n.body.parsed,t);r=new Set(mn(new Set([...r,...i])))}else if(is.includes(n.name))r=this._queryLayoutSelector(r,n,t);else{const i=new Set;for(const s of r){const o=this._queryEngineAll(n,s);for(const a of o)i.add(a)}r=i}return[...r]}finally{this._evaluator.end()}}_queryEngineAll(e,t){const r=this._engines.get(e.name).queryAll(t,e.body);for(const n of r)if(!("nodeName"in n))throw this.createStacklessError(`Expected a Node but got ${Object.prototype.toString.call(n)}`);return r}_createAttributeEngine(e,t){const r=n=>[{simples:[{selector:{css:`[${e}=${JSON.stringify(n)}]`,functions:[]},combinator:""}]}];return{queryAll:(n,i)=>this._evaluator.query({scope:n,pierceShadow:t},r(i))}}_createCSSEngine(){return{queryAll:(e,t)=>this._evaluator.query({scope:e,pierceShadow:!0},t)}}_createTextEngine(e,t){return{queryAll:(n,i)=>{const{matcher:s,kind:o}=ht(i,t),a=[];let l=null;const c=f=>{if(o==="lax"&&l&&l.contains(f))return!1;const u=st(this._evaluator._cacheText,f,s);u==="none"&&(l=f),(u==="self"||u==="selfAndChildren"&&o==="strict"&&!t)&&a.push(f)};n.nodeType===Node.ELEMENT_NODE&&c(n);const h=this._evaluator._queryCSS({scope:n,pierceShadow:e},"*");for(const f of h)c(f);return a}}}_createInternalHasTextEngine(){return{queryAll:(e,t)=>{if(e.nodeType!==1)return[];const r=e,n=q(this._evaluator._cacheText,r),{matcher:i}=ht(t,!0);return i(n)?[r]:[]}}}_createInternalHasNotTextEngine(){return{queryAll:(e,t)=>{if(e.nodeType!==1)return[];const r=e,n=q(this._evaluator._cacheText,r),{matcher:i}=ht(t,!0);return i(n)?[]:[r]}}}_createInternalLabelEngine(){return{queryAll:(e,t)=>{const{matcher:r}=ht(t,!0);return this._evaluator._queryCSS({scope:e,pierceShadow:!0},"*").filter(i=>dn(this._evaluator._cacheText,i).some(s=>r(s)))}}}_createNamedAttributeEngine(){return{queryAll:(t,r)=>{const n=ve(r,!0);if(n.name||n.attributes.length!==1)throw new Error("Malformed attribute selector: "+r);const{name:i,value:s,caseSensitive:o}=n.attributes[0],a=o?null:s.toLowerCase();let l;return s instanceof RegExp?l=h=>!!h.match(s):o?l=h=>h===s:l=h=>h.toLowerCase().includes(a),this._evaluator._queryCSS({scope:t,pierceShadow:!0},`[${i}]`).filter(h=>l(h.getAttribute(i)))}}}_createDescribeEngine(){return{queryAll:t=>t.nodeType!==1?[]:[t]}}_createControlEngine(){return{queryAll(e,t){if(t==="enter-frame")return[];if(t==="return-empty")return[];if(t==="component")return e.nodeType!==1?[]:[e.childElementCount===1?e.firstElementChild:e];throw new Error(`Internal error, unknown internal:control selector ${t}`)}}}_createHasEngine(){return{queryAll:(t,r)=>t.nodeType!==1?[]:!!this.querySelector(r.parsed,t,!1)?[t]:[]}}_createHasNotEngine(){return{queryAll:(t,r)=>t.nodeType!==1?[]:!!this.querySelector(r.parsed,t,!1)?[]:[t]}}_createVisibleEngine(){return{queryAll:(t,r)=>{if(t.nodeType!==1)return[];const n=r==="true";return Q(t)===n?[t]:[]}}}_createInternalChainEngine(){return{queryAll:(t,r)=>this.querySelectorAll(r.parsed,t)}}extend(e,t){const r=this.window.eval(`\n    (() => {\n      const module = {};\n      ${e}\n      return module.exports.default();\n    })()`);return new r(this,t)}async viewportRatio(e){return await new Promise(t=>{const r=new IntersectionObserver(n=>{t(n[0].intersectionRatio),r.disconnect()});r.observe(e),this.utils.builtins.requestAnimationFrame(()=>{})})}getElementBorderWidth(e){if(e.nodeType!==Node.ELEMENT_NODE||!e.ownerDocument||!e.ownerDocument.defaultView)return{left:0,top:0};const t=e.ownerDocument.defaultView.getComputedStyle(e);return{left:parseInt(t.borderLeftWidth||"",10),top:parseInt(t.borderTopWidth||"",10)}}describeIFrameStyle(e){if(!e.ownerDocument||!e.ownerDocument.defaultView)return"error:notconnected";const t=e.ownerDocument.defaultView;for(let n=e;n;n=$(n))if(t.getComputedStyle(n).transform!=="none")return"transformed";const r=t.getComputedStyle(e);return{left:parseInt(r.borderLeftWidth||"",10)+parseInt(r.paddingLeft||"",10),top:parseInt(r.borderTopWidth||"",10)+parseInt(r.paddingTop||"",10)}}retarget(e,t){let r=e.nodeType===Node.ELEMENT_NODE?e:e.parentElement;if(!r)return null;if(t==="none")return r;if(!r.matches("input, textarea, select")&&!r.isContentEditable&&(t==="button-link"?r=r.closest("button, [role=button], a, [role=link]")||r:r=r.closest("button, [role=button], [role=checkbox], [role=radio]")||r),t==="follow-label"&&!r.matches("a, input, textarea, button, select, [role=link], [role=button], [role=checkbox], [role=radio]")&&!r.isContentEditable){const n=r.closest("label");n&&n.control&&(r=n.control)}return r}async checkElementStates(e,t){if(t.includes("stable")){const r=await this._checkElementIsStable(e);if(r===!1)return{missingState:"stable"};if(r==="error:notconnected")return"error:notconnected"}for(const r of t)if(r!=="stable"){const n=this.elementState(e,r);if(n.received==="error:notconnected")return"error:notconnected";if(!n.matches)return{missingState:r}}}async _checkElementIsStable(e){const t=Symbol("continuePolling");let r,n=0,i=0;const s=()=>{const h=this.retarget(e,"no-follow-label");if(!h)return"error:notconnected";const f=this.utils.builtins.performance.now();if(this._stableRafCount>1&&f-i<15)return t;i=f;const u=h.getBoundingClientRect(),p={x:u.top,y:u.left,width:u.width,height:u.height};if(r){if(!(p.x===r.x&&p.y===r.y&&p.width===r.width&&p.height===r.height))return!1;if(++n>=this._stableRafCount)return!0}return r=p,t};let o,a;const l=new Promise((h,f)=>{o=h,a=f}),c=()=>{try{const h=s();h!==t?o(h):this.utils.builtins.requestAnimationFrame(c)}catch(h){a(h)}};return this.utils.builtins.requestAnimationFrame(c),l}_createAriaRefEngine(){return{queryAll:(t,r)=>{var n,i;const s=(i=(n=this._lastAriaSnapshotForQuery)==null?void 0:n.elements)==null?void 0:i.get(r);return s&&s.isConnected?[s]:[]}}}elementState(e,t){const r=this.retarget(e,["visible","hidden"].includes(t)?"none":"follow-label");if(!r||!r.isConnected)return t==="hidden"?{matches:!0,received:"hidden"}:{matches:!1,received:"error:notconnected"};if(t==="visible"||t==="hidden"){const n=Q(r);return{matches:t==="visible"?n:!n,received:n?"visible":"hidden"}}if(t==="disabled"||t==="enabled"){const n=et(r);return{matches:t==="disabled"?n:!n,received:n?"disabled":"enabled"}}if(t==="editable"){const n=et(r),i=Pi(r);if(i==="error")throw this.createStacklessError("Element is not an <input>, <textarea>, <select> or [contenteditable] and does not have a role allowing [aria-readonly]");return{matches:!n&&!i,received:n?"disabled":i?"readOnly":"editable"}}if(t==="checked"||t==="unchecked"){const n=t==="checked",i=Ri(r);if(i==="error")throw this.createStacklessError("Not a checkbox or radio button");const s=r.nodeName==="INPUT"&&r.type==="radio";return{matches:n===i,received:i?"checked":"unchecked",isRadio:s}}if(t==="indeterminate"){const n=Li(r);if(n==="error")throw this.createStacklessError("Not a checkbox or radio button");return{matches:n==="mixed",received:n===!0?"checked":n===!1?"unchecked":"mixed"}}throw this.createStacklessError(`Unexpected element state "${t}"`)}selectOptions(e,t){const r=this.retarget(e,"follow-label");if(!r)return"error:notconnected";if(r.nodeName.toLowerCase()!=="select")throw this.createStacklessError("Element is not a <select> element");const n=r,i=[...n.options],s=[];let o=t.slice();for(let a=0;a<i.length;a++){const l=i[a],c=h=>{if(h instanceof Node)return l===h;let f=!0;return h.valueOrLabel!==void 0&&(f=f&&(h.valueOrLabel===l.value||h.valueOrLabel===l.label)),h.value!==void 0&&(f=f&&h.value===l.value),h.label!==void 0&&(f=f&&h.label===l.label),h.index!==void 0&&(f=f&&h.index===a),f};if(o.some(c)){if(!this.elementState(l,"enabled").matches)return"error:optionnotenabled";if(s.push(l),n.multiple)o=o.filter(h=>!c(h));else{o=[];break}}}return o.length?"error:optionsnotfound":(n.value=void 0,s.forEach(a=>a.selected=!0),n.dispatchEvent(new Event("input",{bubbles:!0,composed:!0})),n.dispatchEvent(new Event("change",{bubbles:!0})),s.map(a=>a.value))}fill(e,t){const r=this.retarget(e,"follow-label");if(!r)return"error:notconnected";if(r.nodeName.toLowerCase()==="input"){const n=r,i=n.type.toLowerCase(),s=new Set(["color","date","time","datetime-local","month","range","week"]);if(!new Set(["","email","number","password","search","tel","text","url"]).has(i)&&!s.has(i))throw this.createStacklessError(`Input of type "${i}" cannot be filled`);if(i==="number"&&(t=t.trim(),isNaN(Number(t))))throw this.createStacklessError("Cannot type text into input[type=number]");if(i==="color"&&(t=t.toLowerCase()),s.has(i)){if(t=t.trim(),n.focus(),n.value=t,n.value!==t)throw this.createStacklessError("Malformed value");return r.dispatchEvent(new Event("input",{bubbles:!0,composed:!0})),r.dispatchEvent(new Event("change",{bubbles:!0})),"done"}}else if(r.nodeName.toLowerCase()!=="textarea"){if(!r.isContentEditable)throw this.createStacklessError("Element is not an <input>, <textarea> or [contenteditable] element")}return this.selectText(r),"needsinput"}selectText(e){const t=this.retarget(e,"follow-label");if(!t)return"error:notconnected";if(t.nodeName.toLowerCase()==="input"){const i=t;return i.select(),i.focus(),"done"}if(t.nodeName.toLowerCase()==="textarea"){const i=t;return i.selectionStart=0,i.selectionEnd=i.value.length,i.focus(),"done"}t.focus();const r=t.ownerDocument.createRange();r.selectNodeContents(t);const n=t.ownerDocument.defaultView.getSelection();return n&&(n.removeAllRanges(),n.addRange(r)),"done"}_activelyFocused(e){const t=e.getRootNode().activeElement,r=t===e&&!!e.ownerDocument&&e.ownerDocument.hasFocus();return{activeElement:t,isFocused:r}}focusNode(e,t){if(!e.isConnected)return"error:notconnected";if(e.nodeType!==Node.ELEMENT_NODE)throw this.createStacklessError("Node is not an element");const{activeElement:r,isFocused:n}=this._activelyFocused(e);if(e.isContentEditable&&!n&&r&&r.blur&&r.blur(),e.focus(),e.focus(),t&&!n&&e.nodeName.toLowerCase()==="input")try{e.setSelectionRange(0,0)}catch{}return"done"}blurNode(e){if(!e.isConnected)return"error:notconnected";if(e.nodeType!==Node.ELEMENT_NODE)throw this.createStacklessError("Node is not an element");return e.blur(),"done"}setInputFiles(e,t){if(e.nodeType!==Node.ELEMENT_NODE)return"Node is not of type HTMLElement";const r=e;if(r.nodeName!=="INPUT")return"Not an <input> element";const n=r;if((n.getAttribute("type")||"").toLowerCase()!=="file")return"Not an input[type=file] element";const s=t.map(a=>{const l=Uint8Array.from(atob(a.buffer),c=>c.charCodeAt(0));return new File([l],a.name,{type:a.mimeType,lastModified:a.lastModifiedMs})}),o=new DataTransfer;for(const a of s)o.items.add(a);n.files=o.files,n.dispatchEvent(new Event("input",{bubbles:!0,composed:!0})),n.dispatchEvent(new Event("change",{bubbles:!0}))}expectHitTarget(e,t){var r;const n=[];let i=t;for(;i;){const h=Ar(i);if(!h||(n.push(h),h.nodeType===9))break;i=h.host}let s;for(let h=n.length-1;h>=0;h--){const f=n[h],u=f.elementsFromPoint(e.x,e.y),p=f.elementFromPoint(e.x,e.y);if(p&&u[0]&&$(p)===u[0]){const g=this.window.getComputedStyle(p);g?.display==="contents"&&u.unshift(p)}u[0]&&u[0].shadowRoot===f&&u[1]===p&&u.shift();const d=u[0];if(!d||(s=d,h&&d!==n[h-1].host))break}const o=[];for(;s&&s!==t;)o.push(s),s=(r=s.assignedSlot)!=null?r:$(s);if(s===t)return"done";const a=this.previewNode(o[0]||this.document.documentElement);let l,c=t;for(;c;){const h=o.indexOf(c);if(h!==-1){h>1&&(l=this.previewNode(o[h-1]));break}c=$(c)}return l?{hitTargetDescription:`${a} from ${l} subtree`}:{hitTargetDescription:a}}setupHitTargetInterceptor(e,t,r,n){const i=this.retarget(e,"button-link");if(!i||!i.isConnected)return"error:notconnected";if(r){const c=this.expectHitTarget(r,i);if(c!=="done")return c.hitTargetDescription}if(t==="drag")return{stop:()=>"done"};const s={hover:this._hoverHitTargetInterceptorEvents,tap:this._tapHitTargetInterceptorEvents,mouse:this._mouseHitTargetInterceptorEvents}[t];let o;const a=c=>{if(!s.has(c.type)||!c.isTrusted)return;const h=this.window.TouchEvent&&c instanceof this.window.TouchEvent?c.touches[0]:c;o===void 0&&h&&(o=this.expectHitTarget({x:h.clientX,y:h.clientY},i)),(n||o!=="done"&&o!==void 0)&&(c.preventDefault(),c.stopPropagation(),c.stopImmediatePropagation())},l=()=>(this._hitTargetInterceptor===a&&(this._hitTargetInterceptor=void 0),o||"done");return this._hitTargetInterceptor=a,{stop:l}}dispatchEvent(e,t,r){var n,i,s,o,a;let l;const c={bubbles:!0,cancelable:!0,composed:!0,...r};switch(this._eventTypes.get(t)){case"mouse":l=new MouseEvent(t,c);break;case"keyboard":l=new KeyboardEvent(t,c);break;case"touch":{if(this._browserName==="webkit"){const h=u=>{var p,d,g;if(u instanceof Touch)return u;let m=u.pageX;m===void 0&&u.clientX!==void 0&&(m=u.clientX+(((p=this.document.scrollingElement)==null?void 0:p.scrollLeft)||0));let w=u.pageY;return w===void 0&&u.clientY!==void 0&&(w=u.clientY+(((d=this.document.scrollingElement)==null?void 0:d.scrollTop)||0)),this.document.createTouch(this.window,(g=u.target)!=null?g:e,u.identifier,m,w,u.screenX,u.screenY,u.radiusX,u.radiusY,u.rotationAngle,u.force)},f=u=>u instanceof TouchList||!u?u:this.document.createTouchList(...u.map(h));(n=c.target)!=null||(c.target=e),c.touches=f(c.touches),c.targetTouches=f(c.targetTouches),c.changedTouches=f(c.changedTouches),l=new TouchEvent(t,c)}else(i=c.target)!=null||(c.target=e),c.touches=(s=c.touches)==null?void 0:s.map(h=>{var f;return h instanceof Touch?h:new Touch({...h,target:(f=h.target)!=null?f:e})}),c.targetTouches=(o=c.targetTouches)==null?void 0:o.map(h=>{var f;return h instanceof Touch?h:new Touch({...h,target:(f=h.target)!=null?f:e})}),c.changedTouches=(a=c.changedTouches)==null?void 0:a.map(h=>{var f;return h instanceof Touch?h:new Touch({...h,target:(f=h.target)!=null?f:e})}),l=new TouchEvent(t,c);break}case"pointer":l=new PointerEvent(t,c);break;case"focus":l=new FocusEvent(t,c);break;case"drag":l=new DragEvent(t,c);break;case"wheel":l=new WheelEvent(t,c);break;case"deviceorientation":try{l=new DeviceOrientationEvent(t,c)}catch{const{bubbles:h,cancelable:f,alpha:u,beta:p,gamma:d,absolute:g}=c;l=this.document.createEvent("DeviceOrientationEvent"),l.initDeviceOrientationEvent(t,h,f,u,p,d,g)}break;case"devicemotion":try{l=new DeviceMotionEvent(t,c)}catch{const{bubbles:h,cancelable:f,acceleration:u,accelerationIncludingGravity:p,rotationRate:d,interval:g}=c;l=this.document.createEvent("DeviceMotionEvent"),l.initDeviceMotionEvent(t,h,f,u,p,d,g)}break;default:l=new Event(t,c);break}e.dispatchEvent(l)}previewNode(e){if(e.nodeType===Node.TEXT_NODE)return ut(`#text=${e.nodeValue||""}`);if(e.nodeType!==Node.ELEMENT_NODE)return ut(`<${e.nodeName.toLowerCase()} />`);const t=e,r=[];for(let a=0;a<t.attributes.length;a++){const{name:l,value:c}=t.attributes[a];l!=="style"&&(!c&&this._booleanAttributes.has(l)?r.push(` ${l}`):r.push(` ${l}="${c}"`))}r.sort((a,l)=>a.length-l.length);const n=Sr(r.join(""),500);if(this._autoClosingTags.has(t.nodeName))return ut(`<${t.nodeName.toLowerCase()}${n}/>`);const i=t.childNodes;let s=!1;if(i.length<=5){s=!0;for(let a=0;a<i.length;a++)s=s&&i[a].nodeType===Node.TEXT_NODE}const o=s?t.textContent||"":i.length?"\\u2026":"";return ut(`<${t.nodeName.toLowerCase()}${n}>${Sr(o,50)}</${t.nodeName.toLowerCase()}>`)}_generateSelectors(e){this._evaluator.begin(),tt(),_t();try{const t=this._isUtilityWorld&&this._browserName==="firefox"?2:10;return e.slice(0,t).map(n=>({preview:this.previewNode(n),selector:this.generateSelectorSimple(n)})).map((n,i)=>`${i+1}) ${n.preview} aka ${oe(this._sdkLanguage,n.selector)}`)}finally{Tt(),rt(),this._evaluator.end()}}strictModeViolationError(e,t){const r=this._generateSelectors(t).map(n=>`\n    `+n);return r.length<t.length&&r.push(`\n    ...`),this.createStacklessError(`strict mode violation: ${oe(this._sdkLanguage,W(e))} resolved to ${t.length} elements:${r.join("")}\n`)}checkDeprecatedSelectorUsage(e,t){const r=new Set(["_react","_vue","xpath:light","text:light","id:light","data-testid:light","data-test-id:light","data-test:light"]);if(!t.length)return;const n=e.parts.find(s=>r.has(s.name));if(!n)return;const i=this._generateSelectors(t).map(s=>`\n    `+s);throw i.length<t.length&&i.push(`\n    ...`),this.createStacklessError(`"${n.name}" selector is not supported: ${oe(this._sdkLanguage,W(e))} resolved to ${t.length} element${t.length===1?"":"s"}:${i.join("")}\n`)}createStacklessError(e){if(this._browserName==="firefox"){const r=new Error("Error: "+e);return r.stack="",r}const t=new Error(e);return delete t.stack,t}createHighlight(){return new it(this)}maskSelectors(e,t){const r=this._createHighlight(),n=[];for(const i of e)n.push(this.querySelectorAll(i,this.document.documentElement));r.maskElements(n.flat(),t)}_createHighlight(){return this._highlight&&this.hideHighlight(),this._highlight=new it(this),this._highlight.install(),this._highlight}_ensureHighlight(){return this._highlight||(this._highlight=new it(this),this._highlight.install()),this._highlight}highlight(e){this._highlight||(this._highlight=new it(this),this._highlight.install()),this._highlight.runHighlightOnRaf(e)}setScreencastAnnotation(e){var t;const r=this._ensureHighlight();if(!e){r.updateHighlight([]),r.hideActionPoint(),r.hideActionTitle();return}const n=(t=e.duration)!=null?t:500;e.box&&r.updateHighlight([{box:e.box,color:"rgba(0, 128, 255, 0.15)",borderColor:"rgba(0, 128, 255, 0.6)",fadeDuration:n}]),e.point&&r.showActionPoint(e.point.x,e.point.y,n),e.actionTitle&&r.showActionTitle(e.actionTitle,n,e.position,e.fontSize)}addUserOverlay(e,t){this._ensureHighlight().addUserOverlay(e,t)}getUserOverlay(e){return this._ensureHighlight().getUserOverlay(e)}removeUserOverlay(e){this._ensureHighlight().removeUserOverlay(e)}setUserOverlaysVisible(e){this._ensureHighlight().setUserOverlaysVisible(e)}hideHighlight(){this._highlight&&(this._highlight.uninstall(),delete this._highlight)}markTargetElements(e,t){var r,n;((r=this._markedElements)==null?void 0:r.callId)!==t&&(this._markedElements=void 0);const i=((n=this._markedElements)==null?void 0:n.elements)||new Set,s=new CustomEvent("__playwright_unmark_target__",{bubbles:!0,cancelable:!0,detail:t,composed:!0});for(const a of i)e.has(a)||a.dispatchEvent(s);const o=new CustomEvent("__playwright_mark_target__",{bubbles:!0,cancelable:!0,detail:t,composed:!0});for(const a of e)i.has(a)||a.dispatchEvent(o);this._markedElements={callId:t,elements:e}}_setupGlobalListenersRemovalDetection(){const e="__playwright_global_listeners_check__";let t=!1;const r=()=>t=!0;this.window.addEventListener(e,r),new MutationObserver(n=>{if(n.some(s=>Array.from(s.addedNodes).includes(this.document.documentElement))&&(t=!1,this.window.dispatchEvent(new CustomEvent(e)),!t)){this.window.addEventListener(e,r);for(const s of this.onGlobalListenersRemoved)s()}}).observe(this.document,{childList:!0})}_setupHitTargetInterceptors(){const e=r=>{var n;return(n=this._hitTargetInterceptor)==null?void 0:n.call(this,r)},t=()=>{for(const r of this._allHitTargetInterceptorEvents)this.window.addEventListener(r,e,{capture:!0,passive:!1})};t(),this.onGlobalListenersRemoved.add(t)}async expect(e,t,r){var n,i;if(t.expression==="to.have.count"||t.expression.endsWith(".array"))return this.expectArray(r,t);if(!e){if(!t.isNot&&t.expression==="to.be.hidden")return{matches:!0};if(t.isNot&&t.expression==="to.be.visible")return{matches:!1};if(!t.isNot&&t.expression==="to.be.detached")return{matches:!0};if(t.isNot&&t.expression==="to.be.attached")return{matches:!1};if(t.isNot&&t.expression==="to.be.in.viewport")return{matches:!1};if(t.expression==="to.have.title"&&((n=t?.expectedText)!=null&&n[0])){const o=new pe(t.expectedText[0]),a=this.document.title;return{received:a,matches:o.matches(a)}}if(t.expression==="to.have.url"&&((i=t?.expectedText)!=null&&i[0])){const o=new pe(t.expectedText[0]),a=this.document.location.href;return{received:a,matches:o.matches(a)}}return{matches:t.isNot,missingReceived:!0}}return await this.expectSingleElement(e,t)}async expectSingleElement(e,t){var r,n;const i=t.expression;{let s;if(i==="to.have.attribute"){const o=e.hasAttribute(t.expressionArg);s={matches:o,received:o?"attribute present":"attribute not present"}}else if(i==="to.be.checked"){const{checked:o,indeterminate:a}=t.expectedValue;if(a){if(o!==void 0)throw this.createStacklessError("Can\'t assert indeterminate and checked at the same time");s=this.elementState(e,"indeterminate")}else s=this.elementState(e,o===!1?"unchecked":"checked")}else if(i==="to.be.disabled")s=this.elementState(e,"disabled");else if(i==="to.be.editable")s=this.elementState(e,"editable");else if(i==="to.be.readonly")s=this.elementState(e,"editable"),s.matches=!s.matches;else if(i==="to.be.empty")if(e.nodeName==="INPUT"||e.nodeName==="TEXTAREA"){const o=e.value;s={matches:!o,received:o?"notEmpty":"empty"}}else{const o=(r=e.textContent)==null?void 0:r.trim();s={matches:!o,received:o?"notEmpty":"empty"}}else if(i==="to.be.enabled")s=this.elementState(e,"enabled");else if(i==="to.be.focused"){const o=this._activelyFocused(e).isFocused;s={matches:o,received:o?"focused":"inactive"}}else i==="to.be.hidden"?s=this.elementState(e,"hidden"):i==="to.be.visible"?s=this.elementState(e,"visible"):i==="to.be.attached"?s={matches:!0,received:"attached"}:i==="to.be.detached"&&(s={matches:!1,received:"attached"});if(s){if(s.received==="error:notconnected")throw this.createStacklessError("Element is not connected");return s}}if(i==="to.have.property"){let s=e;const o=t.expressionArg.split(".");for(let c=0;c<o.length-1;c++){if(typeof s!="object"||!(o[c]in s))return{received:void 0,matches:!1};s=s[o[c]]}const a=s[o[o.length-1]],l=zt(a,t.expectedValue);return{received:a,matches:l}}if(i==="to.be.in.viewport"){const s=await this.viewportRatio(e);return{received:`viewport ratio ${s}`,matches:s>0&&s>((n=t.expectedNumber)!=null?n:0)-1e-9}}if(i==="to.have.values"){if(e=this.retarget(e,"follow-label"),e.nodeName!=="SELECT"||!e.multiple)throw this.createStacklessError("Not a select element with a multiple attribute");const s=[...e.selectedOptions].map(o=>o.value);return s.length!==t.expectedText.length?{received:s,matches:!1}:{received:s,matches:s.map((o,a)=>new pe(t.expectedText[a]).matches(o)).every(Boolean)}}if(i==="to.match.aria"){const s=zi(e,t.expectedValue);return{received:s.received,matches:!!s.matches.length}}{let s;if(i==="to.have.attribute.value"){const o=e.getAttribute(t.expressionArg);if(o===null)return{received:null,matches:!1};s=o}else if(["to.have.class","to.contain.class"].includes(i)){if(!t.expectedText)throw this.createStacklessError("Expected text is not provided for "+i);return{received:e.classList.toString(),matches:new pe(t.expectedText[0]).matchesClassList(this,e.classList,i==="to.contain.class")}}else if(i==="to.have.css")s=this.window.getComputedStyle(e).getPropertyValue(t.expressionArg);else if(i==="to.have.id")s=e.id;else if(i==="to.have.text")s=t.useInnerText?e.innerText:q(new Map,e).full;else if(i==="to.have.accessible.name")s=ke(e,!1);else if(i==="to.have.accessible.description")s=Vr(e,!1);else if(i==="to.have.accessible.error.message")s=Ii(e);else if(i==="to.have.role")s=I(e)||"";else if(i==="to.have.value"){if(e=this.retarget(e,"follow-label"),e.nodeName!=="INPUT"&&e.nodeName!=="TEXTAREA"&&e.nodeName!=="SELECT")throw this.createStacklessError("Not an input element");s=e.value}if(s!==void 0&&t.expectedText){const o=new pe(t.expectedText[0]);return{received:s,matches:o.matches(s)}}}throw this.createStacklessError("Unknown expect matcher: "+i)}expectArray(e,t){const r=t.expression;if(r==="to.have.count"){const a=e.length,l=a===t.expectedNumber;return{received:a,matches:l}}if(!t.expectedText)throw this.createStacklessError("Expected text is not provided for "+r);if(["to.have.class.array","to.contain.class.array"].includes(r)){const a=e.map(h=>h.classList),l=a.map(String);if(a.length!==t.expectedText.length)return{received:l,matches:!1};const c=this._matchSequentially(t.expectedText,a,(h,f)=>h.matchesClassList(this,f,r==="to.contain.class.array"));return{received:l,matches:c}}if(!["to.contain.text.array","to.have.text.array"].includes(r))throw this.createStacklessError("Unknown expect matcher: "+r);const n=e.map(a=>t.useInnerText?a.innerText:q(new Map,a).full),i=r!=="to.contain.text.array";if(!(n.length===t.expectedText.length||!i))return{received:n,matches:!1};const o=this._matchSequentially(t.expectedText,n,(a,l)=>a.matches(l));return{received:n,matches:o}}_matchSequentially(e,t,r){const n=e.map(o=>new pe(o));let i=0,s=0;for(;i<n.length&&s<t.length;)r(n[i],t[s])&&++i,++s;return i===n.length}};function ut(e){return e.replace(/\\n/g,"\\u21B5").replace(/\\t/g,"\\u21C6")}function ro(e){if(e=e.substring(1,e.length-1),!e.includes("\\\\"))return e;const t=[];let r=0;for(;r<e.length;)e[r]==="\\\\"&&r+1<e.length&&r++,t.push(e[r++]);return t.join("")}function ht(e,t){if(e[0]==="/"&&e.lastIndexOf("/")>0){const i=e.lastIndexOf("/"),s=new RegExp(e.substring(1,i),e.substring(i+1));return{matcher:o=>s.test(o.full),kind:"regex"}}const r=t?JSON.parse.bind(JSON):ro;let n=!1;return e.length>1&&e[0]===\'"\'&&e[e.length-1]===\'"\'?(e=r(e),n=!0):t&&e.length>1&&e[0]===\'"\'&&e[e.length-2]===\'"\'&&e[e.length-1]==="i"?(e=r(e.substring(0,e.length-1)),n=!1):t&&e.length>1&&e[0]===\'"\'&&e[e.length-2]===\'"\'&&e[e.length-1]==="s"?(e=r(e.substring(0,e.length-1)),n=!0):e.length>1&&e[0]==="\'"&&e[e.length-1]==="\'"&&(e=r(e),n=!0),e=O(e),n?t?{kind:"strict",matcher:s=>s.normalized===e}:{matcher:s=>!e&&!s.immediate.length?!0:s.immediate.some(o=>O(o)===e),kind:"strict"}:(e=e.toLowerCase(),{kind:"lax",matcher:i=>i.normalized.toLowerCase().includes(e)})}var pe=class{constructor(e){if(this._normalizeWhiteSpace=e.normalizeWhiteSpace,this._ignoreCase=e.ignoreCase,this._string=e.matchSubstring?void 0:this.normalize(e.string),this._substring=e.matchSubstring?this.normalize(e.string):void 0,e.regexSource){const t=new Set((e.regexFlags||"").split(""));e.ignoreCase===!1&&t.delete("i"),e.ignoreCase===!0&&t.add("i"),this._regex=new RegExp(e.regexSource,[...t].join(""))}}matches(e){return this._regex||(e=this.normalize(e)),this._string!==void 0?e===this._string:this._substring!==void 0?e.includes(this._substring):this._regex?!!this._regex.test(e):!1}matchesClassList(e,t,r){if(r){if(this._regex)throw e.createStacklessError("Partial matching does not support regular expressions. Please provide a string value.");return this._string.split(/\\s+/g).filter(Boolean).every(n=>t.contains(n))}return this.matches(t.toString())}normalize(e){return e&&(this._normalizeWhiteSpace&&(e=O(e)),this._ignoreCase&&(e=e.toLocaleLowerCase()),e)}};function zt(e,t){if(e===t)return!0;if(e&&t&&typeof e=="object"&&typeof t=="object"){if(e.constructor!==t.constructor)return!1;if(Array.isArray(e)){if(e.length!==t.length)return!1;for(let n=0;n<e.length;++n)if(!zt(e[n],t[n]))return!1;return!0}if(e instanceof RegExp)return e.source===t.source&&e.flags===t.flags;if(e.valueOf!==Object.prototype.valueOf)return e.valueOf()===t.valueOf();if(e.toString!==Object.prototype.toString)return e.toString()===t.toString();const r=Object.keys(e);if(r.length!==Object.keys(t).length)return!1;for(let n=0;n<r.length;++n)if(!t.hasOwnProperty(r[n]))return!1;for(const n of r)if(!zt(e[n],t[n]))return!1;return!0}return typeof e=="number"&&typeof t=="number"?isNaN(e)&&isNaN(t):!1}})(),{...dt.exports,InjectedScript:dt.exports.InjectedScript()}})();\n';

const fK = `(() => {
  if (!window.__codexPlaywrightInjected) {
    ${nD}
    window.__codexPlaywrightInjected = new PlaywrightInjected.InjectedScript(window, {
      isUnderTest: false,
      sdkLanguage: "javascript",
      testIdAttributeName: "data-testid",
      stableRafCount: 1,
      browserName: "chromium",
      customEngines: [],
    });
  }
  window.__codexPlaywrightInjected.isProtectedCredentialField = (element) =>
    (${ai.toString()})(element, ${JSON.stringify(on)});
})()`;

const oD = "playwright-injected";

const gm = { awaitPromise: !0, expression: fK, returnByValue: !0 };

function iD(e) { return e === "right" || e === "middle" ? e : "left"; }

const Tw = { block: "center", inline: "nearest" };

const hK = 50;

const gK = "octopus.browser.actions";

const sD = 100;

const vw = "Frame target is not available for this iframe selector path";

const Ew = "Browser Use Playwright injected helper is missing";

const pD = "Playwright selector deadline exceeded";

const bK = [Tw, { block: "end", inline: "end" }, { block: "start", inline: "start" }];

const wm = class e {
    constructor(t, r, n, o) { this.cdp = t; this.cua = r; this.performanceSpan = n; this.commandTiming = o; t.addListener("tabDetached", i => { for (let s of this.playwrightInjectedTargets)
        (s === String(i) || s.startsWith(`${i}:`)) && this.playwrightInjectedTargets.delete(s); for (let s of this.playwrightFrameWorlds.keys())
        (s === String(i) || s.startsWith(`${i}:`)) && this.playwrightFrameWorlds.delete(s); }), t.addListener("event", i => { i.method === "Runtime.executionContextsCleared" || i.method === "Page.frameNavigated" && i.params.frame.parentId == null ? this.clearPlaywrightStateForTarget(i.source) : i.method === "Page.frameNavigated" ? this.deletePlaywrightFrameWorld(i.source, i.params.frame.id) : i.method === "Page.frameDetached" ? this.deletePlaywrightFrameWorld(i.source, i.params.frameId) : i.method === "Target.attachedToTarget" && this.clearPlaywrightFrameWorldsForTarget(i.source); }); }
    cdp;
    cua;
    performanceSpan;
    commandTiming;
    playwrightFrameWorlds = new Map;
    playwrightInjectedTargets = new Set;
    boundSelector;
    static injectedConstant = "__codexPlaywrightInjected";
    async resolvePlaywrightSelector(t, r) { if (!Number.isFinite(t))
        throw new Error("resolvePlaywrightSelector requires numeric tab_id"); return (await this.resolvePlaywrightSelectorNode(t, r, { retry: !1 })).backendNodeId; }
    async resolvePlaywrightSelectorNode(t, r, n = {}) { let o = Me({ timeout_ms: n.timeoutMs }), i = Date.now(), s = xw(n, i, o); for (;;) {
        je(s);
        try {
            let a = { deadlineMs: s.deadlineMs, timeoutMs: o, telemetryAttrs: Bn({ operation: this.performanceSpan.currentPlaywrightOperation("locator"), phase: "selector_node" }) }, l = await this.resolveSelectorTarget(bm(t), r, a);
            return { ...await this.nodeForResolvedSelectorTarget(l, a), oopifFrameChain: l.oopifFrameChain, target: l.target };
        }
        catch (a) {
            if (_m(a) || s.retry === !1)
                throw a;
            if (Date.now() - i >= o) {
                let l = a instanceof Error ? a.message : String(a);
                throw new Error(`Timed out after ${o}ms waiting for selector ${r}: ${l}`, { cause: a });
            }
            je(s), await new Promise(l => setTimeout(l, mD(s)));
        }
    } }
    async withBoundPlaywrightSelector(t, r, n, o, i = {}) { let s = await this.resolvePlaywrightSelectorNode(t, r, { retry: !1, timeoutMs: i.timeoutMs }); return await this.withPlaywrightNode(s, async (a, l) => { let c = { executionContextId: l, objectId: a, oopifFrameChain: s.oopifFrameChain, selector: r, target: s.target }; if (!await n())
        return; let p = Object.create(this, { boundSelector: { value: c } }); return await o(p); }, i); }
    async evaluateOnPlaywrightNode(t, r) {
        return await this.withPlaywrightNode(t, async (n) => {
            let o = await this.callCdpTarget(t.target, "Runtime.callFunctionOn", { awaitPromise: !0, functionDeclaration: `function () {
            const __name = (target) => target;
            const injected = window.${e.injectedConstant};
            if (!this.isConnected) throw new Error("Element is not connected");
            return (${r.toString()})(this, injected);
          }`, objectId: n, returnByValue: !0, userGesture: !0 });
            try {
                return Mn(o);
            }
            finally {
                await this.releaseRuntimeObjects(t.target, [o.exceptionDetails?.exception?.objectId], {});
            }
        });
    }
    async prepareReadonlyLocatorAll(t, r, n, o = {}) {
        let i = Me({ timeout_ms: o.timeoutMs }), s = await this.resolveSelectorTarget(bm(t), r, { deadlineMs: o.deadlineMs, timeoutMs: i, telemetryAttrs: Bn({ operation: this.performanceSpan.currentPlaywrightOperation("locator"), phase: "selector_scope" }) });
        return { expression: `(() => {
        ${Ss}
        ${e.selectorScopeFunctions()}
        const injected = globalThis.__codexPlaywrightInjected;
        const parsed = injected.parseSelector(${JSON.stringify(s.selector)});
        const selectorScope = selectorScopeFor(injected, parsed);
        const elements = selectorScope
          ? selectorScope.injected.querySelectorAll(
              selectorScope.parsed,
              selectorScope.root,
            )
          : [];
        return ${n};
      })()`, frameId: s.frameId, target: s.target };
    }
    async evaluateOnPlaywrightSelector(t, r, n, o = {}) { return (await this.evaluateOnPlaywrightSelectorWithTarget(t, r, n, o)).result; }
    async evaluateOnPlaywrightSelectorWithTarget(t, r, n, o = {}) {
        let i = this.boundSelector;
        if (i != null && i.selector === r && i.target.tabId === Number(t)) {
            if (o.strict === !0)
                throw new Error("Strict evaluation cannot re-resolve a bound selector");
            let m = await this.callCdpTarget(i.target, "Runtime.callFunctionOn", { arguments: o.arg == null ? [] : [{ value: o.arg }], awaitPromise: !0, functionDeclaration: `async function (arg) {
            const injected = window.${e.injectedConstant};
            if (!injected) throw new Error(${JSON.stringify(Ew)});
            if (!this.isConnected) throw new Error("Element is not connected");
            ${e.selectorScopeFunctions()}
            const frameChain = [];
            for (
              let frameWindow = this.ownerDocument?.defaultView;
              frameWindow?.frameElement;
              frameWindow = frameWindow.frameElement.ownerDocument?.defaultView
            ) {
              const frameElement = frameWindow.frameElement;
              frameChain.unshift({
                element: frameElement,
                injected: injectedForWindow(injected, frameElement.ownerDocument?.defaultView),
              });
            }
            return await (${n.toString()})(this, injected, arg, {
              frameChain,
              prepareFrameChainForPointerAction: (point, scrollAlignment) =>
                prepareFrameChainForPointerAction(frameChain, point, scrollAlignment),
            });
          }`, objectId: i.objectId, returnByValue: !0 }, { timeoutMs: o.timeoutMs });
            return { executionContextId: i.executionContextId, result: Mn(m), oopifFrameChain: i.oopifFrameChain, target: i.target };
        }
        let s = Me({ timeout_ms: o.timeoutMs }), a = Date.now(), l = n.toString(), c = xw(o, a, s), p = bm(t);
        if (o.isolatedWorld === !0) {
            let { frameTree: m } = await this.callCdpTarget(p.target, "Page.getFrameTree", void 0, c);
            if (!m.frame.id)
                throw new Error(vw);
            p.frameId = m.frame.id;
        }
        return await aD(r, () => this.evaluateSingleSelectorWithTarget(p, r, l, c.arg, { ...c.includeFrameIdentity == null ? {} : { includeFrameIdentity: c.includeFrameIdentity }, ...c.isolatedWorld === !0 ? { isolatedWorld: !0 } : {}, ...c.strict === !0 ? { strict: !0 } : {}, deadlineMs: c.deadlineMs, timeoutMs: s, telemetryAttrs: Bn({ operation: this.performanceSpan.currentPlaywrightOperation("locator"), phase: "selector_eval" }) }), { deadlineMs: c.deadlineMs, retry: c.retry, startedAt: a, timeoutMs: s }, this.commandTiming);
    }
    async evaluateOnPlaywrightSelectorAll(t, r, n, o = {}) { let i = Me({ timeout_ms: o.timeoutMs }), s = Date.now(), a = n.toString(), l = xw(o, s, i); return await aD(r, () => this.evaluateSelectorAllInTarget(bm(t), r, a, l.arg, { deadlineMs: l.deadlineMs, timeoutMs: i, telemetryAttrs: Bn({ operation: this.performanceSpan.currentPlaywrightOperation("locator"), phase: "selector_all_eval" }) }), { deadlineMs: l.deadlineMs, retry: l.retry, startedAt: s, timeoutMs: i }, this.commandTiming); }
    async evaluateOnPlaywrightPage(t, r, n = {}) {
        let o = r.toString(), s = { tabId: H(t) }, a = { telemetryAttrs: Bn({ operation: this.performanceSpan.currentPlaywrightOperation("page"), phase: "page_eval" }), timeoutMs: n.timeoutMs }, l;
        if (n.isolatedWorld === !0) {
            let { frameTree: c } = await this.callCdpTarget(s, "Page.getFrameTree", void 0, a);
            if (!c.frame.id)
                throw new Error(vw);
            l = c.frame.id;
        }
        return await this.evaluateWithPlaywrightInjectedInTarget(s, `(async () => {
          const injected = window.${e.injectedConstant};
          return await (${o})(injected, ${Sw(n.arg)});
        })()`, a, l);
    }
    async selectorsResolveToDistinctElements(t, r, n = {}) {
        return r.length < 2 ? !0 : await this.evaluateWithPlaywrightInjected(t, `(() => {
        ${Ss}
        const injected = window.${e.injectedConstant};
        const elements = ${JSON.stringify(r)}.map((selector) => {
          const parsed = injected.parseSelector(selector);
          return querySelectorStrictWithVisibleFallback(injected, parsed, document);
        });
        return elements.every(
          (element, index) => element != null && elements.indexOf(element) === index
        );
      })()`, { telemetryAttrs: Bn({ operation: this.performanceSpan.currentPlaywrightOperation("locator"), phase: "selector_identity_eval" }), timeoutMs: n.timeoutMs });
    }
    async clickLocator(t, r) { let n = H(t.tab_id), o = Me(t), i = iD("button" in t ? t.button : void 0), s = to("modifiers" in t ? t.modifiers : void 0, this.cdp.platform), a = "force" in t ? t.force === !0 : !1, l = a ? [] : ["visible", "enabled"]; await this.performPointerAction(t, { actionName: r === 1 ? "click" : "dblclick", dispatch: async (c) => { await this.cua.clickPoint({ button: i, clickCount: r, ...wr(c.target) ? { inputPoint: c.inputPoint, inputTarget: c.inputTarget, loadTarget: c.target } : {}, modifiers: s, point: c.point, tabId: n, timeoutMs: o }); }, force: a, requiredStates: l, timeoutMs: o }); }
    async focusLocator(t, r) { let n = ["visible", "enabled"]; r.requireEditable && n.push("editable"); let { executionContextId: o, target: i } = await this.evaluateOnPlaywrightSelectorWithTarget(t.tab_id, t.selector, (s, a, l) => { for (let m of l.states) {
        let h = a.elementState(s, m);
        if (h.received === "error:notconnected")
            throw new Error("Element is not connected");
        if (!h.matches)
            throw new Error("Element is not " + m);
    } let c = l.retargetInput ? a.retarget(s, "follow-label") : s; if (c == null)
        throw new Error("Element is not connected"); s.scrollIntoView({ block: "center", inline: "nearest" }); let p = l.selectText ? a.selectText(c) : c.matches(":focus") ? "done" : a.focusNode(c, !1); if (p !== "done")
        throw new Error(String(p)); return l.iabInputTargetToken != null && Object.defineProperty(c, "__codexIabInputTargetToken", { configurable: !0, value: l.iabInputTargetToken, writable: !0 }), !0; }, { arg: { iabInputTargetToken: r.iabInputTargetToken, retargetInput: r.requireEditable || r.selectText === !0, selectText: r.selectText === !0, states: n }, timeoutMs: t.timeout_ms }); return { executionContextId: o, target: i }; }
    async readCheckedState(t) { return await this.evaluateOnPlaywrightSelector(t.tab_id, t.selector, (r, n) => { let o = n.elementState(r, "checked"); if (o.received === "error:notconnected")
        throw new Error("Element is not connected"); return { checked: !!o.matches, isRadio: !!o.isRadio }; }, { timeoutMs: t.timeout_ms }); }
    async readElementState(t, r) { return await this.evaluateOnPlaywrightSelectorAll(t.tab_id, t.selector, (n, o, i) => { let s = n[0] ?? null; if (!s)
        return !1; let a = o.elementState(s, i.stateName); return a.received === "error:notconnected" ? !1 : !!a.matches; }, { arg: { stateName: r }, timeoutMs: t.timeout_ms }); }
    async performPointerAction(t, r) { let n = r.force ? [Tw] : bK, o = null; for (let i of n) {
        let s = await this.resolvePointerActionTarget(t, { requiredStates: r.requiredStates, scrollAlignment: i });
        if (o = r.force ? null : await this.obstructingFrameHitTarget(s, { timeoutMs: r.timeoutMs }), o == null) {
            await r.dispatch(s);
            return;
        }
    } throw new Error(`Element does not receive pointer events at the ${r.actionName} point; ${o ?? "another element"} intercepts the ${r.actionName}`); }
    async resolvePointerActionTarget(t, r) { let n = await this.evaluateOnPlaywrightSelectorWithTarget(t.tab_id, t.selector, async (i, s, a, l) => { let c = { async waitForAnimationFrame() { await new Promise(m => { let h = i.ownerDocument?.defaultView; if (typeof h?.requestAnimationFrame == "function") {
            h.requestAnimationFrame(() => m());
            return;
        } if (typeof setTimeout == "function") {
            setTimeout(m, 0);
            return;
        } m(); }); }, sameRect(m, h) { return m.left === h.left && m.top === h.top && m.width === h.width && m.height === h.height; }, async waitForStableBoundingRect() { let m = i.getBoundingClientRect(), h = 0; for (let y = 0; y < 10; y += 1) {
            await c.waitForAnimationFrame();
            let w = i.getBoundingClientRect();
            if (c.sameRect(m, w)) {
                if (h += 1, m = w, h >= 2)
                    break;
                continue;
            }
            h = 0, m = w;
        } return m; } }; for (let m of a.requiredStates) {
        let h = s.elementState(i, m);
        if (h.received === "error:notconnected")
            throw new Error("Element is not connected");
        if (!h.matches)
            throw new Error("Element is not " + m);
    } i.scrollIntoView({ block: a.scrollAlignment.block, inline: a.scrollAlignment.inline, behavior: "instant" }); let p = await c.waitForStableBoundingRect(); for (let m of a.requiredStates) {
        let h = s.elementState(i, m);
        if (h.received === "error:notconnected")
            throw new Error("Element is not connected");
        if (!h.matches)
            throw new Error("Element is not " + m);
    } if (!p || p.width <= 0 || p.height <= 0)
        throw new Error("Element does not have a clickable bounding box"); return l.prepareFrameChainForPointerAction({ x: Math.max(0, p.left + p.width / 2), y: Math.max(0, p.top + p.height / 2) }, a.scrollAlignment); }, { arg: { requiredStates: r.requiredStates, scrollAlignment: r.scrollAlignment ?? Tw }, timeoutMs: t.timeout_ms }), o = await this.currentTopLevelPointForAction(n.result, n.oopifFrameChain, { timeoutMs: t.timeout_ms }); return { frameBoundaries: o.frameBoundaries, inputPoint: n.result, inputTarget: n.target, point: o.point, target: n.target }; }
    async currentTopLevelPointForAction(t, r, n = {}) { if (!r?.length)
        return { frameBoundaries: [], point: t }; let o = t, i = []; for (let s of r.slice().reverse()) {
        let a = await this.currentPointInParentFrame(s, o, n);
        o = a.point, i.push({ frame: s, ownerBackendNodeId: a.backendNodeId, point: o });
    } return { frameBoundaries: i, point: o }; }
    async currentPointInParentFrame(t, r, n = {}) { if (t.frameId == null)
        throw new Error("Cannot recompute OOPIF frame offset without a frame id"); let { backendNodeId: o } = await this.callCdpTarget(t.parentTarget, "DOM.getFrameOwner", { frameId: t.frameId }, n), { quads: i } = await this.callCdpTarget(t.parentTarget, "DOM.getContentQuads", { backendNodeId: o }, n), s = i[0], a = ro(r, t.size, s); if (a == null)
        throw new Error("Frame owner does not have content quads"); return { backendNodeId: o, point: a }; }
    async obstructingFrameHitTarget(t, r = {}) { for (let n of t.frameBoundaries.slice().reverse()) {
        let o = await this.obstructingFrameBoundaryHitTarget(n, r);
        if (o != null)
            return o;
    } return null; }
    async obstructingFrameBoundaryHitTarget(t, r = {}) { try {
        let n = await this.callCdpTarget(t.frame.parentTarget, "DOM.getNodeForLocation", { x: Math.round(t.point.x), y: Math.round(t.point.y), includeUserAgentShadowDOM: !0 }, { timeoutMs: r.timeoutMs, telemetryAttrs: Bn({ operation: this.performanceSpan.currentPlaywrightOperation("locator"), phase: "hit_target_check" }) });
        return n.backendNodeId === t.ownerBackendNodeId || n.frameId === t.frame.frameId ? null : await this.describeBackendNodeForHitTarget(t.frame.parentTarget, n.backendNodeId, r);
    }
    catch {
        return null;
    } }
    async describeBackendNodeForHitTarget(t, r, n = {}) { try {
        let { node: o } = await this.callCdpTarget(t, "DOM.describeNode", { backendNodeId: r }, n), i = o.localName || o.nodeName?.toLowerCase() || "unknown", s = Array.isArray(o.attributes) ? o.attributes : [], a = dD(s, "id"), l = dD(s, "class");
        return `<${i}${a ? `#${a}` : ""}${l ? `.${l.trim().split(/\s+/).slice(0, 3).join(".")}` : ""}>`;
    }
    catch {
        return "another element";
    } }
    async nodeForResolvedSelectorTarget(t, r) {
        let n = await this.callRuntimeEvaluateWithPlaywrightInjectedInTarget(t.target, { expression: `(() => {
            ${Ss}
            ${e.selectorScopeFunctions()}
            const initialInjected = window.${e.injectedConstant};
            const parsed = initialInjected.parseSelector(${JSON.stringify(t.selector)});
            const scope = selectorScopeFor(initialInjected, parsed);
            const element = scope
              ? querySelectorStrictWithVisibleFallback(scope.injected, scope.parsed, scope.root)
              : null;
            if (!element) throw new Error("No element matched selector");
            return element;
          })()`, returnByValue: !1 }, r, t.frameId), o = n.result?.objectId, i = n.exceptionDetails?.exception?.objectId;
        try {
            if (n.exceptionDetails != null && Mn(n), o == null)
                throw new Error("Playwright selector did not resolve to a DOM node");
            let { node: s } = await this.callCdpTarget(t.target, "DOM.describeNode", { objectId: o }, r), a = t.frameId ?? (t.selector.includes(Du) ? await this.frameIdForElementObject(t.target, o, r) : void 0);
            return { backendNodeId: s.backendNodeId, ...a == null ? {} : { frameId: a } };
        }
        finally {
            await this.releaseRuntimeObjects(t.target, [o, i], r);
        }
    }
    async frameIdForElementObject(t, r, n) {
        let o = await this.callCdpTarget(t, "Runtime.callFunctionOn", { functionDeclaration: `function () {
          try {
            return this.ownerDocument?.defaultView?.frameElement ?? null;
          } catch {
            return null;
          }
        }`, objectId: r, returnByValue: !1 }, n), i = o.result?.objectId, s = o.exceptionDetails?.exception?.objectId;
        try {
            if (o.exceptionDetails != null && Mn(o), i == null)
                return;
            let { node: a } = await this.callCdpTarget(t, "DOM.describeNode", { objectId: i }, n);
            return a.frameId;
        }
        finally {
            await this.releaseRuntimeObjects(t, [i, s], n);
        }
    }
    async withPlaywrightNode(t, r, n = {}) { t.frameId != null && await this.ensurePlaywrightInjectedInTarget(t.target, n, t.frameId); let o = t.frameId == null ? void 0 : (await this.playwrightFrameWorld(t.target, t.frameId, n)).executionContextId, { object: i } = await this.callCdpTarget(t.target, "DOM.resolveNode", { backendNodeId: t.backendNodeId, ...o == null ? {} : { executionContextId: o } }, n); if (i.objectId == null)
        throw new Error("Playwright node has no document-bound handle"); try {
        return await r(i.objectId, o);
    }
    finally {
        await this.releaseRuntimeObjects(t.target, [i.objectId], n);
    } }
    async releaseRuntimeObjects(t, r, n) { let o = { deadlineMs: Date.now() + sD, telemetryAttrs: n.telemetryAttrs, timeoutMs: sD }; for (let i of new Set(r))
        i != null && await this.callCdpTarget(t, "Runtime.releaseObject", { objectId: i }, o).catch(() => { }); }
    async evaluateSingleSelectorWithTarget(t, r, n, o, i = {}) { je(i); let s = await this.resolveSelectorTarget(t, r, i); return je(i), { ...i.includeFrameIdentity === !0 ? { frameIdentity: await this.frameIdentityForResolvedSelectorTarget(s, i) } : {}, ...await this.evaluateSingleSelectorInResolvedTarget(s, n, o, i), oopifFrameChain: s.oopifFrameChain, target: s.target }; }
    async frameIdentityForResolvedSelectorTarget(t, r = {}) { let { frameTree: n } = await this.callCdpTarget(t.target, "Page.getFrameTree", void 0, r), o = t.selector.includes(Du) ? await this.uniqueFrameForResolvedSelectorDocumentUrl(t, n, r) : t.frameId != null ? ym(n).find(i => i.id === t.frameId) : n.frame; if (!(o == null || !o.id || !o.loaderId || !o.url))
        return { ...o.domainAndRegistry == null ? {} : { domainAndRegistry: o.domainAndRegistry }, frameId: o.id, loaderId: o.loaderId, url: o.url }; }
    async uniqueFrameForResolvedSelectorDocumentUrl(t, r, n = {}) {
        let { result: o } = await this.evaluateSingleSelectorInResolvedTarget(t, `(element) =>
          element.ownerDocument.defaultView?.location.href ??
          window.location.href`, void 0, n), i = ym(r).filter(s => s.url === o.split("#", 1)[0]);
        return i.length === 1 ? i[0] : void 0;
    }
    async evaluateSingleSelectorInResolvedTarget(t, r, n, o = {}) {
        let i = await this.callRuntimeEvaluateWithPlaywrightInjectedInTarget(t.target, { awaitPromise: !0, returnByValue: !0, expression: `(async () => {
          ${Ss}
          ${e.selectorScopeFunctions()}
          const initialInjected = window.${e.injectedConstant};
          const parsed = initialInjected.parseSelector(${JSON.stringify(t.selector)});
          const scope = selectorScopeFor(initialInjected, parsed, ${o.strict === !0});
          const element = scope
            ? querySelectorStrictWithVisibleFallback(scope.injected, scope.parsed, scope.root, ${o.strict === !0})
            : null;
          if (!element) throw new Error("No element matched selector");
          return await (${r})(element, scope.injected, ${Sw(n)}, scope);
        })()` }, o, t.frameId);
        return { executionContextId: i.executionContextId, result: Mn(i) };
    }
    async evaluateSelectorAllInTarget(t, r, n, o, i = {}) {
        je(i);
        let s = await this.resolveSelectorTarget(t, r, i);
        return je(i), await this.evaluateWithPlaywrightInjectedInTarget(s.target, `(async () => {
          ${Ss}
          ${e.selectorScopeFunctions()}
          const initialInjected = window.${e.injectedConstant};
          const parsed = initialInjected.parseSelector(${JSON.stringify(s.selector)});
          const scope = selectorScopeFor(initialInjected, parsed);
          const elements = scope
            ? scope.injected.querySelectorAll(scope.parsed, scope.root)
            : [];
          const scopedInjected = scope ? scope.injected : initialInjected;
          return await (${n})(elements, scopedInjected, ${Sw(o)});
        })()`, i, s.frameId);
    }
    async resolveSelectorTarget(t, r, n = {}) { let o = t, i = r; for (;;) {
        je(n);
        let s = await this.frameRouteForFirstOopif(o, i, n);
        if (s == null)
            return { ...o.frameId == null ? {} : { frameId: o.frameId }, oopifFrameChain: o.oopifFrameChain, selector: i, target: o.target };
        o = s, i = s.selector;
    } }
    async frameRouteForFirstOopif(t, r, n = {}) {
        if (!r.includes(Du) || !xK(this.cdp))
            return null;
        je(n), await this.cdp.enableOopifAutoAttach(t.target.tabId, n), je(n);
        let o = await this.callRuntimeEvaluateWithPlaywrightInjectedInTarget(t.target, { expression: `(() => {
          ${Ss}
          ${e.selectorScopeFunctions()}
          const initialInjected = window.${e.injectedConstant};
          const parsedSelector = initialInjected.parseSelector(${JSON.stringify(r)});
          let currentRoot = document;
          let currentInjected = initialInjected;
          let partStart = 0;

          while (true) {
            const enterFrameIndex = parsedSelector.parts.findIndex(
              (part, index) =>
                index >= partStart &&
                part.name === "internal:control" &&
                part.body === "enter-frame"
            );

            if (enterFrameIndex === -1) return null;

            const frameSelector = sliceParsedSelector(
              parsedSelector,
              partStart,
              enterFrameIndex
            );
            const frameElement = querySelectorStrictWithVisibleFallback(
              currentInjected,
              frameSelector,
              currentRoot,
              ${n.strict === !0}
            );
            if (!frameElement) return null;

            const tagName = String(frameElement.localName || frameElement.tagName || "").toLowerCase();
            if (tagName !== "iframe" && tagName !== "frame") {
              throw new Error("internal:control=enter-frame must target a frame element");
            }

            const state = currentInjected.elementState(frameElement, "visible");
            if (state.received === "error:notconnected") {
              throw new Error("Frame is not connected");
            }

            if (!state.matches) {
              throw new Error("Frame is not visible");
            }

            frameElement.scrollIntoView({
              block: "center",
              inline: "nearest",
              behavior: "instant",
            });
            let frameWindow;
            let frameDocument;
            try {
              frameWindow = frameElement.contentWindow;
              frameDocument = frameElement.contentDocument || frameWindow?.document;
            } catch {
              return frameElement;
            }

            if (!frameWindow || !frameDocument) return frameElement;

            currentRoot = frameDocument;
            currentInjected = injectedForWindow(initialInjected, frameWindow);
            partStart = enterFrameIndex + 1;
          }
        })()`, returnByValue: !1 }, n, t.frameId), i = o.result?.objectId, s = o.exceptionDetails?.exception?.objectId, a, l;
        try {
            let c = Aw(o);
            if (c != null)
                throw c;
            if (i == null)
                return je(n), null;
            je(n);
            let [p, m] = await Promise.allSettled([this.callCdpTarget(t.target, "Runtime.callFunctionOn", { functionDeclaration: `function () {
              let ownerDocument = this.ownerDocument;
              let enterFrameCount = 1;
              while (ownerDocument !== document) {
                const ownerFrame = ownerDocument?.defaultView?.frameElement;
                if (!ownerFrame) throw new Error("Frame is not connected");
                ownerDocument = ownerFrame.ownerDocument;
                enterFrameCount += 1;
              }

              const rect = this.getBoundingClientRect();
              if (!rect || rect.width <= 0 || rect.height <= 0) {
                throw new Error("Frame does not have an actionable bounding box");
              }

              const width = Number(this.clientWidth) || rect.width;
              const height = Number(this.clientHeight) || rect.height;
              if (width <= 0 || height <= 0) {
                throw new Error("Frame does not have an actionable bounding box");
              }

              return {
                enterFrameCount,
                frameName: this.name || undefined,
                frameUrl: this.src || undefined,
                size: { height, width },
              };
            }`, objectId: i, returnByValue: !0 }, n), this.callCdpTarget(t.target, "DOM.describeNode", { objectId: i }, n)]);
            if (p.status === "fulfilled")
                a = p.value.result?.objectId, l = p.value.exceptionDetails?.exception?.objectId;
            else
                throw p.reason;
            if (m.status === "rejected")
                throw m.reason;
            let h = Mn(p.value), y = typeof m.value.node.frameId == "string" ? m.value.node.frameId : await this.frameIdForFrameMatch(t.target, h, n), w = _K(h.frameUrl) || y != null && this.playwrightFrameWorlds.has(Cs(t.target, y)) && this.cdp.targetForFrame(t.target.tabId, y) == null ? null : await this.targetForFrame(t.target.tabId, y, { parentTarget: t.target, ...h.frameUrl != null ? { url: h.frameUrl } : {} }, n);
            if (je(n), (w == null || n.isolatedWorld === !0) && y == null)
                throw new Error(vw);
            let v = { frameId: y, parentTarget: t.target, size: h.size }, S = t.oopifFrameChain[t.oopifFrameChain.length - 1], R = S != null && Cs(S.parentTarget) === Cs(v.parentTarget) ? [...t.oopifFrameChain.slice(0, -1), v] : [...t.oopifFrameChain, v];
            return { ...y != null && (w == null || n.isolatedWorld === !0) ? { frameId: y } : {}, oopifFrameChain: R, selector: vK(r, h.enterFrameCount), target: w ?? t.target };
        }
        finally {
            await this.releaseRuntimeObjects(t.target, [i, s, a, l], n);
        }
    }
    async frameIdForFrameMatch(t, r, n = {}) { if (r.frameUrl == null)
        return null; let { frameTree: o } = await this.callCdpTarget(t, "Page.getFrameTree", void 0, n), i = ym(o).filter(s => s.url !== r.frameUrl ? !1 : r.frameName == null || s.name === r.frameName); return i.length !== 1 ? null : i[0]?.id ?? null; }
    async targetForFrame(t, r, n, o = {}) { return je(o), SK(this.cdp) ? await this.cdp.targetForFrameOrAttach(t, r, o, n) : r == null ? null : this.cdp.targetForFrame(t, r); }
    async evaluateWithPlaywrightInjected(t, r, n) { let o = H(t); return await this.evaluateWithPlaywrightInjectedInTarget({ tabId: o }, r, n); }
    async evaluateWithPlaywrightInjectedInTarget(t, r, n = {}, o) { let i = await this.callRuntimeEvaluateWithPlaywrightInjectedInTarget(t, { expression: r, returnByValue: !0, awaitPromise: !0 }, n, o); return Mn(i); }
    async callRuntimeEvaluateWithPlaywrightInjectedInTarget(t, r, n = {}, o) {
        je(n), await this.ensurePlaywrightInjectedInTarget(t, n, o);
        let i = async () => {
            let s = wK(n), a = { ...r, expression: `(() => {
          if (!window.${e.injectedConstant}) {
            throw new Error(${JSON.stringify(Ew)});
          }
          return (${r.expression});
        })()` };
            return delete a.timeout, s.timeoutMs != null && s.timeoutMs > 0 && (a.timeout = s.timeoutMs), { options: s, result: await this.callRuntimeEvaluateInExecutionTarget({ frameId: o, oopifFrameChain: [], target: t }, a, s) };
        };
        try {
            let s = await i(), a = Aw(s.result);
            if (a == null)
                return s.result;
            if (uD(a))
                o == null ? this.clearPlaywrightStateForTarget(t) : this.deletePlaywrightFrameWorld(t, o);
            else {
                if (o == null || !lD(a))
                    return s.result;
                this.deletePlaywrightFrameWorld(t, o);
            }
            await this.releaseRuntimeObjects(t, [s.result.result?.objectId, s.result.exceptionDetails?.exception?.objectId], s.options);
        }
        catch (s) {
            if (uD(s))
                o == null ? this.clearPlaywrightStateForTarget(t) : this.deletePlaywrightFrameWorld(t, o);
            else {
                if (o == null || !lD(s))
                    throw s;
                this.deletePlaywrightFrameWorld(t, o);
            }
        }
        return je(n), await this.ensurePlaywrightInjectedInTarget(t, n, o), je(n), (await i()).result;
    }
    async ensurePlaywrightInjected(t, r = {}) { let n = Number(t); if (!Number.isFinite(n))
        throw new Error("ensurePlaywrightInjected requires numeric tab_id"); await this.ensurePlaywrightInjectedInTarget({ tabId: n }, r); }
    async ensurePlaywrightInjectedInTarget(t, r = {}, n) { let o = Cs(t, n), i = yK(r.telemetryAttrs) ?? "unknown"; this.playwrightInjectedTargets.has(o) || (je(r), Mn(await this.callRuntimeEvaluateInExecutionTarget({ frameId: n, oopifFrameChain: [], target: t }, gm, { timeoutMs: r.timeoutMs, ...r.deadlineMs == null ? {} : { deadlineMs: r.deadlineMs }, telemetryAttrs: Bn({ operation: i, phase: "inject_install" }) }, oD)), this.playwrightInjectedTargets.add(o)); }
    async callRuntimeEvaluateInExecutionTarget(t, r, n = {}, o) { je(n); let i = t.frameId == null ? void 0 : (await this.playwrightFrameWorld(t.target, t.frameId, n)).executionContextId; return je(n), { ...await this.callCdpTarget(t.target, "Runtime.evaluate", { ...r, ...i == null ? {} : { contextId: i } }, { ...n, expressionCacheKey: o }), executionContextId: i }; }
    async playwrightFrameWorld(t, r, n = {}) { let o = Cs(t, r), i = this.playwrightFrameWorlds.get(o); if (i != null)
        return i; je(n); let a = { executionContextId: (await this.callCdpTarget(t, "Page.createIsolatedWorld", { frameId: r, grantUniveralAccess: !1, worldName: gK }, n)).executionContextId }; if (this.playwrightFrameWorlds.set(o, a), this.playwrightFrameWorlds.size > hK) {
        let l = this.playwrightFrameWorlds.keys().next().value;
        l != null && (this.playwrightFrameWorlds.delete(l), this.playwrightInjectedTargets.delete(l));
    } return a; }
    deletePlaywrightFrameWorld(t, r) { let n = Cs(t, r); this.playwrightFrameWorlds.delete(n), this.playwrightInjectedTargets.delete(n); }
    clearPlaywrightFrameWorldsForTarget(t) { let r = cD(t); for (let n of this.playwrightFrameWorlds.keys())
        r.some(o => Cw(n, o)) && (this.playwrightFrameWorlds.delete(n), this.playwrightInjectedTargets.delete(n)); }
    clearPlaywrightStateForTarget(t) { let r = cD(t); for (let n of this.playwrightInjectedTargets)
        r.some(o => Cw(n, o)) && this.playwrightInjectedTargets.delete(n); for (let n of this.playwrightFrameWorlds.keys())
        r.some(o => Cw(n, o)) && this.playwrightFrameWorlds.delete(n); }
    async callCdpTarget(t, r, n, o = {}) { let i = this.cdp; return typeof i.callTarget == "function" ? await i.callTarget(t, r, n, o) : await this.cdp.call(t.tabId, r, n, o); }
    static selectorScopeFunctions() {
        return `
function injectedForWindow(rootInjected, targetWindow) {
  if (!targetWindow) throw new Error("Frame window is not available");
  if (targetWindow.${e.injectedConstant}) {
    return targetWindow.${e.injectedConstant};
  }

  targetWindow.${e.injectedConstant} = new rootInjected.constructor(targetWindow, {
    isUnderTest: false,
    sdkLanguage: "javascript",
    testIdAttributeName: "data-testid",
    stableRafCount: 1,
    browserName: "chromium",
    customEngines: [],
  });
  return targetWindow.${e.injectedConstant};
}

const unsupportedFrameAccessMessage =
  "Cross-origin or out-of-process iframes are not supported by this runtime selector path";

function sliceParsedSelector(parsedSelector, startIndex, endIndex) {
  const sliced = {
    ...parsedSelector,
    parts: parsedSelector.parts.slice(startIndex, endIndex),
  };

  if (parsedSelector.capture === undefined) {
    delete sliced.capture;
  } else if (
    parsedSelector.capture >= startIndex &&
    parsedSelector.capture < endIndex
  ) {
    sliced.capture = parsedSelector.capture - startIndex;
  } else {
    delete sliced.capture;
  }

  return sliced;
}

function frameContentSize(frameElement) {
  const rect = frameElement.getBoundingClientRect();
  if (!rect || rect.width <= 0 || rect.height <= 0) {
    throw new Error("Frame does not have an actionable bounding box");
  }

  const clientWidth = Number(frameElement.clientWidth) || rect.width;
  const clientHeight = Number(frameElement.clientHeight) || rect.height;
  if (clientWidth <= 0 || clientHeight <= 0) {
    throw new Error("Frame does not have an actionable bounding box");
  }

  return {
    height: clientHeight,
    width: clientWidth,
  };
}

function frameContentGeometry(frameElement) {
  const rect = frameElement.getBoundingClientRect();
  if (!rect || rect.width <= 0 || rect.height <= 0) {
    throw new Error("Frame does not have an actionable bounding box");
  }

  const offsetWidth = Number(frameElement.offsetWidth) || rect.width;
  const offsetHeight = Number(frameElement.offsetHeight) || rect.height;
  const clientWidth = Number(frameElement.clientWidth);
  const clientHeight = Number(frameElement.clientHeight);
  if (
    offsetWidth <= 0 ||
    offsetHeight <= 0 ||
    clientWidth <= 0 ||
    clientHeight <= 0
  ) {
    throw new Error("Frame does not have an actionable bounding box");
  }

  const scaleX = rect.width / offsetWidth;
  const scaleY = rect.height / offsetHeight;
  return {
    left: rect.left + (Number(frameElement.clientLeft) || 0) * scaleX,
    scaleX,
    scaleY,
    top: rect.top + (Number(frameElement.clientTop) || 0) * scaleY,
  };
}

function pointThroughFrameElement(point, frameElement) {
  const geometry = frameContentGeometry(frameElement);
  return {
    x: geometry.left + point.x * geometry.scaleX,
    y: geometry.top + point.y * geometry.scaleY,
  };
}

function prepareFrameChainForPointerAction(frameChain, point, scrollAlignment) {
  const block = scrollAlignment?.block ?? "center";
  const inline = scrollAlignment?.inline ?? "nearest";
  let currentPoint = point;

  for (const frameScope of frameChain.slice().reverse()) {
    frameScope.element.scrollIntoView({
      block,
      inline,
      behavior: "instant",
    });

    const state = frameScope.injected.elementState(frameScope.element, "visible");
    if (state.received === "error:notconnected") {
      throw new Error("Frame is not connected");
    }

    if (!state.matches) {
      throw new Error("Frame is not visible");
    }

    currentPoint = pointThroughFrameElement(currentPoint, frameScope.element);
  }

  return currentPoint;
}

function selectorScopeFor(initialInjected, parsedSelector, strict = false) {
  let currentRoot = document;
  let currentInjected = initialInjected;
  const frameChain = [];
  let partStart = 0;

  while (true) {
    const enterFrameIndex = parsedSelector.parts.findIndex(
      (part, index) =>
        index >= partStart &&
        part.name === "internal:control" &&
        part.body === "enter-frame"
    );

    if (enterFrameIndex === -1) {
      return {
        frameChain,
        injected: currentInjected,
        prepareFrameChainForPointerAction: (point, scrollAlignment) =>
          prepareFrameChainForPointerAction(frameChain, point, scrollAlignment),
        root: currentRoot,
        parsed: sliceParsedSelector(
          parsedSelector,
          partStart,
          parsedSelector.parts.length
        ),
      };
    }

    const frameSelector = sliceParsedSelector(
      parsedSelector,
      partStart,
      enterFrameIndex
    );
    const frameElement = querySelectorStrictWithVisibleFallback(
      currentInjected,
      frameSelector,
      currentRoot,
      strict
    );

    if (!frameElement) return null;
    const tagName = String(frameElement.localName || frameElement.tagName || "").toLowerCase();
    if (tagName !== "iframe" && tagName !== "frame") {
      throw new Error("internal:control=enter-frame must target a frame element");
    }

    let frameWindow;
    let frameDocument;
    try {
      frameWindow = frameElement.contentWindow;
      frameDocument = frameElement.contentDocument || frameWindow?.document;
    } catch {
      throw new Error(unsupportedFrameAccessMessage);
    }

    if (!frameWindow || !frameDocument) {
      throw new Error(unsupportedFrameAccessMessage);
    }

    frameChain.push({ element: frameElement, injected: currentInjected });
    currentRoot = frameDocument;
    currentInjected = injectedForWindow(initialInjected, frameWindow);
    partStart = enterFrameIndex + 1;
  }
}
`;
    }
};

function Bn(e) { return { "browser_use.playwright.operation": e.operation, "browser_use.playwright.phase": e.phase }; }

function yK(e) { let t = e?.["browser_use.playwright.operation"]; return typeof t == "string" ? t : void 0; }

async function aD(e, t, r, n) { let o = r.retry !== !1 ? n.startLocatorRetry() : void 0; try {
    for (;;) {
        je(r);
        try {
            let i = await t();
            return o?.finish("success"), i;
        }
        catch (i) {
            if (_m(i) || r.retry === !1)
                throw i;
            if (o?.attemptFailed(), Date.now() - r.startedAt >= r.timeoutMs) {
                o?.finish("timeout");
                let s = i instanceof Error ? i.message : String(i);
                throw new Error(`Timed out after ${r.timeoutMs}ms waiting for selector ${e}: ${s}`, { cause: i });
            }
            je(r), await new Promise(s => setTimeout(s, mD(r)));
        }
    }
}
catch (i) {
    throw i instanceof Error && i.message === pD && r.timeoutMs > 0 && r.deadlineMs === r.startedAt + r.timeoutMs && o?.finish("timeout"), i;
} }

function je(e) { if (e.deadlineMs != null && Date.now() >= e.deadlineMs)
    throw new Error(pD); }

function xw(e, t, r) { let n = t + r; return r <= 0 || e.deadlineMs != null && e.deadlineMs <= n ? e : { ...e, deadlineMs: n }; }

function wK(e) { if (je(e), e.deadlineMs == null)
    return e; let t = e.deadlineMs - Date.now(); return { ...e, timeoutMs: Math.max(1, Math.min(e.timeoutMs ?? t, t)) }; }

function mD(e) { return e.deadlineMs == null ? 100 : Math.max(1, Math.min(100, e.deadlineMs - Date.now())); }

function Mn(e) { let t = Aw(e); if (t != null)
    throw t; return e.result?.value; }

function Aw(e) { if (e.exceptionDetails == null)
    return null; let t = e.exceptionDetails.exception?.value, r = e.exceptionDetails.exception?.description ?? e.exceptionDetails.text ?? (t == null ? "Playwright selector evaluation failed." : String(t)); return new Error(`Browser Use encountered an error interacting with this webpage: ${r}`); }

function _m(e) { return String(e instanceof Error ? e.message : e).includes("strict mode violation:"); }

function uD(e) { let t = String(e instanceof Error ? e.message : e); return t.includes(Ew) || /Cannot read properties of (?:undefined|null) \(reading ['"]incrementalAriaSnapshot['"]\)/.test(t); }

const Ss = `
function querySelectorStrictWithVisibleFallback(injected, parsedSelector, root, strict = false) {
  const matches = injected.querySelectorAll(parsedSelector, root);
  if (!matches.length) {
    injected.checkDeprecatedSelectorUsage(parsedSelector, matches);
    return null;
  }

  if (matches.length === 1) {
    injected.checkDeprecatedSelectorUsage(parsedSelector, matches);
    return matches[0];
  }

  if (strict) throw injected.strictModeViolationError(parsedSelector, matches);

  const visibleMatches = matches.filter((element) => {
    const state = injected.elementState(element, "visible");
    return !!state.matches;
  });
  if (visibleMatches.length === 1) return visibleMatches[0];

  throw injected.strictModeViolationError(parsedSelector, matches);
}
`;

function Sw(e) { return JSON.stringify(e) ?? "undefined"; }

function lD(e) { let t = e instanceof Error ? e.message : String(e); return /Cannot find context|Execution context was destroyed|context.*destroyed|context.*not found/i.test(t); }

const Du = " >> internal:control=enter-frame >> ";

function bm(e) { return { oopifFrameChain: [], target: { tabId: H(e) } }; }

function _K(e) { if (e == null)
    return !1; try {
    let { protocol: t } = new URL(e);
    return t === "about:" || t === "data:";
}
catch {
    return !1;
} }

function vK(e, t) { return e.split(Du).slice(t).join(Du); }

function ym(e) { return [e.frame, ...(e.childFrames ?? []).flatMap(t => ym(t))]; }

function Cs(e, t) { let r = t == null ? "" : `:${t}`; return e.sessionId != null ? `${e.tabId}:${e.sessionId}${r}` : e.targetId != null ? `${e.tabId}:${e.targetId}${r}` : `${e.tabId}${r}`; }

function cD(e) { let t = [...e.sessionId == null ? [] : [`${e.tabId}:${e.sessionId}`], ...e.targetId == null ? [] : [`${e.tabId}:${e.targetId}`]]; return t.length === 0 ? [String(e.tabId)] : t; }

function Cw(e, t) { return e === t || e.startsWith(`${t}:`); }

function dD(e, t) { for (let r = 0; r < e.length; r += 2)
    if (e[r] === t)
        return e[r + 1]; }

function xK(e) { return typeof e.enableOopifAutoAttach == "function" && typeof e.targetForFrame == "function"; }

function SK(e) { return typeof e.targetForFrameOrAttach == "function"; }

const CK = 8;

const TK = 2e3;

const EK = 200;

const AK = 2e5;

const fD = 100;

const IK = 100;

const PK = 20;

const RK = "octopus.browser.actions";

const kK = /\bimport\b(?:(?:\s+)|(?:\/\/[^\n\r]*(?:\r?\n|$))|(?:\/\*[\s\S]*?\*\/))*\(/;

const Lo = new Map;

const DK = 0;

const gD = async (e, t) => { let r = H(e.tab_id), n = Me(e), o = Date.now() + n; _D(e.script); let i; if (e.selector_mode === "all") {
    if (e.selector == null)
        throw new Error("playwright.evaluate selector_mode=all requires a selector");
    i = await t.playwright.prepareReadonlyLocatorAll(r, e.selector, kw(e.script, void 0, "elements", "selectorScope?.root?.defaultView ?? globalThis"), { deadlineMs: o, timeoutMs: n });
} let { evaluationTarget: s, runtimeResult: a } = e.selector == null || i != null ? await BK(t, r, e.script, o, n, i) : await MK(t, r, e.selector, e.script, o, n); try {
    return { value: xD(a) };
}
finally {
    await yD(t, s.target, [a.result?.objectId, a.exceptionDetails?.exception?.objectId], vm());
} };

async function BK(e, t, r, n, o, i) { let s = i?.target ?? { tabId: t }, a = { frameId: i?.frameId ?? vD(await Gr(e, s, "Page.getFrameTree", void 0, { telemetryAttrs: { "browser_use.cdp.eval.kind": "readonly_js_frame_tree" }, deadlineMs: n, timeoutMs: o })), target: s }, l = await Iw(e, a, n, o), c; try {
    c = await Pw(e, a, l.executionContextId, r, n, o, i?.expression);
}
catch (p) {
    if (!wD(p))
        throw p;
    Lo.delete(Rw(a.target, a.frameId)), l = await Iw(e, a, n, o), c = await Pw(e, a, l.executionContextId, r, n, o, i?.expression);
} return { evaluationTarget: a, runtimeResult: c }; }

async function MK(e, t, r, n, o, i) { let s = e.commandTiming.startLocatorRetry(); for (;;) {
    let a;
    try {
        let l = await e.playwright.resolvePlaywrightSelectorNode(t, r, { deadlineMs: o, retry: !1, timeoutMs: i });
        a = { backendNodeId: l.backendNodeId, frameId: l.frameId ?? vD(await Gr(e, l.target, "Page.getFrameTree", void 0, { telemetryAttrs: { "browser_use.cdp.eval.kind": "readonly_js_frame_tree" }, deadlineMs: o, timeoutMs: i })), target: l.target };
        let c = await Iw(e, a, o, i), p = await Pw(e, a, c.executionContextId, n, o, i);
        return s.finish("success"), { evaluationTarget: a, runtimeResult: p };
    }
    catch (l) {
        if (_m(l))
            throw l;
        if (a != null && wD(l) && Lo.delete(Rw(a.target, a.frameId)), s.attemptFailed(), Date.now() >= o) {
            s.finish("timeout");
            let c = l instanceof Error ? l.message : String(l);
            throw new Error(`Timed out after ${i}ms evaluating selector ${r}: ${c}`, { cause: l });
        }
        await new Promise(c => setTimeout(c, Math.max(1, Math.min(IK, o - Date.now()))));
    }
} }

async function Iw(e, t, r, n) { let { frameId: o, target: i } = t, s = Rw(i, o), a = Lo.get(s); if (a != null)
    return a; let c = { executionContextId: (await Gr(e, i, "Page.createIsolatedWorld", { frameId: o, grantUniveralAccess: !1, worldName: RK }, { telemetryAttrs: { "browser_use.cdp.eval.kind": "readonly_js_world" }, deadlineMs: r, timeoutMs: n })).executionContextId, frameId: o }; if (Lo.set(s, c), Lo.size > PK) {
    let p = Lo.keys().next().value;
    p != null && Lo.delete(p);
} return c; }

async function Pw(e, t, r, n, o, i, s) { let a = { deadlineMs: o, telemetryAttrs: { "browser_use.cdp.eval.kind": "readonly_live_dom" }, timeoutMs: i }; s != null && await Gr(e, t.target, "Runtime.evaluate", { ...gm, contextId: r, timeout: hD(o, i) }, a); let l = t.backendNodeId == null ? void 0 : await OK(e, t.target, t.backendNodeId, r, a), c = s ?? (l == null ? NK(n) : kw(n, `rawWindow[${JSON.stringify(l)}]`)); try {
    return await Gr(e, t.target, "Runtime.evaluate", { awaitPromise: !0, contextId: r, expression: c, returnByValue: !0, timeout: hD(o, i) }, a);
}
finally {
    l != null && await bD(e, t.target, r, l, vm());
} }

async function OK(e, t, r, n, o) { let { object: i } = await Gr(e, t, "DOM.resolveNode", { backendNodeId: r, executionContextId: n }, o), s = i.objectId; if (s == null)
    throw new Error("playwright.evaluate could not resolve the locator element"); let a = `__browserUseReadonlyElement${DK++}`, l, c; try {
    let p = await Gr(e, t, "Runtime.callFunctionOn", { arguments: [{ value: a }], functionDeclaration: "function (name) { globalThis[name] = this; }", objectId: s, returnByValue: !0 }, o);
    return l = p.exceptionDetails?.exception?.objectId, xD(p), a;
}
catch (p) {
    throw c = vm(), await bD(e, t, n, a, c), p;
}
finally {
    await yD(e, t, [s, l], c ?? vm());
} }

async function bD(e, t, r, n, o) { await Gr(e, t, "Runtime.evaluate", { contextId: r, expression: `delete globalThis[${JSON.stringify(n)}]`, returnByValue: !0 }, o).catch(() => { }); }

async function yD(e, t, r, n) { for (let o of new Set(r))
    o != null && await Gr(e, t, "Runtime.releaseObject", { objectId: o }, n).catch(() => { }); }

function vm() { return { deadlineMs: Date.now() + fD, telemetryAttrs: { "browser_use.cdp.eval.kind": "readonly_live_dom_cleanup" }, timeoutMs: fD }; }

function hD(e, t) { let r = e - Date.now(); if (r <= 0)
    throw new Error("playwright.evaluate exceeded its deadline"); return Math.max(1, Math.min(t, r)); }

async function Gr(e, t, r, n, o) { return t.sessionId != null || t.targetId != null ? await e.cdp.callTarget(t, r, n, o) : await e.cdp.call(t.tabId, r, n, o); }

function Rw(e, t) { let r = e.sessionId ?? e.targetId; return r == null ? `${e.tabId}:${t}` : `${e.tabId}:${r}:${t}`; }

function wD(e) { let t = e instanceof Error ? e.message : String(e); return /Cannot find context|Execution context was destroyed|context.*destroyed|context.*not found/i.test(t); }

function NK(e) { return kw(e); }

function kw(e, t, r, n = "globalThis") {
    return _D(e), `(() => {
  const RESULT_MAX_DEPTH = ${CK};
  const RESULT_MAX_ARRAY_LENGTH = ${TK};
  const RESULT_MAX_OBJECT_KEYS = ${EK};
  const RESULT_MAX_STRING_LENGTH = ${AK};
  const protectedCredentialFieldPolicy = ${JSON.stringify(on)};
  const isProtectedCredentialField = ${ai.toString()};
  const ELEMENT_NODE = 1;
  const TEXT_NODE = 3;
  const COMMENT_NODE = 8;
  const DOCUMENT_NODE = 9;
  const DOCUMENT_TYPE_NODE = 10;
  const DOCUMENT_FRAGMENT_NODE = 11;
  const rawWindow = ${n};
  const rawDocument = rawWindow.document;
  const wrapperToken = {};
  const nodeWrappers = new WeakMap();
  const rawNodesByWrapper = new WeakMap();
  const styleWrappers = new WeakSet();
  const mediaListWrappers = new WeakMap();
  const cssRuleWrappers = new WeakMap();
  const styleSheetWrappers = new WeakMap();
  const cssWrappers = new WeakSet();
  const rangeWrappers = new WeakMap();
  const rawRangesByWrapper = new WeakMap();
  const selectionWrappers = new WeakMap();
  const selectionAndRangeWrappers = new WeakSet();
  const fontFaceSetWrappers = new WeakMap();
  const mediaQueryListWrappers = new WeakMap();
  const readonlyApiWrappers = new WeakSet();

  // Define fixed data properties so sandbox bindings cannot be replaced,
  // deleted, or reconfigured by the evaluated script.
  const defineReadonly = (target, name, value) => {
    Object.defineProperty(target, name, {
      configurable: false,
      enumerable: true,
      value,
      writable: false,
    });
  };

  const defineHiddenReadonly = (target, name, value) => {
    Object.defineProperty(target, name, {
      configurable: false,
      enumerable: false,
      value,
      writable: false,
    });
  };

  const defineReadonlyGetter = (target, name, get) => {
    Object.defineProperty(target, name, {
      configurable: false,
      enumerable: true,
      get,
    });
  };

  const readonlyError = (name) =>
    new Error(
      name +
        " is not available in playwright.evaluate because the DOM is read-only",
    );

  const camelToHyphen = (value) =>
    String(value).replace(/[A-Z]/g, (letter) => "-" + letter.toLowerCase());

  const ownKeys = (value) => Object.keys(value).slice(0, RESULT_MAX_OBJECT_KEYS);

  const makeReadonlyList = (items, configure) => {
    const list = Array.from(items);
    defineHiddenReadonly(list, "constructor", undefined);
    defineHiddenReadonly(list, "item", (index) => list[index] ?? null);
    if (typeof configure === "function") configure(list);
    return Object.freeze(list);
  };

  const makeReadonlyAttribute = (attribute) =>
    Object.freeze({
      name: attribute.name,
      nodeName: attribute.name,
      nodeValue: attribute.value,
      specified: true,
      value: attribute.value,
    });

  const makeReadonlyAttributeList = (element) => {
    const attributes = element.attributes;
    const protectedField = isProtectedCredentialField(
      element,
      protectedCredentialFieldPolicy,
    );
    const list = Array.from(attributes)
      .filter((attribute) => !protectedField || attribute.name.toLowerCase() !== "value")
      .map(makeReadonlyAttribute);
    defineHiddenReadonly(list, "constructor", undefined);
    defineHiddenReadonly(list, "getNamedItem", (name) => {
      const attributeName = String(name);
      if (protectedField && attributeName.toLowerCase() === "value") return null;
      const attribute = attributes.getNamedItem(attributeName);
      return attribute == null ? null : makeReadonlyAttribute(attribute);
    });
    defineHiddenReadonly(list, "item", (index) => list[index] ?? null);
    return Object.freeze(list);
  };

  const makeReadonlyHtml = (node, includeNode) => {
    const html = includeNode ? node.outerHTML : node.innerHTML;
    if (typeof html !== "string") return "";

    const isProtectedInputValue = (element) =>
      element.hasAttribute("value") &&
      isProtectedCredentialField(element, protectedCredentialFieldPolicy);
    if (
      !(includeNode && isProtectedInputValue(node)) &&
      !Array.from(node.querySelectorAll("input[value]")).some(isProtectedInputValue)
    ) return html;

    const template = node.ownerDocument.createElement("template");
    const nodes = includeNode ? [node] : Array.from(node.childNodes);
    for (const child of nodes)
      template.content.append(template.content.ownerDocument.importNode(child, true));
    for (const input of template.content.querySelectorAll("input[value]")) {
      if (isProtectedInputValue(input)) input.removeAttribute("value");
    }
    return template.innerHTML;
  };

  const makeReadonlyTokenList = (tokenList) => {
    if (tokenList == null) return undefined;
    const list = Array.from(tokenList, String);
    defineHiddenReadonly(list, "constructor", undefined);
    defineHiddenReadonly(list, "contains", (value) =>
      tokenList.contains(String(value)),
    );
    defineHiddenReadonly(list, "item", (index) => tokenList.item(index));
    defineHiddenReadonly(list, "value", tokenList.value);
    return Object.freeze(list);
  };

  const makeReadonlyTimeRanges = (ranges) => {
    if (ranges == null) return undefined;
    const list = [];
    for (let index = 0; index < ranges.length; index += 1) {
      list.push(Object.freeze({
        end: ranges.end(index),
        start: ranges.start(index),
      }));
    }
    defineHiddenReadonly(list, "constructor", undefined);
    defineHiddenReadonly(list, "end", (index) => ranges.end(Number(index)));
    defineHiddenReadonly(list, "item", (index) => list[index] ?? null);
    defineHiddenReadonly(list, "start", (index) => ranges.start(Number(index)));
    return Object.freeze(list);
  };

  const makeReadonlyTrackList = (tracks) => {
    if (tracks == null) return undefined;
    const list = Array.from(tracks, (track) =>
      Object.freeze({
        enabled: track.enabled === true,
        id: typeof track.id === "string" ? track.id : "",
        kind: typeof track.kind === "string" ? track.kind : "",
        label: typeof track.label === "string" ? track.label : "",
        language: typeof track.language === "string" ? track.language : "",
        mode: typeof track.mode === "string" ? track.mode : undefined,
        selected: track.selected === true,
      }),
    );
    defineHiddenReadonly(list, "constructor", undefined);
    defineHiddenReadonly(list, "getTrackById", (id) =>
      list.find((track) => track.id === String(id)) ?? null,
    );
    defineHiddenReadonly(list, "item", (index) => list[index] ?? null);
    return Object.freeze(list);
  };

  const makeReadonlyDataset = (dataset) => {
    const output = Object.create(null);
    for (const key of Object.keys(dataset)) {
      defineReadonly(output, key, dataset[key]);
    }
    defineHiddenReadonly(output, "constructor", undefined);
    return Object.freeze(output);
  };

  const makeReadonlyRect = (rect) =>
    Object.freeze({
      bottom: rect.bottom,
      height: rect.height,
      left: rect.left,
      right: rect.right,
      top: rect.top,
      width: rect.width,
      x: rect.x,
      y: rect.y,
      toJSON() {
        return {
          bottom: rect.bottom,
          height: rect.height,
          left: rect.left,
          right: rect.right,
          top: rect.top,
          width: rect.width,
          x: rect.x,
          y: rect.y,
        };
      },
    });

  const makeReadonlyValidityState = (validity) => {
    if (validity == null) return undefined;
    return Object.freeze({
      badInput: validity.badInput === true,
      customError: validity.customError === true,
      patternMismatch: validity.patternMismatch === true,
      rangeOverflow: validity.rangeOverflow === true,
      rangeUnderflow: validity.rangeUnderflow === true,
      stepMismatch: validity.stepMismatch === true,
      tooLong: validity.tooLong === true,
      tooShort: validity.tooShort === true,
      typeMismatch: validity.typeMismatch === true,
      valid: validity.valid === true,
      valueMissing: validity.valueMissing === true,
    });
  };

  const makeNodeSummary = (node) => {
    const wrapper = wrapNode(node);
    return wrapper == null ? null : wrapper.toJSON();
  };

  class ReadonlyStyleDeclaration {
    #style;

    constructor(style, token) {
      if (token !== wrapperToken) throw readonlyError("CSSStyleDeclaration constructor");
      this.#style = style;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get length() {
      return this.#style.length;
    }

    get cssText() {
      return this.#style.cssText ?? "";
    }

    getPropertyValue(name) {
      return this.#style.getPropertyValue(String(name));
    }

    getPropertyPriority(name) {
      return this.#style.getPropertyPriority(String(name));
    }

    item(index) {
      return this.#style.item(Number(index)) ?? "";
    }

    *[Symbol.iterator]() {
      for (let index = 0; index < this.#style.length; index += 1) {
        yield this.item(index);
      }
    }

    toJSON() {
      const output = {};
      const length = Math.min(this.#style.length, RESULT_MAX_OBJECT_KEYS);
      for (let index = 0; index < length; index += 1) {
        const name = this.#style.item(index);
        output[name] = this.#style.getPropertyValue(name);
      }
      return output;
    }
  }

  const wrapStyle = (style) => {
    const wrapper = new Proxy(new ReadonlyStyleDeclaration(style, wrapperToken), {
      get(target, property, receiver) {
        if (property === Symbol.toStringTag) return "CSSStyleDeclaration";
        if (property === Symbol.iterator)
          return target[Symbol.iterator].bind(target);
        if (typeof property === "string") {
          if (/^\\d+$/.test(property)) return target.item(Number(property));
          if (property in target) {
            const value = Reflect.get(target, property, target);
            return typeof value === "function" ? value.bind(target) : value;
          }
          if (typeof style[property] === "function") return undefined;
          return (
            target.getPropertyValue(camelToHyphen(property)) ||
            target.getPropertyValue(property)
          );
        }
        return Reflect.get(target, property, receiver);
      },
      set() {
        throw readonlyError("CSSStyleDeclaration assignment");
      },
    });
    styleWrappers.add(wrapper);
    return wrapper;
  };

  // CSSOM wrappers expose stylesheet and rule reads without exposing mutation
  // methods like insertRule(), deleteRule(), replace(), or setProperty().
  // Inaccessible cssRules keep native browser behavior when read directly; the
  // serializer records that state without trying a network fallback.
  class ReadonlyMediaList {
    #media;

    constructor(media, token) {
      if (token !== wrapperToken) throw readonlyError("MediaList constructor");
      this.#media = media;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get length() {
      return this.#media.length;
    }

    get mediaText() {
      return this.#media.mediaText;
    }

    item(index) {
      return this.#media.item(Number(index)) ?? "";
    }

    *[Symbol.iterator]() {
      for (let index = 0; index < this.#media.length; index += 1) {
        yield this.item(index);
      }
    }

    toJSON() {
      return Array.from(this);
    }
  }

  const wrapMediaList = (media) => {
    if (media == null) return null;
    const existing = mediaListWrappers.get(media);
    if (existing != null) return existing;
    const wrapper = new ReadonlyMediaList(media, wrapperToken);
    mediaListWrappers.set(media, wrapper);
    cssWrappers.add(wrapper);
    return wrapper;
  };

  class ReadonlyCSSRule {
    #rule;

    constructor(rule, token) {
      if (token !== wrapperToken) throw readonlyError("CSSRule constructor");
      this.#rule = rule;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get conditionText() {
      return typeof this.#rule.conditionText === "string"
        ? this.#rule.conditionText
        : undefined;
    }

    get cssRules() {
      if (!("cssRules" in this.#rule)) return undefined;
      return makeReadonlyList(Array.from(this.#rule.cssRules, wrapCSSRule));
    }

    get cssText() {
      return this.#rule.cssText;
    }

    get href() {
      return typeof this.#rule.href === "string" ? this.#rule.href : undefined;
    }

    get keyText() {
      return typeof this.#rule.keyText === "string" ? this.#rule.keyText : undefined;
    }

    get media() {
      return "media" in this.#rule ? wrapMediaList(this.#rule.media) : undefined;
    }

    get name() {
      return typeof this.#rule.name === "string" ? this.#rule.name : undefined;
    }

    get parentRule() {
      return wrapCSSRule(this.#rule.parentRule);
    }

    get parentStyleSheet() {
      return wrapCSSStyleSheet(this.#rule.parentStyleSheet);
    }

    get selectorText() {
      return typeof this.#rule.selectorText === "string"
        ? this.#rule.selectorText
        : undefined;
    }

    get style() {
      return this.#rule.style == null ? undefined : wrapStyle(this.#rule.style);
    }

    get styleSheet() {
      return wrapCSSStyleSheet(this.#rule.styleSheet);
    }

    get type() {
      return this.#rule.type;
    }

    toJSON() {
      const output = {
        cssText: this.cssText,
        type: this.type,
      };
      for (const name of [
        "conditionText",
        "href",
        "keyText",
        "name",
        "selectorText",
      ]) {
        const value = this[name];
        if (value !== undefined) output[name] = value;
      }
      if (this.style !== undefined) output.style = this.style.toJSON();
      return output;
    }
  }

  const wrapCSSRule = (rule) => {
    if (rule == null) return null;
    const existing = cssRuleWrappers.get(rule);
    if (existing != null) return existing;
    const wrapper = new ReadonlyCSSRule(rule, wrapperToken);
    cssRuleWrappers.set(rule, wrapper);
    cssWrappers.add(wrapper);
    return wrapper;
  };

  class ReadonlyCSSStyleSheet {
    #sheet;

    constructor(sheet, token) {
      if (token !== wrapperToken) throw readonlyError("CSSStyleSheet constructor");
      this.#sheet = sheet;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get cssRules() {
      return makeReadonlyList(Array.from(this.#sheet.cssRules, wrapCSSRule));
    }

    get disabled() {
      return this.#sheet.disabled === true;
    }

    get href() {
      return this.#sheet.href;
    }

    get media() {
      return wrapMediaList(this.#sheet.media);
    }

    get ownerNode() {
      return wrapNode(this.#sheet.ownerNode);
    }

    get ownerRule() {
      return wrapCSSRule(this.#sheet.ownerRule);
    }

    get parentStyleSheet() {
      return wrapCSSStyleSheet(this.#sheet.parentStyleSheet);
    }

    get rules() {
      return this.cssRules;
    }

    get title() {
      return this.#sheet.title;
    }

    get type() {
      return this.#sheet.type;
    }

    toJSON() {
      const output = {
        disabled: this.disabled,
        href: this.href,
        media: this.media?.toJSON() ?? null,
        title: this.title,
        type: this.type,
      };
      try {
        output.ruleCount = this.cssRules.length;
      } catch (error) {
        output.ruleAccessError =
          error instanceof Error ? error.name || error.message : String(error);
      }
      return output;
    }
  }

  const wrapCSSStyleSheet = (sheet) => {
    if (sheet == null) return null;
    const existing = styleSheetWrappers.get(sheet);
    if (existing != null) return existing;
    const wrapper = new ReadonlyCSSStyleSheet(sheet, wrapperToken);
    styleSheetWrappers.set(sheet, wrapper);
    cssWrappers.add(wrapper);
    return wrapper;
  };

  const unwrapRange = (value) =>
    value != null && (typeof value === "object" || typeof value === "function")
      ? rawRangesByWrapper.get(value) ?? null
      : null;

  class ReadonlyRange {
    #range;

    constructor(range, token) {
      if (token !== wrapperToken) throw readonlyError("Range constructor");
      this.#range = range;
      rawRangesByWrapper.set(this, range);
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get collapsed() {
      return this.#range.collapsed === true;
    }

    get commonAncestorContainer() {
      return wrapNode(this.#range.commonAncestorContainer);
    }

    get endContainer() {
      return wrapNode(this.#range.endContainer);
    }

    get endOffset() {
      return this.#range.endOffset;
    }

    get startContainer() {
      return wrapNode(this.#range.startContainer);
    }

    get startOffset() {
      return this.#range.startOffset;
    }

    compareBoundaryPoints(how, sourceRange) {
      const rawRange = unwrapRange(sourceRange);
      if (rawRange == null) throw new TypeError("compareBoundaryPoints expects a Range");
      return this.#range.compareBoundaryPoints(Number(how), rawRange);
    }

    comparePoint(node, offset) {
      const rawNode = unwrapNode(node);
      if (rawNode == null) throw new TypeError("comparePoint expects a Node");
      return this.#range.comparePoint(rawNode, Number(offset));
    }

    getBoundingClientRect() {
      return makeReadonlyRect(this.#range.getBoundingClientRect());
    }

    getClientRects() {
      return makeReadonlyList(
        Array.from(this.#range.getClientRects(), makeReadonlyRect),
      );
    }

    intersectsNode(node) {
      const rawNode = unwrapNode(node);
      return rawNode != null && this.#range.intersectsNode(rawNode);
    }

    isPointInRange(node, offset) {
      const rawNode = unwrapNode(node);
      return rawNode != null && this.#range.isPointInRange(rawNode, Number(offset));
    }

    selectNodeContents(node) {
      const rawNode = unwrapNode(node);
      if (rawNode == null) throw new TypeError("selectNodeContents expects a Node");
      this.#range.selectNodeContents(rawNode);
    }

    setEnd(node, offset) {
      const rawNode = unwrapNode(node);
      if (rawNode == null) throw new TypeError("setEnd expects a Node");
      this.#range.setEnd(rawNode, Number(offset));
    }

    setStart(node, offset) {
      const rawNode = unwrapNode(node);
      if (rawNode == null) throw new TypeError("setStart expects a Node");
      this.#range.setStart(rawNode, Number(offset));
    }

    toJSON() {
      return {
        collapsed: this.collapsed,
        commonAncestorContainer: makeNodeSummary(this.#range.commonAncestorContainer),
        endOffset: this.endOffset,
        startOffset: this.startOffset,
        text: this.toString(),
      };
    }

    toString() {
      return this.#range.toString();
    }
  }

  const wrapRange = (range) => {
    if (range == null) return null;
    const existing = rangeWrappers.get(range);
    if (existing != null) return existing;
    const wrapper = new ReadonlyRange(range, wrapperToken);
    rangeWrappers.set(range, wrapper);
    selectionAndRangeWrappers.add(wrapper);
    return wrapper;
  };

  class ReadonlySelection {
    #selection;

    constructor(selection, token) {
      if (token !== wrapperToken) throw readonlyError("Selection constructor");
      this.#selection = selection;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get anchorNode() {
      return wrapNode(this.#selection.anchorNode);
    }

    get anchorOffset() {
      return this.#selection.anchorOffset;
    }

    get focusNode() {
      return wrapNode(this.#selection.focusNode);
    }

    get focusOffset() {
      return this.#selection.focusOffset;
    }

    get isCollapsed() {
      return this.#selection.isCollapsed === true;
    }

    get rangeCount() {
      return this.#selection.rangeCount;
    }

    get type() {
      return this.#selection.type;
    }

    containsNode(node, allowPartialContainment = false) {
      const rawNode = unwrapNode(node);
      return (
        rawNode != null &&
        this.#selection.containsNode(rawNode, Boolean(allowPartialContainment))
      );
    }

    getRangeAt(index) {
      return wrapRange(this.#selection.getRangeAt(Number(index)).cloneRange());
    }

    toJSON() {
      return {
        anchorNode: makeNodeSummary(this.#selection.anchorNode),
        anchorOffset: this.anchorOffset,
        focusNode: makeNodeSummary(this.#selection.focusNode),
        focusOffset: this.focusOffset,
        isCollapsed: this.isCollapsed,
        rangeCount: this.rangeCount,
        text: this.toString(),
        type: this.type,
      };
    }

    toString() {
      return this.#selection.toString();
    }
  }

  const wrapSelection = (selection) => {
    if (selection == null) return null;
    const existing = selectionWrappers.get(selection);
    if (existing != null) return existing;
    const wrapper = new ReadonlySelection(selection, wrapperToken);
    selectionWrappers.set(selection, wrapper);
    selectionAndRangeWrappers.add(wrapper);
    return wrapper;
  };

  class ReadonlyFontFaceSet {
    #fonts;

    constructor(fonts, token) {
      if (token !== wrapperToken) throw readonlyError("FontFaceSet constructor");
      this.#fonts = fonts;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get size() {
      return this.#fonts.size;
    }

    get status() {
      return this.#fonts.status;
    }

    check(font, text) {
      return this.#fonts.check(
        String(font),
        text === undefined ? undefined : String(text),
      );
    }

    toJSON() {
      return {
        size: this.size,
        status: this.status,
      };
    }
  }

  const wrapFontFaceSet = (fonts) => {
    if (fonts == null) return undefined;
    const existing = fontFaceSetWrappers.get(fonts);
    if (existing != null) return existing;
    const wrapper = new ReadonlyFontFaceSet(fonts, wrapperToken);
    fontFaceSetWrappers.set(fonts, wrapper);
    readonlyApiWrappers.add(wrapper);
    return wrapper;
  };

  class ReadonlyMediaQueryList {
    #query;

    constructor(query, token) {
      if (token !== wrapperToken) throw readonlyError("MediaQueryList constructor");
      this.#query = query;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get matches() {
      return this.#query.matches === true;
    }

    get media() {
      return this.#query.media;
    }

    toJSON() {
      return {
        matches: this.matches,
        media: this.media,
      };
    }
  }

  const wrapMediaQueryList = (query) => {
    if (query == null) return null;
    const existing = mediaQueryListWrappers.get(query);
    if (existing != null) return existing;
    const wrapper = new ReadonlyMediaQueryList(query, wrapperToken);
    mediaQueryListWrappers.set(query, wrapper);
    readonlyApiWrappers.add(wrapper);
    return wrapper;
  };

  class ReadonlyVisualViewport {
    #viewport;

    constructor(viewport, token) {
      if (token !== wrapperToken) throw readonlyError("VisualViewport constructor");
      this.#viewport = viewport;
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get height() {
      return this.#viewport.height;
    }

    get offsetLeft() {
      return this.#viewport.offsetLeft;
    }

    get offsetTop() {
      return this.#viewport.offsetTop;
    }

    get pageLeft() {
      return this.#viewport.pageLeft;
    }

    get pageTop() {
      return this.#viewport.pageTop;
    }

    get scale() {
      return this.#viewport.scale;
    }

    get width() {
      return this.#viewport.width;
    }

    toJSON() {
      return {
        height: this.height,
        offsetLeft: this.offsetLeft,
        offsetTop: this.offsetTop,
        pageLeft: this.pageLeft,
        pageTop: this.pageTop,
        scale: this.scale,
        width: this.width,
      };
    }
  }

  const wrapVisualViewport = (viewport) => {
    if (viewport == null) return null;
    const wrapper = new ReadonlyVisualViewport(viewport, wrapperToken);
    readonlyApiWrappers.add(wrapper);
    return wrapper;
  };

  class ReadonlyLocation {
    constructor() {
      defineHiddenReadonly(this, "constructor", undefined);
      Object.freeze(this);
    }

    get hash() {
      return rawWindow.location.hash;
    }

    get host() {
      return rawWindow.location.host;
    }

    get hostname() {
      return rawWindow.location.hostname;
    }

    get href() {
      return rawWindow.location.href;
    }

    get origin() {
      return rawWindow.location.origin;
    }

    get pathname() {
      return rawWindow.location.pathname;
    }

    get port() {
      return rawWindow.location.port;
    }

    get protocol() {
      return rawWindow.location.protocol;
    }

    get search() {
      return rawWindow.location.search;
    }

    toJSON() {
      return this.href;
    }

    toString() {
      return this.href;
    }
  }

  class ReadonlyNode {
    #node;

    constructor(node, token) {
      if (token !== wrapperToken) throw readonlyError("Node constructor");
      this.#node = node;
      rawNodesByWrapper.set(this, node);
      defineHiddenReadonly(this, "constructor", undefined);
      if (new.target === ReadonlyNode) Object.freeze(this);
    }

    get childNodes() {
      return makeReadonlyNodeList(this.#node.childNodes);
    }

    get firstChild() {
      return wrapNode(this.#node.firstChild);
    }

    get lastChild() {
      return wrapNode(this.#node.lastChild);
    }

    get nextSibling() {
      return wrapNode(this.#node.nextSibling);
    }

    get nodeName() {
      return this.#node.nodeName;
    }

    get name() {
      return typeof this.#node.name === "string" ? this.#node.name : undefined;
    }

    get nodeType() {
      return this.#node.nodeType;
    }

    get nodeValue() {
      return this.#node.nodeValue;
    }

    get publicId() {
      return typeof this.#node.publicId === "string" ? this.#node.publicId : undefined;
    }

    get systemId() {
      return typeof this.#node.systemId === "string" ? this.#node.systemId : undefined;
    }

    get isConnected() {
      return this.#node.isConnected === true;
    }

    get ownerDocument() {
      return wrapNode(this.#node.ownerDocument ?? rawDocument);
    }

    get parentElement() {
      return wrapNode(this.#node.parentElement);
    }

    get parentNode() {
      return wrapNode(this.#node.parentNode);
    }

    get previousSibling() {
      return wrapNode(this.#node.previousSibling);
    }

    get textContent() {
      return this.#node.textContent;
    }

    contains(node) {
      const rawNode = unwrapNode(node);
      return rawNode != null && this.#node.contains(rawNode);
    }

    getRootNode(options) {
      return wrapNode(
        typeof this.#node.getRootNode === "function"
          ? this.#node.getRootNode(options)
          : (this.#node.ownerDocument ?? rawDocument),
      );
    }

    toJSON() {
      return {
        name: this.name,
        nodeName: this.nodeName,
        nodeType: this.nodeType,
        publicId: this.publicId,
        systemId: this.systemId,
        textContent: this.textContent,
      };
    }
  }

  class ReadonlyElement extends ReadonlyNode {
    #element;

    constructor(element, token) {
      super(element, token);
      this.#element = element;
      Object.freeze(this);
    }

    get attributes() {
      return makeReadonlyAttributeList(this.#element);
    }

    get alt() {
      return typeof this.#element.alt === "string" ? this.#element.alt : "";
    }

    get ariaChecked() {
      return typeof this.#element.ariaChecked === "string"
        ? this.#element.ariaChecked
        : null;
    }

    get ariaControls() {
      return typeof this.#element.ariaControls === "string"
        ? this.#element.ariaControls
        : null;
    }

    get ariaCurrent() {
      return typeof this.#element.ariaCurrent === "string"
        ? this.#element.ariaCurrent
        : null;
    }

    get ariaDescribedBy() {
      return typeof this.#element.ariaDescribedBy === "string"
        ? this.#element.ariaDescribedBy
        : null;
    }

    get ariaDescription() {
      return typeof this.#element.ariaDescription === "string"
        ? this.#element.ariaDescription
        : null;
    }

    get ariaDisabled() {
      return typeof this.#element.ariaDisabled === "string"
        ? this.#element.ariaDisabled
        : null;
    }

    get ariaExpanded() {
      return typeof this.#element.ariaExpanded === "string"
        ? this.#element.ariaExpanded
        : null;
    }

    get ariaHasPopup() {
      return typeof this.#element.ariaHasPopup === "string"
        ? this.#element.ariaHasPopup
        : null;
    }

    get ariaHidden() {
      return typeof this.#element.ariaHidden === "string"
        ? this.#element.ariaHidden
        : null;
    }

    get ariaInvalid() {
      return typeof this.#element.ariaInvalid === "string"
        ? this.#element.ariaInvalid
        : null;
    }

    get action() {
      return typeof this.#element.action === "string" ? this.#element.action : "";
    }

    get allow() {
      return typeof this.#element.allow === "string" ? this.#element.allow : "";
    }

    get ariaLabel() {
      return typeof this.#element.ariaLabel === "string"
        ? this.#element.ariaLabel
        : null;
    }

    get ariaLabelledBy() {
      return typeof this.#element.ariaLabelledBy === "string"
        ? this.#element.ariaLabelledBy
        : null;
    }

    get ariaPressed() {
      return typeof this.#element.ariaPressed === "string"
        ? this.#element.ariaPressed
        : null;
    }

    get ariaSelected() {
      return typeof this.#element.ariaSelected === "string"
        ? this.#element.ariaSelected
        : null;
    }

    get ariaSort() {
      return typeof this.#element.ariaSort === "string"
        ? this.#element.ariaSort
        : null;
    }

    get ariaValueMax() {
      return typeof this.#element.ariaValueMax === "string"
        ? this.#element.ariaValueMax
        : null;
    }

    get ariaValueMin() {
      return typeof this.#element.ariaValueMin === "string"
        ? this.#element.ariaValueMin
        : null;
    }

    get ariaValueNow() {
      return typeof this.#element.ariaValueNow === "string"
        ? this.#element.ariaValueNow
        : null;
    }

    get ariaValueText() {
      return typeof this.#element.ariaValueText === "string"
        ? this.#element.ariaValueText
        : null;
    }

    get as() {
      return typeof this.#element.as === "string" ? this.#element.as : "";
    }

    get assignedElements() {
      if (typeof this.#element.assignedElements !== "function") return undefined;
      return (options = {}) =>
        makeReadonlyList(
          Array.from(
            this.#element.assignedElements({ flatten: options.flatten === true }),
            wrapNode,
          ),
        );
    }

    get assignedNodes() {
      if (typeof this.#element.assignedNodes !== "function") return undefined;
      return (options = {}) =>
        makeReadonlyList(
          Array.from(
            this.#element.assignedNodes({ flatten: options.flatten === true }),
            wrapNode,
          ),
        );
    }

    get assignedSlot() {
      return wrapNode(this.#element.assignedSlot);
    }

    get async() {
      return this.#element.async === true;
    }

    get autoplay() {
      return this.#element.autoplay === true;
    }

    get autocomplete() {
      return typeof this.#element.autocomplete === "string"
        ? this.#element.autocomplete
        : "";
    }

    get buffered() {
      return makeReadonlyTimeRanges(this.#element.buffered);
    }

    get checked() {
      return this.#element.checked === true;
    }

    get children() {
      return makeReadonlyNodeList(this.#element.children);
    }

    get caption() {
      return wrapNode(this.#element.caption);
    }

    get cellIndex() {
      return this.#element.cellIndex ?? -1;
    }

    get cells() {
      return makeReadonlyNodeList(this.#element.cells);
    }

    get clientHeight() {
      return this.#element.clientHeight ?? 0;
    }

    get clientLeft() {
      return this.#element.clientLeft ?? 0;
    }

    get clientTop() {
      return this.#element.clientTop ?? 0;
    }

    get clientWidth() {
      return this.#element.clientWidth ?? 0;
    }

    get classList() {
      return makeReadonlyTokenList(this.#element.classList);
    }

    get className() {
      const className = this.#element.className;
      if (typeof className === "string") return className;
      if (typeof className?.baseVal === "string") return className.baseVal;
      return "";
    }

    get colSpan() {
      return this.#element.colSpan ?? 1;
    }

    get content() {
      const content = this.#element.content;
      if (content === undefined) return undefined;
      return content != null && typeof content === "object" && "nodeType" in content
        ? wrapNode(content)
        : content;
    }

    get controls() {
      return this.#element.controls === true;
    }

    get crossOrigin() {
      return typeof this.#element.crossOrigin === "string"
        ? this.#element.crossOrigin
        : null;
    }

    get currentTime() {
      return this.#element.currentTime ?? 0;
    }

    get dataset() {
      return makeReadonlyDataset(this.#element.dataset);
    }

    get decoding() {
      return typeof this.#element.decoding === "string"
        ? this.#element.decoding
        : "";
    }

    get defaultSelected() {
      return this.#element.defaultSelected === true;
    }

    get defaultValue() {
      if (isProtectedCredentialField(this.#element, protectedCredentialFieldPolicy))
        return "";
      return typeof this.#element.defaultValue === "string"
        ? this.#element.defaultValue
        : "";
    }

    get defer() {
      return this.#element.defer === true;
    }

    get defaultMuted() {
      return this.#element.defaultMuted === true;
    }

    get defaultPlaybackRate() {
      return this.#element.defaultPlaybackRate ?? 1;
    }

    get disabled() {
      return this.#element.disabled === true;
    }

    get dir() {
      return typeof this.#element.dir === "string" ? this.#element.dir : "";
    }

    get download() {
      return typeof this.#element.download === "string"
        ? this.#element.download
        : "";
    }

    get complete() {
      return this.#element.complete === true;
    }

    get currentSrc() {
      return typeof this.#element.currentSrc === "string" ? this.#element.currentSrc : "";
    }

    get duration() {
      return this.#element.duration ?? 0;
    }

    get elements() {
      return makeReadonlyNodeList(this.#element.elements);
    }

    get ended() {
      return this.#element.ended === true;
    }

    get enctype() {
      return typeof this.#element.enctype === "string"
        ? this.#element.enctype
        : "";
    }

    get exportparts() {
      return typeof this.#element.exportparts === "string"
        ? this.#element.exportparts
        : "";
    }

    get firstElementChild() {
      return wrapNode(this.#element.firstElementChild);
    }

    get form() {
      return wrapNode(this.#element.form);
    }

    get formAction() {
      return typeof this.#element.formAction === "string"
        ? this.#element.formAction
        : "";
    }

    get formEnctype() {
      return typeof this.#element.formEnctype === "string"
        ? this.#element.formEnctype
        : "";
    }

    get formMethod() {
      return typeof this.#element.formMethod === "string"
        ? this.#element.formMethod
        : "";
    }

    get formNoValidate() {
      return this.#element.formNoValidate === true;
    }

    get formTarget() {
      return typeof this.#element.formTarget === "string"
        ? this.#element.formTarget
        : "";
    }

    get height() {
      return this.#element.height ?? 0;
    }

    get hash() {
      return typeof this.#element.hash === "string" ? this.#element.hash : "";
    }

    get hidden() {
      return this.#element.hidden === true;
    }

    get fetchPriority() {
      return typeof this.#element.fetchPriority === "string"
        ? this.#element.fetchPriority
        : "";
    }

    get host() {
      return typeof this.#element.host === "string" ? this.#element.host : "";
    }

    get hostname() {
      return typeof this.#element.hostname === "string"
        ? this.#element.hostname
        : "";
    }

    get href() {
      return typeof this.#element.href === "string" ? this.#element.href : "";
    }

    get hreflang() {
      return typeof this.#element.hreflang === "string"
        ? this.#element.hreflang
        : "";
    }

    get httpEquiv() {
      return typeof this.#element.httpEquiv === "string"
        ? this.#element.httpEquiv
        : "";
    }

    get htmlFor() {
      return typeof this.#element.htmlFor === "string" ? this.#element.htmlFor : "";
    }

    get id() {
      return this.#element.id;
    }

    get index() {
      return this.#element.index ?? -1;
    }

    get innerHTML() {
      return makeReadonlyHtml(this.#element, false);
    }

    get innerText() {
      return this.#element.innerText;
    }

    get lastElementChild() {
      return wrapNode(this.#element.lastElementChild);
    }

    get labels() {
      return makeReadonlyNodeList(this.#element.labels);
    }

    get label() {
      return typeof this.#element.label === "string" ? this.#element.label : "";
    }

    get lang() {
      return typeof this.#element.lang === "string" ? this.#element.lang : "";
    }

    get length() {
      return this.#element.length ?? 0;
    }

    get loading() {
      return typeof this.#element.loading === "string"
        ? this.#element.loading
        : "";
    }

    get loop() {
      return this.#element.loop === true;
    }

    get list() {
      return wrapNode(this.#element.list);
    }

    get localName() {
      return this.#element.localName;
    }

    get maxLength() {
      return this.#element.maxLength ?? -1;
    }

    get minLength() {
      return this.#element.minLength ?? -1;
    }

    get media() {
      return typeof this.#element.media === "string" ? this.#element.media : "";
    }

    get max() {
      return typeof this.#element.max === "string" ? this.#element.max : "";
    }

    get method() {
      return typeof this.#element.method === "string" ? this.#element.method : "";
    }

    get min() {
      return typeof this.#element.min === "string" ? this.#element.min : "";
    }

    get multiple() {
      return this.#element.multiple === true;
    }

    get muted() {
      return this.#element.muted === true;
    }

    get name() {
      return typeof this.#element.name === "string" ? this.#element.name : "";
    }

    get namespaceURI() {
      return this.#element.namespaceURI;
    }

    get noValidate() {
      return this.#element.noValidate === true;
    }

    get naturalHeight() {
      return this.#element.naturalHeight ?? 0;
    }

    get naturalWidth() {
      return this.#element.naturalWidth ?? 0;
    }

    get nextElementSibling() {
      return wrapNode(this.#element.nextElementSibling);
    }

    get outerHTML() {
      return makeReadonlyHtml(this.#element, true);
    }

    get offsetHeight() {
      return this.#element.offsetHeight ?? 0;
    }

    get offsetLeft() {
      return this.#element.offsetLeft ?? 0;
    }

    get offsetTop() {
      return this.#element.offsetTop ?? 0;
    }

    get offsetWidth() {
      return this.#element.offsetWidth ?? 0;
    }

    get options() {
      return makeReadonlyNodeList(this.#element.options);
    }

    get open() {
      return this.#element.open === true;
    }

    get origin() {
      return typeof this.#element.origin === "string" ? this.#element.origin : "";
    }

    get paused() {
      return this.#element.paused === true;
    }

    get part() {
      return makeReadonlyTokenList(this.#element.part);
    }

    get pathname() {
      return typeof this.#element.pathname === "string"
        ? this.#element.pathname
        : "";
    }

    get pattern() {
      return typeof this.#element.pattern === "string"
        ? this.#element.pattern
        : "";
    }

    get playbackRate() {
      return this.#element.playbackRate ?? 1;
    }

    get ping() {
      return typeof this.#element.ping === "string" ? this.#element.ping : "";
    }

    get played() {
      return makeReadonlyTimeRanges(this.#element.played);
    }

    get placeholder() {
      return typeof this.#element.placeholder === "string"
        ? this.#element.placeholder
        : "";
    }

    get port() {
      return typeof this.#element.port === "string" ? this.#element.port : "";
    }

    get poster() {
      return typeof this.#element.poster === "string" ? this.#element.poster : "";
    }

    get protocol() {
      return typeof this.#element.protocol === "string"
        ? this.#element.protocol
        : "";
    }

    get previousElementSibling() {
      return wrapNode(this.#element.previousElementSibling);
    }

    get readOnly() {
      return this.#element.readOnly === true;
    }

    get readyState() {
      return this.#element.readyState ?? 0;
    }

    get referrerPolicy() {
      return typeof this.#element.referrerPolicy === "string"
        ? this.#element.referrerPolicy
        : "";
    }

    get rel() {
      return typeof this.#element.rel === "string" ? this.#element.rel : "";
    }

    get relList() {
      return makeReadonlyTokenList(this.#element.relList);
    }

    get required() {
      return this.#element.required === true;
    }

    get role() {
      return typeof this.#element.role === "string" ? this.#element.role : null;
    }

    get sandbox() {
      return makeReadonlyTokenList(this.#element.sandbox);
    }

    get scrollHeight() {
      return this.#element.scrollHeight ?? 0;
    }

    get scrollLeft() {
      return this.#element.scrollLeft ?? 0;
    }

    get scrollTop() {
      return this.#element.scrollTop ?? 0;
    }

    get scrollWidth() {
      return this.#element.scrollWidth ?? 0;
    }

    get rowIndex() {
      return this.#element.rowIndex ?? -1;
    }

    get rows() {
      return makeReadonlyNodeList(this.#element.rows);
    }

    get rowSpan() {
      return this.#element.rowSpan ?? 1;
    }

    get sectionRowIndex() {
      return this.#element.sectionRowIndex ?? -1;
    }

    get selected() {
      return this.#element.selected === true;
    }

    get selectedOptions() {
      return makeReadonlyNodeList(this.#element.selectedOptions);
    }

    get selectedIndex() {
      return this.#element.selectedIndex ?? -1;
    }

    get selectionDirection() {
      return typeof this.#element.selectionDirection === "string"
        ? this.#element.selectionDirection
        : null;
    }

    get selectionEnd() {
      return this.#element.selectionEnd ?? null;
    }

    get selectionStart() {
      return this.#element.selectionStart ?? null;
    }

    get shadowRoot() {
      return wrapNode(this.#element.shadowRoot);
    }

    get search() {
      return typeof this.#element.search === "string" ? this.#element.search : "";
    }

    get seekable() {
      return makeReadonlyTimeRanges(this.#element.seekable);
    }

    get size() {
      return this.#element.size ?? 0;
    }

    get sizes() {
      return typeof this.#element.sizes === "string" ? this.#element.sizes : "";
    }

    get slot() {
      return typeof this.#element.slot === "string" ? this.#element.slot : "";
    }

    get src() {
      return typeof this.#element.src === "string" ? this.#element.src : "";
    }

    get srcdoc() {
      return typeof this.#element.srcdoc === "string" ? this.#element.srcdoc : "";
    }

    get srcset() {
      return typeof this.#element.srcset === "string" ? this.#element.srcset : "";
    }

    get step() {
      return typeof this.#element.step === "string" ? this.#element.step : "";
    }

    get style() {
      return wrapStyle(this.#element.style);
    }

    get tagName() {
      return this.#element.tagName;
    }

    get tabIndex() {
      return this.#element.tabIndex ?? -1;
    }

    get tBodies() {
      return makeReadonlyNodeList(this.#element.tBodies);
    }

    get tFoot() {
      return wrapNode(this.#element.tFoot);
    }

    get tHead() {
      return wrapNode(this.#element.tHead);
    }

    get target() {
      return typeof this.#element.target === "string" ? this.#element.target : "";
    }

    get text() {
      return typeof this.#element.text === "string"
        ? this.#element.text
        : this.textContent;
    }

    get textTracks() {
      return makeReadonlyTrackList(this.#element.textTracks);
    }

    get title() {
      return typeof this.#element.title === "string" ? this.#element.title : "";
    }

    get type() {
      return typeof this.#element.type === "string" ? this.#element.type : "";
    }

    get validationMessage() {
      if (isProtectedCredentialField(this.#element, protectedCredentialFieldPolicy))
        return "";
      return typeof this.#element.validationMessage === "string"
        ? this.#element.validationMessage
        : "";
    }

    get validity() {
      return makeReadonlyValidityState(this.#element.validity);
    }

    get value() {
      if (isProtectedCredentialField(this.#element, protectedCredentialFieldPolicy))
        return "";
      return typeof this.#element.value === "string" ? this.#element.value : "";
    }

    get valueAsNumber() {
      if (isProtectedCredentialField(this.#element, protectedCredentialFieldPolicy))
        return null;
      return this.#element.valueAsNumber ?? null;
    }

    get audioTracks() {
      return makeReadonlyTrackList(this.#element.audioTracks);
    }

    get videoHeight() {
      return this.#element.videoHeight ?? 0;
    }

    get videoTracks() {
      return makeReadonlyTrackList(this.#element.videoTracks);
    }

    get videoWidth() {
      return this.#element.videoWidth ?? 0;
    }

    get width() {
      return this.#element.width ?? 0;
    }

    get willValidate() {
      return this.#element.willValidate === true;
    }

    closest(selector) {
      return wrapNode(this.#element.closest(String(selector)));
    }

    getAttribute(name) {
      const attributeName = String(name);
      if (
        attributeName.toLowerCase() === "value" &&
        isProtectedCredentialField(this.#element, protectedCredentialFieldPolicy)
      )
        return null;
      return this.#element.getAttribute(attributeName);
    }

    getBoundingClientRect() {
      return makeReadonlyRect(this.#element.getBoundingClientRect());
    }

    getClientRects() {
      return makeReadonlyList(
        Array.from(this.#element.getClientRects(), makeReadonlyRect),
      );
    }

    getElementsByClassName(className) {
      return makeReadonlyNodeList(
        this.#element.getElementsByClassName(String(className)),
      );
    }

    getElementsByTagName(tagName) {
      return makeReadonlyNodeList(
        this.#element.getElementsByTagName(String(tagName)),
      );
    }

    hasAttribute(name) {
      return this.#element.hasAttribute(String(name));
    }

    matches(selector) {
      return this.#element.matches(String(selector));
    }

    querySelector(selector) {
      return wrapNode(this.#element.querySelector(String(selector)));
    }

    querySelectorAll(selector) {
      return makeReadonlyNodeList(
        this.#element.querySelectorAll(String(selector)),
      );
    }

    scroll(leftOrOptions, top) {
      this.scrollTo(leftOrOptions, top);
    }

    scrollBy(leftOrOptions, top) {
      if (typeof this.#element.scrollBy !== "function") {
        return;
      }
      if (
        leftOrOptions != null &&
        typeof leftOrOptions === "object" &&
        !Array.isArray(leftOrOptions)
      ) {
        this.#element.scrollBy({ ...leftOrOptions });
        return;
      }
      this.#element.scrollBy(Number(leftOrOptions ?? 0), Number(top ?? 0));
    }

    scrollIntoView(arg) {
      if (typeof this.#element.scrollIntoView !== "function") {
        return;
      }
      if (arg == null || typeof arg === "boolean") {
        this.#element.scrollIntoView(arg);
        return;
      }
      if (typeof arg === "object" && !Array.isArray(arg)) {
        this.#element.scrollIntoView({ ...arg });
        return;
      }
      this.#element.scrollIntoView(Boolean(arg));
    }

    scrollTo(leftOrOptions, top) {
      if (typeof this.#element.scrollTo !== "function") {
        return;
      }
      if (
        leftOrOptions != null &&
        typeof leftOrOptions === "object" &&
        !Array.isArray(leftOrOptions)
      ) {
        this.#element.scrollTo({ ...leftOrOptions });
        return;
      }
      this.#element.scrollTo(Number(leftOrOptions ?? 0), Number(top ?? 0));
    }

    toJSON() {
      return {
        id: this.id,
        tagName: this.tagName,
        textContent: this.textContent,
      };
    }
  }

  class ReadonlyDocumentFragment extends ReadonlyNode {
    #fragment;

    constructor(fragment, token) {
      super(fragment, token);
      this.#fragment = fragment;
      Object.freeze(this);
    }

    get activeElement() {
      return wrapNode(this.#fragment.activeElement);
    }

    get adoptedStyleSheets() {
      return makeReadonlyList(
        Array.from(this.#fragment.adoptedStyleSheets ?? [], wrapCSSStyleSheet),
      );
    }

    get childElementCount() {
      return this.#fragment.childElementCount ?? this.children.length;
    }

    get children() {
      return makeReadonlyNodeList(this.#fragment.children);
    }

    get firstElementChild() {
      return wrapNode(this.#fragment.firstElementChild);
    }

    get host() {
      return wrapNode(this.#fragment.host);
    }

    get innerHTML() {
      return makeReadonlyHtml(this.#fragment, false);
    }

    get lastElementChild() {
      return wrapNode(this.#fragment.lastElementChild);
    }

    get mode() {
      return typeof this.#fragment.mode === "string"
        ? this.#fragment.mode
        : undefined;
    }

    get styleSheets() {
      return makeReadonlyList(
        Array.from(this.#fragment.styleSheets ?? [], wrapCSSStyleSheet),
      );
    }

    getElementById(id) {
      return typeof this.#fragment.getElementById === "function"
        ? wrapNode(this.#fragment.getElementById(String(id)))
        : null;
    }

    querySelector(selector) {
      return typeof this.#fragment.querySelector === "function"
        ? wrapNode(this.#fragment.querySelector(String(selector)))
        : null;
    }

    querySelectorAll(selector) {
      return makeReadonlyNodeList(
        typeof this.#fragment.querySelectorAll === "function"
          ? this.#fragment.querySelectorAll(String(selector))
          : [],
      );
    }
  }

  class ReadonlyDocument extends ReadonlyNode {
    #document;

    constructor(document, token) {
      super(document, token);
      this.#document = document;
      Object.freeze(this);
    }

    get activeElement() {
      return wrapNode(this.#document.activeElement);
    }

    get body() {
      return wrapNode(this.#document.body);
    }

    get characterSet() {
      return this.#document.characterSet;
    }

    get cookie() {
      return undefined;
    }

    get contentType() {
      return this.#document.contentType;
    }

    get defaultView() {
      return windowObject;
    }

    get adoptedStyleSheets() {
      return makeReadonlyList(
        Array.from(this.#document.adoptedStyleSheets ?? [], wrapCSSStyleSheet),
      );
    }

    get compatMode() {
      return this.#document.compatMode;
    }

    get doctype() {
      return wrapNode(this.#document.doctype);
    }

    get documentElement() {
      return wrapNode(this.#document.documentElement);
    }

    get fonts() {
      return wrapFontFaceSet(this.#document.fonts);
    }

    get forms() {
      return makeReadonlyNodeList(this.#document.forms);
    }

    get head() {
      return wrapNode(this.#document.head);
    }

    get hidden() {
      return this.#document.hidden === true;
    }

    get images() {
      return makeReadonlyNodeList(this.#document.images);
    }

    get lastModified() {
      return this.#document.lastModified;
    }

    get links() {
      return makeReadonlyNodeList(this.#document.links);
    }

    get location() {
      return locationObject;
    }

    get readyState() {
      return this.#document.readyState;
    }

    get referrer() {
      return this.#document.referrer;
    }

    get scripts() {
      return makeReadonlyNodeList(this.#document.scripts);
    }

    get scrollingElement() {
      return wrapNode(this.#document.scrollingElement);
    }

    get styleSheets() {
      return makeReadonlyList(
        Array.from(this.#document.styleSheets ?? [], wrapCSSStyleSheet),
      );
    }

    get title() {
      return this.#document.title;
    }

    get URL() {
      return this.#document.URL;
    }

    get visibilityState() {
      return this.#document.visibilityState;
    }

    elementFromPoint(x, y) {
      return typeof this.#document.elementFromPoint === "function"
        ? wrapNode(this.#document.elementFromPoint(Number(x), Number(y)))
        : null;
    }

    elementsFromPoint(x, y) {
      return makeReadonlyNodeList(
        typeof this.#document.elementsFromPoint === "function"
          ? this.#document.elementsFromPoint(Number(x), Number(y))
          : [],
      );
    }

    createRange() {
      return wrapRange(this.#document.createRange());
    }

    getElementById(id) {
      return wrapNode(this.#document.getElementById(String(id)));
    }

    getElementsByClassName(className) {
      return makeReadonlyNodeList(
        this.#document.getElementsByClassName(String(className)),
      );
    }

    getElementsByName(name) {
      return makeReadonlyNodeList(
        this.#document.getElementsByName(String(name)),
      );
    }

    getElementsByTagName(tagName) {
      return makeReadonlyNodeList(this.#document.getElementsByTagName(String(tagName)));
    }

    querySelector(selector) {
      return wrapNode(this.#document.querySelector(String(selector)));
    }

    querySelectorAll(selector) {
      return makeReadonlyNodeList(
        this.#document.querySelectorAll(String(selector)),
      );
    }
  }

  // Raw DOM references are closure-owned, not methods on exposed wrappers.
  // That keeps helper APIs like contains() and getComputedStyle() usable without
  // adding a model-visible path back to the mutable DOM.
  const unwrapNode = (value) =>
    value != null && (typeof value === "object" || typeof value === "function")
      ? rawNodesByWrapper.get(value) ?? null
      : null;
  const unwrapElement = (value) => {
    const node = unwrapNode(value);
    return node?.nodeType === ELEMENT_NODE ? node : null;
  };

  const isElement = (node) => node?.nodeType === ELEMENT_NODE;
  const isDocument = (node) => node?.nodeType === DOCUMENT_NODE;
  const isDocumentFragment = (node) => node?.nodeType === DOCUMENT_FRAGMENT_NODE;

  const wrapNode = (node) => {
    if (node == null) return null;
    const existing = nodeWrappers.get(node);
    if (existing != null) return existing;
    const wrapper = isDocument(node)
      ? new ReadonlyDocument(node, wrapperToken)
      : isElement(node)
        ? new ReadonlyElement(node, wrapperToken)
        : isDocumentFragment(node)
          ? new ReadonlyDocumentFragment(node, wrapperToken)
          : new ReadonlyNode(node, wrapperToken);
    nodeWrappers.set(node, wrapper);
    return wrapper;
  };

  const wrapNodeCollectionResult = (value) => {
    if (value == null) return null;
    if (typeof value.nodeType === "number") return wrapNode(value);
    if (
      (typeof value === "object" || typeof value === "function") &&
      typeof value.length === "number"
    ) {
      return makeReadonlyNodeList(value);
    }
    return null;
  };

  const makeReadonlyNodeList = (nodes) => {
    const rawNodes = Array.from(nodes ?? []);
    return makeReadonlyList(rawNodes.map(wrapNode), (list) => {
      defineHiddenReadonly(list, "namedItem", (name) => {
        const key = String(name);
        const rawNamedItem =
          typeof nodes?.namedItem === "function"
            ? nodes.namedItem(key)
            : (rawNodes.find((node) => node?.id === key || node?.name === key) ??
              null);
        return wrapNodeCollectionResult(rawNamedItem);
      });
    });
  };

  const locationObject = new ReadonlyLocation();
  const windowObject = Object.create(null);
  const documentObject = wrapNode(rawDocument);

  const safeGetComputedStyle = (element, pseudoElement) => {
    const rawElement = unwrapElement(element);
    if (rawElement == null) throw new TypeError("getComputedStyle expects an Element");
    return wrapStyle(rawWindow.getComputedStyle(rawElement, pseudoElement));
  };

  const safeGetSelection = () =>
    typeof rawWindow.getSelection === "function"
      ? wrapSelection(rawWindow.getSelection())
      : null;

  const safeMatchMedia = (query) => {
    if (typeof rawWindow.matchMedia !== "function") return null;
    return wrapMediaQueryList(rawWindow.matchMedia(String(query)));
  };

  const isScrollOptionsObject = (value) =>
    value != null && typeof value === "object" && !Array.isArray(value);

  const finiteNumber = (value, fallback) => {
    if (value == null) return fallback;
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  };

  const safeWindowScrollTo = (leftOrOptions, top) => {
    if (typeof rawWindow.scrollTo !== "function") {
      throw new TypeError("scrollTo is not available");
    }
    if (isScrollOptionsObject(leftOrOptions)) {
      rawWindow.scrollTo({ ...leftOrOptions });
      return;
    }
    rawWindow.scrollTo(
      finiteNumber(leftOrOptions, 0),
      finiteNumber(top, 0),
    );
  };

  const safeWindowScrollBy = (leftOrOptions, top) => {
    const currentX = finiteNumber(rawWindow.scrollX, 0);
    const currentY = finiteNumber(rawWindow.scrollY, 0);
    if (typeof rawWindow.scrollBy === "function") {
      if (isScrollOptionsObject(leftOrOptions)) {
        rawWindow.scrollBy({ ...leftOrOptions });
        return;
      }
      rawWindow.scrollBy(
        finiteNumber(leftOrOptions, 0),
        finiteNumber(top, 0),
      );
      return;
    }
    if (isScrollOptionsObject(leftOrOptions)) {
      safeWindowScrollTo({
        left: currentX + finiteNumber(leftOrOptions.left, 0),
        top: currentY + finiteNumber(leftOrOptions.top, 0),
      });
      return;
    }
    safeWindowScrollTo(
      currentX + finiteNumber(leftOrOptions, 0),
      currentY + finiteNumber(top, 0),
    );
  };

  const safeSetTimeout = (callback, delay, ...args) => {
    if (typeof callback !== "function") {
      throw new TypeError("setTimeout callback must be a function");
    }
    if (typeof rawWindow.setTimeout !== "function") {
      throw new TypeError("setTimeout is not available");
    }
    return rawWindow.setTimeout(
      () => {
        callback(...args);
      },
      finiteNumber(delay, 0),
    );
  };

  const safeClearTimeout = (timeoutId) => {
    if (typeof rawWindow.clearTimeout === "function") {
      rawWindow.clearTimeout(timeoutId);
    }
  };

  const cssObject = Object.freeze({
    escape:
      typeof rawWindow.CSS?.escape === "function"
        ? (value) => rawWindow.CSS.escape(String(value))
        : undefined,
    supports:
      typeof rawWindow.CSS?.supports === "function"
        ? (property, value) =>
            value === undefined
              ? rawWindow.CSS.supports(String(property))
              : rawWindow.CSS.supports(String(property), String(value))
        : undefined,
  });

  defineHiddenReadonly(windowObject, "constructor", undefined);
  defineReadonly(windowObject, "CSS", cssObject);
  defineReadonly(windowObject, "document", documentObject);
  defineReadonly(windowObject, "location", locationObject);
  defineReadonly(windowObject, "self", windowObject);
  defineReadonly(windowObject, "top", windowObject);
  defineReadonly(windowObject, "window", windowObject);
  defineReadonly(windowObject, "getComputedStyle", safeGetComputedStyle);
  defineReadonly(windowObject, "getSelection", safeGetSelection);
  defineReadonly(windowObject, "matchMedia", safeMatchMedia);
  defineReadonly(windowObject, "clearTimeout", safeClearTimeout);
  defineReadonly(windowObject, "scroll", safeWindowScrollTo);
  defineReadonly(windowObject, "scrollBy", safeWindowScrollBy);
  defineReadonly(windowObject, "scrollTo", safeWindowScrollTo);
  defineReadonly(windowObject, "setTimeout", safeSetTimeout);
  defineReadonly(
    windowObject,
    "visualViewport",
    wrapVisualViewport(rawWindow.visualViewport),
  );
  for (const name of [
    "devicePixelRatio",
    "innerHeight",
    "innerWidth",
    "outerHeight",
    "outerWidth",
    "pageXOffset",
    "pageYOffset",
    "scrollX",
    "scrollY",
  ]) {
    defineReadonlyGetter(windowObject, name, () => rawWindow[name]);
  }
  Object.freeze(windowObject);

  const consoleObject = Object.freeze({
    debug() {},
    error() {},
    info() {},
    log() {},
    warn() {},
  });

  const cssRuleObject = Object.freeze({
    CHARSET_RULE: rawWindow.CSSRule?.CHARSET_RULE ?? 2,
    FONT_FACE_RULE: rawWindow.CSSRule?.FONT_FACE_RULE ?? 5,
    IMPORT_RULE: rawWindow.CSSRule?.IMPORT_RULE ?? 3,
    KEYFRAME_RULE: rawWindow.CSSRule?.KEYFRAME_RULE ?? 8,
    KEYFRAMES_RULE: rawWindow.CSSRule?.KEYFRAMES_RULE ?? 7,
    MEDIA_RULE: rawWindow.CSSRule?.MEDIA_RULE ?? 4,
    NAMESPACE_RULE: rawWindow.CSSRule?.NAMESPACE_RULE ?? 10,
    PAGE_RULE: rawWindow.CSSRule?.PAGE_RULE ?? 6,
    STYLE_RULE: rawWindow.CSSRule?.STYLE_RULE ?? 1,
    SUPPORTS_RULE: rawWindow.CSSRule?.SUPPORTS_RULE ?? 12,
  });

  const nodeObject = Object.freeze({
    COMMENT_NODE,
    DOCUMENT_FRAGMENT_NODE,
    DOCUMENT_NODE,
    DOCUMENT_TYPE_NODE,
    ELEMENT_NODE,
    TEXT_NODE,
  });
  const rangeObject = Object.freeze({
    END_TO_END: rawWindow.Range?.END_TO_END ?? 2,
    END_TO_START: rawWindow.Range?.END_TO_START ?? 3,
    START_TO_END: rawWindow.Range?.START_TO_END ?? 1,
    START_TO_START: rawWindow.Range?.START_TO_START ?? 0,
  });
  for (const name of ["createObjectURL", "revokeObjectURL"]) {
    defineHiddenReadonly(URL, name, undefined);
  }
  for (const namespace of [
    JSON,
    Math,
    Reflect,
    consoleObject,
    cssObject,
    cssRuleObject,
    nodeObject,
    rangeObject,
  ]) {
    Object.freeze(namespace);
  }

  // This object is the global allowlist used by the scope proxy below.
  // Anything not listed here resolves as undefined instead of falling through
  // to the isolated world's real global.
  const safeBindings = Object.freeze({
    Array,
    Boolean,
    console: consoleObject,
    CSS: cssObject,
    CSSRule: cssRuleObject,
    CSSRuleList: Array,
    CSSStyleDeclaration: ReadonlyStyleDeclaration,
    CSSStyleSheet: ReadonlyCSSStyleSheet,
    Date,
    document: documentObject,
${t == null ? "" : `    element: wrapNode(${t}),
`}    Element: ReadonlyElement,
${r == null ? "" : `    elements: makeReadonlyList(Array.from(${r}, wrapNode)),
`}    DocumentFragment: ReadonlyDocumentFragment,
    Error,
    getComputedStyle: safeGetComputedStyle,
    getSelection: safeGetSelection,
    HTMLCollection: Array,
    Infinity,
    isFinite,
    isNaN,
    JSON,
    location: locationObject,
    Map,
    Math,
    matchMedia: safeMatchMedia,
    MediaList: Array,
    NaN,
    Node: nodeObject,
    NodeList: Array,
    Number,
    Object,
    Promise,
    RangeError,
    Range: rangeObject,
    ReferenceError,
    Reflect,
    RegExp,
    Set,
    Selection: ReadonlySelection,
    String,
    StyleSheetList: Array,
    Symbol,
    ShadowRoot: ReadonlyDocumentFragment,
    clearTimeout: safeClearTimeout,
    scroll: safeWindowScrollTo,
    scrollBy: safeWindowScrollBy,
    scrollTo: safeWindowScrollTo,
    setTimeout: safeSetTimeout,
    TypeError,
    undefined,
    URIError,
    URL,
    URLSearchParams,
    visualViewport: windowObject.visualViewport,
    window: windowObject,
    self: windowObject,
    top: windowObject,
  });

  // The with-scope proxy claims every identifier lookup made by the user
  // script, so unlisted globals cannot leak through.
  const scope = new Proxy(Object.create(null), {
    defineProperty() {
      throw readonlyError("global defineProperty");
    },
    deleteProperty() {
      throw readonlyError("global delete");
    },
    get(_target, property) {
      if (property === Symbol.unscopables) return undefined;
      if (typeof property !== "string") return undefined;
      if (Object.prototype.hasOwnProperty.call(safeBindings, property)) {
        return safeBindings[property];
      }
      if (Object.prototype.hasOwnProperty.call(windowObject, property)) {
        return windowObject[property];
      }
      return undefined;
    },
    has() {
      return true;
    },
    set() {
      throw readonlyError("global assignment");
    },
  });

  const poisonConstructor = (prototype) => {
    try {
      Object.defineProperty(prototype, "constructor", {
        configurable: false,
        enumerable: false,
        value: undefined,
        writable: false,
      });
    } catch {}
  };

  // User code can create functions/classes/objects inside the isolated world.
  // Hide constructor links and freeze exposed intrinsics before it runs so those
  // objects cannot be used to climb back to a broader global or mutate helpers
  // that serialization relies on.
  const lockdownConstructors = (constructors) => {
    for (const constructor of constructors) {
      poisonConstructor(constructor.prototype);
      Object.freeze(constructor.prototype);
      Object.freeze(constructor);
    }
  };

  lockdownConstructors([
    Object,
    Function,
    Array,
    Promise,
    Map,
    Set,
    WeakMap,
    WeakSet,
    RegExp,
    Error,
    Number,
    String,
    Boolean,
    Date,
    URL,
    URLSearchParams,
    ReadonlyStyleDeclaration,
    ReadonlyMediaList,
    ReadonlyCSSRule,
    ReadonlyCSSStyleSheet,
    ReadonlyRange,
    ReadonlySelection,
    ReadonlyFontFaceSet,
    ReadonlyMediaQueryList,
    ReadonlyVisualViewport,
    ReadonlyLocation,
    ReadonlyNode,
    ReadonlyElement,
    ReadonlyDocumentFragment,
    ReadonlyDocument,
  ]);

  for (const prototype of [
    Object.getPrototypeOf(async function () {}),
    Object.getPrototypeOf(function* () {}),
    Object.getPrototypeOf(async function* () {}),
  ]) {
    if (prototype != null) {
      poisonConstructor(prototype);
      Object.freeze(prototype);
    }
  }

  const serializeResult = (value) => {
    // Runtime.evaluate returns only this bounded JSON-ish projection. Live DOM
    // wrappers never cross the boundary back to the caller.
    const seen = new WeakSet();

    const serializeNonObject = (current) => {
      switch (typeof current) {
        case "bigint":
          return current.toString();
        case "boolean":
        case "undefined":
          return current;
        case "function":
        case "symbol":
          return undefined;
        case "number":
          return Number.isFinite(current) ? current : null;
        case "string":
          return current.length > RESULT_MAX_STRING_LENGTH
            ? current.slice(0, RESULT_MAX_STRING_LENGTH) + "[Truncated]"
            : current;
        default:
          return current;
      }
    };

    const serializeArray = (items, depth) =>
      items
        .slice(0, RESULT_MAX_ARRAY_LENGTH)
        .map((item) => serialize(item, depth + 1));

    const serializeObject = (current, depth) => {
      if (depth > RESULT_MAX_DEPTH) return "[MaxDepth]";
      if (seen.has(current)) return "[Circular]";
      seen.add(current);
      if (
        rawNodesByWrapper.has(current) ||
        styleWrappers.has(current) ||
        cssWrappers.has(current) ||
        selectionAndRangeWrappers.has(current) ||
        readonlyApiWrappers.has(current)
      ) {
        return serialize(current.toJSON(), depth);
      }
      if (Array.isArray(current)) return serializeArray(current, depth);
      const output = {};
      for (const key of ownKeys(current)) {
        const serialized = serialize(current[key], depth + 1);
        if (serialized !== undefined) output[key] = serialized;
      }
      return output;
    };

    const serialize = (current, depth) =>
      current == null || typeof current !== "object"
        ? serializeNonObject(current)
        : serializeObject(current, depth);

    return serialize(value, 0);
  };

  const runUserScript = async () => {
    with (scope) {
      return await (async function () {
        "use strict";
${e}
      }).call(windowObject);
    }
  };

  return runUserScript().then(serializeResult);
})()`;
}

function _D(e) { if (kK.test(e))
    throw new Error("module loading is not available in playwright.evaluate"); }

function vD(e) { let t = e.frameTree.frame.id; if (!t)
    throw new Error("playwright.evaluate could not find a frame"); return t; }

function xD(e) { if (e.exceptionDetails != null) {
    let r = e.exceptionDetails.exception?.description ?? e.exceptionDetails.text ?? "Runtime.evaluate failed";
    throw new Error(r);
} let t = e.result; if (t != null)
    return t.unserializableValue != null ? FK(t.unserializableValue) : t.value; }

function FK(e) { switch (e) {
    case "-0": return -0;
    case "NaN": return null;
    case "Infinity": return null;
    case "-Infinity": return null;
} return /^-?\d+n$/.test(e) ? e.slice(0, -1) : e; }

const iX = "const __name = (target) => target;";

async function Ow(e, t, r) { let n = xI(e, r), o = await ru(t, n.target, n.frameId, n.loaderId, sX(n.localRef, n.viewportClip), { returnByValue: !0 }); if (o.exceptionDetails != null)
    throw new Error(lX(o.exceptionDetails)); let i = uX(o.result.value); if (i == null)
    throw new Error(`DOM node ${r} is stale or missing`); return { inputPoint: await TI(e, t, n.frameId, i), point: await CI(e, t, n.frameId, i), target: n.target }; }

function sX(e, t) {
    return `(() => {
    ${iX}
    return (${aX.toString()})(${JSON.stringify(e)}, ${JSON.stringify(t)});
  })()`;
}

function aX(e, t) { let n = globalThis.__browserUseVisibleDomState?.refToElement.get(e); if (!n?.isConnected)
    return null; n.scrollIntoView({ block: "nearest", inline: "nearest" }); let o = i => { if (i.width <= 0 || i.height <= 0)
    return null; let s = Math.max(i.left, t.left), a = Math.min(i.right, t.right), l = Math.max(i.top, t.top), c = Math.min(i.bottom, t.bottom); return a > s && c > l ? { x: s + (a - s) / 2, y: l + (c - l) / 2 } : null; }; for (let i of n.getClientRects()) {
    let s = o(i);
    if (s != null)
        return s;
} return o(n.getBoundingClientRect()); }

function uX(e) { if (e == null || typeof e != "object")
    return null; let t = e; return typeof t.x == "number" && typeof t.y == "number" ? { x: t.x, y: t.y } : null; }

function lX(e) { let t = e.exception; return typeof t?.value == "string" ? t.value : t?.description ?? e.text ?? "DOM node failed"; }

const AJ = "const __name = (target) => target;";

const IJ = e => { let t = globalThis, r = t.__browserUseClipboardBridge; if (r?.bindingName === e)
    return; r?.cleanup(); let n = t[e]; if (typeof n != "function")
    throw new Error("Browser Use clipboard bridge is unavailable"); let o = globalThis, i = navigator.clipboard, s = Object.prototype.hasOwnProperty.call(navigator, "clipboard"), a = Object.getOwnPropertyDescriptor(navigator, "clipboard"), l = new Map, c = 0, p = Y => { let F = new Uint8Array(Y), D = 32768, P = ""; for (let W = 0; W < F.length; W += D) {
    let V = F.subarray(W, W + D);
    P += String.fromCharCode(...V);
} return btoa(P); }, m = Y => { let F = atob(Y), D = new Uint8Array(F.length); for (let P = 0; P < F.length; P += 1)
    D[P] = F.charCodeAt(P); return D.buffer; }, h = Y => { let F = typeof Y.text == "string" ? Y.text : m(Y.base64 ?? ""); return new Blob([F], { type: Y.mime_type }); }, y = Y => typeof ClipboardItem == "function" ? new ClipboardItem(Object.fromEntries(Y.entries.map(F => [F.mime_type, h(F)])), { presentationStyle: Y.presentation_style ?? "unspecified" }) : { types: Y.entries.map(F => F.mime_type), presentationStyle: Y.presentation_style ?? "unspecified", getType: async (F) => { let D = Y.entries.find(P => P.mime_type === F); if (D == null)
        throw new DOMException(`No clipboard entry for ${F}`, "NotFoundError"); return h(D); } }, w = async (Y) => await Promise.all(Array.from(Y).map(async (F) => ({ entries: await Promise.all(F.types.map(async (D) => { let P = await F.getType(D); return D.startsWith("text/") ? { mime_type: D, text: await P.text() } : { mime_type: D, base64: p(await P.arrayBuffer()) }; })), presentation_style: F.presentationStyle ?? "unspecified" }))), v = (Y, F) => new Promise((D, P) => { let W = ++c; l.set(W, { reject: P, resolve: D }); try {
    n(JSON.stringify({ id: W, items: F, operation: Y }));
}
catch (V) {
    l.delete(W), P(V instanceof Error ? V : new Error(String(V)));
} }), S = Y => { if (Y !== T)
    throw new TypeError("Illegal invocation"); }, R = { read: async function () { return S(this), (await v("read")).map(y); }, readText: async function () { S(this); let Y = (await v("read")).flatMap(F => F.entries).find(F => F.mime_type === "text/plain"); return Y == null ? "" : await h(Y).text(); }, write: async function (Y) { if (S(this), arguments.length === 0)
        throw new TypeError("Failed to execute 'write' on 'Clipboard': 1 argument required"); let F = await w(Y); F.length !== 0 && await v("write", F); }, writeText: async function (Y) { if (S(this), arguments.length === 0)
        throw new TypeError("Failed to execute 'writeText' on 'Clipboard': 1 argument required"); let F = `${Y}`; await v("write", [{ entries: [{ mime_type: "text/plain", text: F }], presentation_style: "unspecified" }]); } }, T = { ...R, addEventListener: () => { }, dispatchEvent: () => !0, removeEventListener: () => { } }, k = { configurable: !0, get: () => T }, I = Object.keys(R), L = new Map, $, U = () => { if ($ != null) {
    for (let [Y, F] of L)
        Object.getOwnPropertyDescriptor($, Y)?.value === R[Y] && (F == null ? Reflect.deleteProperty($, Y) : Object.defineProperty($, Y, F));
    $ = void 0, L.clear();
} }, X = Y => { $ = Y; try {
    for (let F of I)
        L.set(F, Object.getOwnPropertyDescriptor(Y, F)), Object.defineProperty(Y, F, { configurable: !0, enumerable: !0, value: R[F], writable: !0 });
    return !0;
}
catch {
    return U(), !1;
} }; if (i != null) {
    let Y = Object.getPrototypeOf(i);
    X(i) || Y != null && I.every(F => F in Y) && X(Y) ? T = i : Object.defineProperty(navigator, "clipboard", k);
}
else
    Object.defineProperty(navigator, "clipboard", k); let oe = () => { if (t.__browserUseClipboardBridge?.bindingName === e) {
    for (let F of document.querySelectorAll("iframe,frame"))
        (F instanceof HTMLIFrameElement || F instanceof HTMLFrameElement) && F.contentWindow?.postMessage(e, "*");
    o.removeEventListener("message", me);
    for (let F of l.values())
        F.reject(new Error("Browser Use clipboard bridge was removed"));
    l.clear(), $ != null ? U() : Object.getOwnPropertyDescriptor(navigator, "clipboard")?.get === k.get && (s && a != null ? Object.defineProperty(navigator, "clipboard", a) : Reflect.deleteProperty(navigator, "clipboard")), delete t.__browserUseClipboardBridge, t[e] === n && Reflect.deleteProperty(t, e);
} }, me = Y => { Y.data !== e || Y.source !== o && Y.source !== o.parent || oe(); }, se = { bindingName: e, cleanup: oe, respond: Y => { let F; try {
        F = JSON.parse(Y);
    }
    catch {
        return;
    } let D = l.get(F.id); D != null && (l.delete(F.id), F.ok ? D.resolve(Array.isArray(F.items) ? F.items : []) : D.reject(new Error(F.error ?? "Clipboard request failed"))); } }; t.__browserUseClipboardBridge = se, o.addEventListener("message", me); };

function bB(e) {
    return `(() => {
    ${AJ}
    const installPageClipboardBridge = ${IJ.toString()};
    installPageClipboardBridge(${JSON.stringify(e)});
  })()`;
}

function yB(e) { return `globalThis.__browserUseClipboardBridge?.bindingName === ${JSON.stringify(e)} && globalThis.__browserUseClipboardBridge.cleanup()`; }

function wB(e) { return `globalThis.__browserUseClipboardBridge?.respond(${JSON.stringify(JSON.stringify(e))})`; }

const qm = class {
    constructor(t, r) { this.cdp = t; this.clipboard = r; this.cdp.on("event", this.handleCdpEvent), this.cdp.on("tabDetached", this.handleTabDetached), this.removeTabCleanupHandler = this.cdp.addTabCleanupHandler(async (n) => { await this.cleanupTab(n); }); }
    cdp;
    clipboard;
    installations = new Map;
    removeTabCleanupHandler;
    async ensure(t) { let r = _B(t), n = this.installations.get(r); if (n == null) {
        let o = `__browserUseClipboard_${crypto.randomUUID().replaceAll("-", "")}`;
        n = { bindingName: o, invalidated: !1, installation: this.install(t, o) }, this.installations.set(r, n);
    } try {
        let o = await n.installation;
        if (n.invalidated)
            return await this.cleanupTarget(o), this.installations.get(r) === n && this.installations.delete(r), await this.ensure(t);
    }
    catch (o) {
        if (this.installations.get(r) === n && this.installations.delete(r), n.invalidated)
            return await this.ensure(t);
        throw o;
    } }
    dispose() { this.cdp.removeListener("event", this.handleCdpEvent), this.cdp.removeListener("tabDetached", this.handleTabDetached), this.removeTabCleanupHandler(), this.installations.clear(); }
    async cleanup() { let t = [...this.installations.values()]; this.installations.clear(); for (let r of t) {
        let n = await r.installation.catch(() => { });
        n != null && await this.cleanupTarget(n);
    } }
    handleCdpEvent = t => { t.method !== "Runtime.bindingCalled" || t.params.name !== this.installations.get(_B(t.source))?.bindingName || this.handleBindingCall(t.source, t.params.executionContextId, t.params.payload); };
    handleTabDetached = t => { for (let [r, n] of this.installations)
        vB(r, t) && (n.invalidated = !0); };
    handleBindingCall(t, r, n) { let o; try {
        o = JSON.parse(n);
    }
    catch {
        return;
    } if (o == null || typeof o != "object" || !("id" in o) || typeof o.id != "number" || !Number.isSafeInteger(o.id))
        return; let i = o.id, s; try {
        if (!("operation" in o))
            throw new Error("Clipboard request is missing an operation");
        switch (o.operation) {
            case "read":
                s = { id: i, items: this.clipboard.read(), ok: !0 };
                break;
            case "write":
                if (!("items" in o) || !Array.isArray(o.items))
                    throw new Error("Clipboard write requires items");
                this.clipboard.write(o.items, "navigator.clipboard.write"), s = { id: i, ok: !0 };
                break;
            default: throw new Error("Unsupported clipboard operation");
        }
    }
    catch (a) {
        s = { error: a instanceof Error ? a.message : String(a), id: i, ok: !1 };
    } this.cdp.callTarget(t, "Runtime.evaluate", { contextId: r, expression: wB(s), returnByValue: !0 }).catch(() => { }); }
    async install(t, r) { let n = bB(r), o = !1, i; try {
        if (await this.cdp.callTarget(t, "Runtime.addBinding", { name: r }), o = !0, i = (await this.cdp.callTarget(t, "Page.addScriptToEvaluateOnNewDocument", { runImmediately: !0, source: n })).identifier, (await this.cdp.callTarget(t, "Runtime.evaluate", { expression: `globalThis.__browserUseClipboardBridge?.bindingName === ${JSON.stringify(r)}`, returnByValue: !0 })).result?.value !== !0)
            throw new Error("Browser Use clipboard bridge installation failed");
        return i == null ? { bindingName: r, target: t } : { bindingName: r, identifier: i, target: t };
    }
    catch (s) {
        throw o && await this.cleanupTarget(i == null ? { bindingName: r, target: t } : { bindingName: r, identifier: i, target: t }), s;
    } }
    async cleanupTab(t) { let r = [...this.installations.entries()].filter(([n]) => vB(n, t)); for (let [n, o] of r) {
        this.installations.delete(n);
        let i = await o.installation.catch(() => { });
        i != null && await this.cleanupTarget(i);
    } }
    async cleanupTarget(t) { let { bindingName: r, identifier: n, target: o } = t; await this.cdp.callTarget(o, "Runtime.evaluate", { expression: yB(r), returnByValue: !0 }).catch(() => { }), n != null && await this.cdp.callTarget(o, "Page.removeScriptToEvaluateOnNewDocument", { identifier: n }).catch(() => { }), await this.cdp.callTarget(o, "Runtime.removeBinding", { name: r }).catch(() => { }); }
};

function _B(e) { return e.sessionId != null ? `${e.tabId}:session:${e.sessionId}` : e.targetId != null ? `${e.tabId}:target:${e.targetId}` : `${e.tabId}:`; }

function vB(e, t) { return e.startsWith(`${t}:`); }

const Wm = class {
    items = [];
    operationTail = Promise.resolve();
    bridge;
    async ensurePageClipboard(t, r) { this.bridge ??= new qm(t, this), await this.bridge.ensure(r); }
    async cleanupPageClipboards() { await this.bridge?.cleanup(); }
    runExclusive(t) { let r = this.operationTail.then(t); return this.operationTail = r.then(() => { }, () => { }), r; }
    read() { return DJ(this.items); }
    write(t, r) { this.items = PJ(t, r); }
    dispose() { this.bridge?.dispose(), this.bridge = void 0, this.items = []; }
};

function PJ(e, t) { if (!Array.isArray(e) || e.length === 0)
    throw new Error(`${t} requires items`); return e.map(r => { if (r == null || typeof r != "object" || !Array.isArray(r.entries) || r.entries.length === 0)
    throw new Error(`${t} requires clipboard item entries`); if (r.presentation_style != null && r.presentation_style !== "unspecified" && r.presentation_style !== "inline" && r.presentation_style !== "attachment")
    throw new Error(`${t} requires a valid presentation_style`); return { entries: r.entries.map(n => RJ(n, t)), ...r.presentation_style == null ? {} : { presentation_style: r.presentation_style } }; }); }

function RJ(e, t) { if (e == null || typeof e != "object" || typeof e.mime_type != "string" || e.mime_type.length === 0)
    throw new Error(`${t} requires entry mime_type`); if (typeof e.text == "string" && typeof e.base64 != "string")
    return { mime_type: e.mime_type, text: e.text }; if (typeof e.base64 == "string" && typeof e.text != "string")
    return kJ(e.base64, t), { mime_type: e.mime_type, base64: e.base64 }; throw new Error(`${t} requires exactly one of entry text or base64`); }

function kJ(e, t) { try {
    atob(e);
}
catch {
    throw new Error(`${t} requires valid base64 entry data`);
} }

function DJ(e) { return e.map(t => ({ entries: t.entries.map(r => ({ ...r })), ...t.presentation_style == null ? {} : { presentation_style: t.presentation_style } })); }

module.exports = { LocatorRuntime: wm, readonlySource: kw, evaluateReadonly: gD, Clipboard: Wm, PageKeyboard: Zl, keypress: ec, character: hx, clipboardChord: gx, pastePage: tc, commitCutPage: _x, credentialFieldPolicy: on, isProtectedCredentialField: ai, visibleDom: MI, domPoint: Ow, invalidatePageWorlds(tabId) { for (const key of Lo.keys()) if (key.startsWith(`${tabId}:`)) Lo.delete(key); for (const key of bn.keys()) if (key.startsWith(`${tabId}:`)) bn.delete(key); }, clearDomState(tabId) { yn.delete(tabId); for (const key of Lo.keys()) if (key.startsWith(`${tabId}:`)) Lo.delete(key); for (const key of bn.keys()) if (key.startsWith(`${tabId}:`)) bn.delete(key); } };
