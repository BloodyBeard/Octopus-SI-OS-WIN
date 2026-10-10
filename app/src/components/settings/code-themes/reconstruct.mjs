import pierreDark from "./upstream/pierre-0.0.29/pierre-dark.json" with { type: "json" };
import pierreLight from "./upstream/pierre-0.0.29/pierre-light.json" with { type: "json" };
import rayRules from "./upstream/ray-so/token-rules.json" with { type: "json" };
import parameters from "./appearance-parameters.json" with { type: "json" };

function variantParameters(kind, family) {
  if (kind !== "dark" && kind !== "light") throw new Error(`Unknown theme variant: ${kind}`);
  const entry = parameters[`${kind}-${family}`];
  if (!entry) throw new Error(`Missing theme appearance parameters: ${kind}-${family}`);
  return entry;
}

export function reconstructCodex(kind) {
  const { appearance, foreground_substitutions: substitutions } = variantParameters(kind, "codex");
  const source = kind === "dark" ? pierreDark : pierreLight;
  const replace = (color) => {
    if (!Object.hasOwn(substitutions, color)) throw new Error(`Unmapped Pierre foreground: ${color}`);
    return substitutions[color];
  };
  const tokenColors = source.tokenColors.map((token) => ({
    ...structuredClone(token),
    settings: Object.hasOwn(token.settings, "foreground")
      ? { ...token.settings, foreground: replace(token.settings.foreground) }
      : { ...token.settings },
  }));
  const semanticTokenColors = Object.fromEntries(
    Object.entries(source.semanticTokenColors).map(([scope, color]) => [scope, replace(color)]),
  );
  return { ...structuredClone(appearance), tokenColors, semanticTokenColors };
}

export function reconstructRaycast(kind) {
  const { appearance, foreground_palette: palette } = variantParameters(kind, "raycast");
  const source = structuredClone(rayRules);
  const tokenColors = [...source.slice(0, 17), source[18]];
  tokenColors[6].scope = ["comment", "string.quoted.docstring.multi"];
  tokenColors[11].scope.splice(1, 0, "support.class.component");
  tokenColors[9].settings.fontStyle = "italic";
  if (kind === "light") {
    tokenColors.splice(12, 0, {
      scope: ["string.quoted", "string.template", "string.regexp", "string.interpolated"],
      settings: {},
    });
  }
  for (const scope of ["markup.inserted", "markup.deleted", "markup.changed"]) {
    tokenColors.push({ scope: [scope], settings: {} });
  }
  if (palette.length !== tokenColors.length) throw new Error(`Raycast palette length differs: ${kind}`);
  tokenColors.forEach((token, index) => {
    const color = palette[index];
    if (color === null) {
      if (Object.hasOwn(token.settings, "foreground")) throw new Error(`Missing Raycast foreground: ${index}`);
    } else {
      if (typeof color !== "string") throw new Error(`Invalid Raycast foreground: ${index}`);
      token.settings.foreground = color;
    }
  });
  return { ...structuredClone(appearance), tokenColors };
}
