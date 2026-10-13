const { TextDecoder } = require("node:util");

const MAX_JSON_DEPTH = 512;
const MAX_ROOT_KEY_BYTES = 64 * 1024;
const MAX_ROOT_MEMBERS = 65_536;

function parseArtifactReadProjection(value) {
  const fields = ["schema_version", "json_object_labels", "excluded_root_fields", "max_source_bytes", "max_projected_bytes"];
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== fields.length || fields.some(key => !Object.hasOwn(value, key))
    || value.schema_version !== 1) {
    throw new Error("Artifact read projection requires its exact version 1 contract");
  }
  for (const key of ["max_source_bytes", "max_projected_bytes"]) {
    if (!Number.isSafeInteger(value[key]) || value[key] < 1) throw new Error(`Artifact read projection ${key} must be a finite positive byte bound`);
  }
  const names = value.excluded_root_fields;
  if (!Array.isArray(names) || names.length > MAX_ROOT_MEMBERS || names.some(name => (
    typeof name !== "string" || !name || name.trim() !== name || Buffer.byteLength(name) > MAX_ROOT_KEY_BYTES
  )) || new Set(names).size !== names.length) {
    throw new Error("Artifact read projection requires unique explicit root field names");
  }
  const labels = value.json_object_labels;
  if (!Array.isArray(labels) || labels.length > MAX_ROOT_MEMBERS || labels.some(label => (
    typeof label !== "string" || !label || label.trim() !== label || Buffer.byteLength(label) > MAX_ROOT_KEY_BYTES
  )) || new Set(labels).size !== labels.length) throw new Error("Artifact read projection requires unique explicit JSON object labels");
  return Object.freeze({ schema_version: 1, json_object_labels: Object.freeze([...labels]), excluded_root_fields: Object.freeze([...names]),
    max_source_bytes: value.max_source_bytes, max_projected_bytes: value.max_projected_bytes });
}

function artifactReadProjectionForLabel(value, label) {
  const contract = parseArtifactReadProjection(value);
  return contract.json_object_labels.includes(label) ? contract : null;
}

class BoundedJsonProjection {
  constructor(contract) {
    this.contract = parseArtifactReadProjection(contract);
    this.excluded = new Set(this.contract.excluded_root_fields);
    this.excludedPresent = new Set();
    this.decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
    this.frames = [];
    this.rootNames = new Set();
    this.parts = [];
    this.projectedBytes = 0;
    this.sourceBytes = 0;
    this.publicMembers = 0;
    this.rootStarted = false;
    this.rootDone = false;
    this.finished = false;
    this.token = null;
    this.capture = false;
    this.captureStart = -1;
    this.keyStart = -1;
    this.keyParts = [];
    this.keyBytes = 0;
    this.text = "";
  }

  write(bytes) {
    if (this.finished) throw new Error("JSON projection is already finished");
    if (!Buffer.isBuffer(bytes)) throw new Error("JSON projection requires source bytes");
    this.sourceBytes += bytes.length;
    if (this.sourceBytes > this.contract.max_source_bytes) throw new Error("Artifact JSON exceeds its source byte bound");
    this.consume(this.decode(bytes, true));
  }

  finish() {
    if (this.finished) throw new Error("JSON projection is already finished");
    this.consume(this.decode(undefined, false));
    if (this.token?.type === "number" && this.numberCanEnd()) {
      this.token = null;
      this.valueComplete(0);
    }
    if (this.token || !this.rootDone || this.frames.length) this.fail("incomplete JSON");
    this.finished = true;
    const bytes = Buffer.concat(this.parts, this.projectedBytes);
    return { payload: JSON.parse(bytes.toString("utf8")), projectedBytes: bytes,
      originalBytes: this.sourceBytes, excludedRootFields: [...this.excludedPresent] };
  }

  decode(bytes, stream) {
    try { return this.decoder.decode(bytes, { stream }); }
    catch (error) { throw new Error("Artifact JSON contains invalid UTF-8", { cause: error }); }
  }

  consume(text) {
    this.text = text;
    this.captureStart = this.capture ? 0 : -1;
    this.keyStart = this.token?.rootKey ? 0 : -1;
    for (let index = 0; index < text.length; index++) this.character(text.charCodeAt(index), index);
    this.flushCapture(text.length);
    if (this.keyStart >= 0) this.appendKey(text.slice(this.keyStart));
    this.keyStart = -1;
    this.text = "";
  }

