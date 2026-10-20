const fs = require("node:fs");
const path = require("node:path");
const postcss = require("postcss");

const appRoot = path.resolve(__dirname, "../..");
const inherited = new Set(["color", "cursor", "pointer-events", "text-align", "visibility"]);
const states = { hover: "data-hover", "focus-visible": "data-focus-visible", "focus-within": "data-focus-within", active: "data-active" };

function parseStylesheet(file, text = fs.readFileSync(file, "utf8")) {
  const root = postcss.parse(text, { from: file });
  root.walkAtRules("import", (rule) => {
    const target = postcss.list.space(rule.params)[0].replace(/^["']|["']$/g, "");
    const layer = postcss.list.space(rule.params).find((part) => part.startsWith("layer("));
    if (!target.startsWith(".")) return rule.remove();
    const nodes = parseStylesheet(path.resolve(path.dirname(file), target)).nodes;
    rule.replaceWith(layer ? postcss.atRule({ name: "layer", params: layer.slice(6, -1), nodes }) : nodes);
  });
  return root;
}

async function tailwindStylesheet(file, candidates) {
  const { compile } = require("@tailwindcss/node");
  const compiler = await compile(fs.readFileSync(file, "utf8"), { base: path.dirname(file), onDependency() {} });
  return postcss.parse(compiler.build(candidates), { from: file });
}

function splitTop(value, separator) {
  const parts = [];
  let depth = 0, current = "";
  for (const character of value) {
    if (character === "(") depth++;
    if (character === ")") depth--;
    if (depth === 0 && (separator === " " ? /\s/.test(character) : character === separator)) {
      if (current.trim()) parts.push(current.trim());
      current = "";
    } else current += character;
  }
  if (current.trim()) parts.push(current.trim());
  return parts;
}

function specificity(selector) {
  const score = [0, 0, 0];
  const identEnd = (index) => {
    while (index < selector.length && /[\w-]|\\/.test(selector[index])) index += selector[index] === "\\" ? 2 : 1;
    return index;
  };
  for (let index = 0; index < selector.length;) {
    const character = selector[index];
    if (character === "#") { score[0]++; index = identEnd(index + 1); }
    else if (character === ".") { score[1]++; index = identEnd(index + 1); }
    else if (character === "[") { score[1]++; index = selector.indexOf("]", index) + 1; }
    else if (character === ":") {
      const element = selector[index + 1] === ":", start = index + (element ? 2 : 1), end = identEnd(start), name = selector.slice(start, end);
      let next = end, args = "";
      if (selector[end] === "(") {
        let depth = 1;
        for (next = end + 1; depth; next++) depth += selector[next] === "(" ? 1 : selector[next] === ")" ? -1 : 0;
        args = selector.slice(end + 1, next - 1);
      }
      if (element) score[2]++;
      else if (["is", "not", "has"].includes(name)) {
        const best = splitTop(args, ",").map(specificity).sort(compare).at(-1) ?? [0, 0, 0];
        best.forEach((value, position) => { score[position] += value; });
      } else if (name !== "where") score[1]++;
      index = next;
    } else if (/[a-z]/i.test(character)) { score[2]++; index = identEnd(index); }
    else index++;
  }
  return score;
}

function compare(left, right) {
  for (let index = 0; index < left.length; index++) if (left[index] !== right[index]) return left[index] - right[index];
  return 0;
}

function expand(property, value) {
  const parts = splitTop(value, " ");
  const sides = (prefix, suffix = "") => {
    const [top, right = top, bottom = top, left = right] = parts;
    return [[`${prefix}top${suffix}`, top], [`${prefix}right${suffix}`, right], [`${prefix}bottom${suffix}`, bottom], [`${prefix}left${suffix}`, left]];
  };
  if (property === "inset") return sides("");
  if (property === "padding" || property === "margin") return sides(`${property}-`);
  if (property === "flex") {
    const [grow, shrink, basis] = value === "none" ? ["0", "0", "auto"] : value === "auto" ? ["1", "1", "auto"]
      : parts.length === 1 && !/^\d/.test(parts[0]) ? ["1", "1", parts[0]] : [parts[0], parts[1] ?? "1", parts[2] ?? "0%"];
    return [["flex-grow", grow], ["flex-shrink", shrink], ["flex-basis", basis]];
  }
  return [[property, value]];
}

function mediaMatches(params, media) {
  const features = [...params.matchAll(/\(\s*([\w-]+)\s*:\s*([^)]+?)\s*\)/g)];
  return features.every(([, name, value]) => media[name] === value);
}

function stylesheetCascade(sources, { media = {} } = {}) {
  const layers = [], rules = [], keyframes = new Set();
  const layerRank = (name) => {
    if (!layers.includes(name)) layers.push(name);
    return layers.indexOf(name);
  };
  let order = 0;
  const visit = (node, layer) => {
    for (const child of node.nodes ?? []) {
      if (child.type === "atrule" && child.name === "layer") {
        const names = splitTop(child.params, ",");
        if (!child.nodes) names.forEach((name) => layerRank(layer ? `${layer}.${name}` : name));
        else {
          const name = layer ? `${layer}.${names[0] ?? `anonymous-${order}`}` : names[0] ?? `anonymous-${order}`;
          layerRank(name);
          visit(child, name);
        }
      } else if (child.type === "atrule" && child.name === "media") {
        if (mediaMatches(child.params, media)) visit(child, layer);
      } else if (child.type === "atrule" && child.name === "supports") visit(child, layer);
      else if (child.type === "atrule" && /keyframes$/.test(child.name)) keyframes.add(child.params);
      else if (child.type === "rule") {
        const declarations = child.nodes.filter((entry) => entry.type === "decl")
          .flatMap((entry) => expand(entry.prop, entry.value).map(([property, value]) => ({ property, value, important: entry.important })));
        rules.push({ selectors: child.selectors, declarations, layer, order: order++ });
      }
    }
  };
  for (const source of sources) visit(typeof source === "string" ? parseStylesheet(path.resolve(appRoot, source)) : source);

  const matching = (element, selector) => {
    if (selector.includes("::") || /:has\(\s*[>+~]/.test(selector)) return false;
    try {
      return element.matches(selector.replace(/:(hover|focus-visible|focus-within|active)\b/g, (_, state) => `[${states[state]}]`));
    } catch {
      return false;
    }
  };
  const winners = (element) => {
    const found = new Map();
    for (const rule of rules) {
      const selected = rule.selectors.filter((selector) => matching(element, selector)).map(specificity).sort(compare).at(-1);
      if (!selected) continue;
      const rank = rule.layer === undefined ? layers.length : layerRank(rule.layer);
      for (const declaration of rule.declarations) {
        const key = [declaration.important ? 1 : 0, declaration.important ? -rank : rank, ...selected, rule.order];
        const current = found.get(declaration.property);
        if (!current || compare(current.key, key) <= 0) found.set(declaration.property, { key, value: declaration.value });
      }
    }
    for (let index = 0; index < element.style.length; index++) {
      const property = element.style[index];
      for (const [name, value] of expand(property, element.style.getPropertyValue(property))) found.set(name, { key: [], value });
    }
    return found;
  };
  const declared = (element, property) => winners(element).get(property)?.value;
  const value = (element, property) => {
    for (let current = element; current; current = current.parentElement) {
      const own = declared(current, property);
      if (own !== undefined && own !== "inherit") return resolve(current, own);
      if (!property.startsWith("--") && !inherited.has(property)) return undefined;
    }
    return undefined;
  };
  const resolve = (element, text) => {
    let result = text;
    for (let depth = 0; result.includes("var(") && depth < 32; depth++) {
      result = result.replace(/var\(\s*(--[\w-]+)\s*(?:,\s*((?:[^()]|\([^()]*\))*))?\)/g, (_, name, fallback) => value(element, name) ?? fallback ?? "");
    }
    return result.trim();
  };
  const appRegion = (element) => {
    for (let current = element; current; current = current.parentElement) {
      const region = declared(current, "-webkit-app-region");
      if (region && region !== "none") return region;
    }
    return "none";
  };
  return { keyframes, declared, value, resolve, appRegion, properties: (element) => [...winners(element).keys()], splitTop };
}

module.exports = { stylesheetCascade, tailwindStylesheet, parseStylesheet };