  character(code, index) {
    const token = this.token;
    if (token?.type === "string") {
      if (token.unicode) {
        if (!(code >= 48 && code <= 57 || code >= 65 && code <= 70 || code >= 97 && code <= 102)) this.fail("invalid Unicode escape");
        token.unicode--;
      } else if (token.escape) {
        token.escape = false;
        if (code === 117) token.unicode = 4;
        else if (![34, 92, 47, 98, 102, 110, 114, 116].includes(code)) this.fail("invalid string escape");
      } else if (code === 92) token.escape = true;
      else if (code === 34) {
        this.token = null;
        if (token.key) this.keyComplete(token.rootKey, index + 1);
        else this.valueComplete(index + 1);
      } else if (code < 32) this.fail("unescaped control character");
      return;
    }
    if (token?.type === "literal") {
      if (code !== token.word.charCodeAt(token.index++)) this.fail("invalid literal");
      if (token.index === token.word.length) { this.token = null; this.valueComplete(index + 1); }
      return;
    }
    if (token?.type === "number") {
      if (this.numberCharacter(code)) return;
      if (!this.numberCanEnd()) this.fail("incomplete number");
      this.token = null;
      this.valueComplete(index);
    }
    if (code === 32 || code === 9 || code === 10 || code === 13) return;
    if (!this.rootStarted) {
      if (code !== 123) this.fail("root must be an object");
      this.rootStarted = true;
      this.appendPublic("{");
      this.frames.push({ type: "object", state: "key_or_end", root: true });
      return;
    }
    if (this.rootDone) this.fail("data follows the root object");
    const frame = this.frames.at(-1);
    if (frame.type === "object") {
      if (frame.state === "key_or_end" && code === 125) return this.closeContainer(index + 1);
      if (frame.state === "key" || frame.state === "key_or_end") {
        if (code !== 34) this.fail("object requires a quoted member name");
        this.token = { type: "string", key: true, rootKey: frame.root, escape: false, unicode: 0 };
        if (frame.root) { this.keyStart = index; this.keyParts = []; this.keyBytes = 0; }
        return;
      }
      if (frame.state === "colon") {
        if (code !== 58) this.fail("object member requires a colon");
        frame.state = "value";
        return;
      }
      if (frame.state === "comma_or_end") {
        if (code === 125) return this.closeContainer(index + 1);
        if (code !== 44) this.fail("object requires a comma or closing brace");
        frame.state = "key";
        return;
      }
    } else {
      if (frame.state === "value_or_end" && code === 93) return this.closeContainer(index + 1);
      if (frame.state === "comma_or_end") {
        if (code === 93) return this.closeContainer(index + 1);
        if (code !== 44) this.fail("array requires a comma or closing bracket");
        frame.state = "value";
        return;
      }
    }
    this.startValue(code);
  }

  startValue(code) {
    if (code === 123 || code === 91) {
      if (this.frames.length >= MAX_JSON_DEPTH) this.fail(`nesting exceeds ${MAX_JSON_DEPTH}`);
      this.frames.push({ type: code === 123 ? "object" : "array", state: code === 123 ? "key_or_end" : "value_or_end" });
    } else if (code === 34) this.token = { type: "string", key: false, rootKey: false, escape: false, unicode: 0 };
    else if (code === 116 || code === 102 || code === 110) {
      this.token = { type: "literal", word: code === 116 ? "true" : code === 102 ? "false" : "null", index: 1 };
    } else if (code === 45 || code >= 48 && code <= 57) {
      this.token = { type: "number", state: code === 45 ? "minus" : code === 48 ? "zero" : "integer" };
    } else this.fail("invalid value");
  }

  numberCharacter(code) {
    const token = this.token;
    const digit = code >= 48 && code <= 57;
    if (token.state === "minus") {
      if (!digit) this.fail("number sign requires a digit");
      token.state = code === 48 ? "zero" : "integer";
    } else if (token.state === "zero" || token.state === "integer") {
      if (digit) { if (token.state === "zero") this.fail("number has a leading zero"); }
      else if (code === 46) token.state = "fraction_start";
      else if (code === 101 || code === 69) token.state = "exponent_start";
      else return false;
    } else if (token.state === "fraction_start") {
      if (!digit) this.fail("fraction requires a digit");
      token.state = "fraction";
    } else if (token.state === "fraction") {
      if (code === 101 || code === 69) token.state = "exponent_start";
      else if (!digit) return false;
    } else if (token.state === "exponent_start") {
      if (code === 43 || code === 45) token.state = "exponent_sign";
      else if (digit) token.state = "exponent";
      else this.fail("exponent requires a digit");
    } else if (token.state === "exponent_sign") {
      if (!digit) this.fail("exponent sign requires a digit");
      token.state = "exponent";
    } else if (!digit) return false;
    return true;
  }

  numberCanEnd() { return ["zero", "integer", "fraction", "exponent"].includes(this.token.state); }

  keyComplete(root, end) {
    const frame = this.frames.at(-1);
    frame.state = "colon";
    if (!root) return;
    this.appendKey(this.text.slice(this.keyStart, end));
    this.keyStart = -1;
    const raw = this.keyParts.join("");
    const name = JSON.parse(raw);
    this.keyParts = [];
    if (this.rootNames.has(name)) this.fail("duplicate root member");
    if (this.rootNames.size >= MAX_ROOT_MEMBERS) this.fail("too many root members");
    this.rootNames.add(name);
    if (this.excluded.has(name)) this.excludedPresent.add(name);
    else {
      if (this.publicMembers++) this.appendPublic(",");
      this.appendPublic(raw);
      this.capture = true;
      this.captureStart = end;
    }
  }

  valueComplete(end) {
    const frame = this.frames.at(-1);
    if (!frame || !["value", "value_or_end"].includes(frame.state)) this.fail("unexpected completed value");
    frame.state = "comma_or_end";
    if (frame.root) { this.flushCapture(end); this.capture = false; }
  }

  closeContainer(end) {
    const frame = this.frames.pop();
    if (frame.root) { this.rootDone = true; this.appendPublic("}"); }
    else this.valueComplete(end);
  }

  flushCapture(end) {
    if (this.captureStart >= 0) this.appendPublic(this.text.slice(this.captureStart, end));
    this.captureStart = -1;
  }

  appendKey(text) {
    this.keyBytes += Buffer.byteLength(text);
    if (this.keyBytes > MAX_ROOT_KEY_BYTES) this.fail("root member name exceeds its byte bound");
    this.keyParts.push(text);
  }

  appendPublic(text) {
    if (!text) return;
    const bytes = Buffer.from(text, "utf8");
    this.projectedBytes += bytes.length;
    if (this.projectedBytes > this.contract.max_projected_bytes) throw new Error("Artifact JSON exceeds its projected byte bound");
    this.parts.push(bytes);
  }

  fail(detail) { throw new Error(`Artifact JSON projection is malformed: ${detail}`); }
}

module.exports = { BoundedJsonProjection, parseArtifactReadProjection, artifactReadProjectionForLabel };
