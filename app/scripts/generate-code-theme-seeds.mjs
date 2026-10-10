import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { reconstructCodex, reconstructRaycast } from "../src/components/settings/code-themes/reconstruct.mjs";

const args = process.argv.slice(2);
if (args.some((argument) => argument !== "--check")) throw new Error("Usage: generate-code-theme-seeds.mjs [--check]");

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const appDirectory = resolve(scriptDirectory, "..");
const outputDirectory = resolve(
  appDirectory,
  "src/components/settings/code-themes",
);
const seedOutputFile = resolve(
  appDirectory,
  "src/design-system/code-theme-seeds.json",
);

const defaultChromeThemes = {
  dark: {
    accent: "#339cff",
    contrast: 60,
    fonts: { code: null, ui: null },
    ink: "#ffffff",
    opaqueWindows: false,
    semanticColors: {
      diffAdded: "#40c977",
      diffRemoved: "#fa423e",
      skill: "#ad7bf9",
    },
    surface: "#181818",
  },
  light: {
    accent: "#339cff",
    contrast: 45,
    fonts: { code: null, ui: null },
    ink: "#1a1c1f",
    opaqueWindows: false,
    semanticColors: {
      diffAdded: "#00a240",
      diffRemoved: "#ba2623",
      skill: "#924ff7",
    },
    surface: "#ffffff",
  },
};

const backgroundColorKeys = [
  "editor.background",
  "sideBar.background",
  "editorGroupHeader.tabsBackground",
  "panel.background",
  "activityBar.background",
];
const foregroundColorKeys = [
  "editor.foreground",
  "sideBarTitle.foreground",
  "sideBar.foreground",
  "foreground",
];
const accentColorKeys = [
  "activityBarBadge.background",
  "textLink.foreground",
  "editorCursor.foreground",
  "focusBorder",
  "button.background",
  "activityBar.activeBorder",
];
const addedColorKeys = [
  "gitDecoration.addedResourceForeground",
  "gitDecoration.untrackedResourceForeground",
  "terminal.ansiGreen",
  "terminal.ansiBrightGreen",
];
const removedColorKeys = [
  "gitDecoration.deletedResourceForeground",
  "terminal.ansiRed",
  "terminal.ansiBrightRed",
];
const skillColorKeys = [
  "charts.purple",
  "terminal.ansiMagenta",
  "terminal.ansiBrightMagenta",
];

const sourceModules = {
  "light": {
    "absolutely": "light-absolutely.json",
    "catppuccin": "@shikijs/themes/catppuccin-latte",
    "codex": () => reconstructCodex("light"),
    "everforest": "@shikijs/themes/everforest-light",
    "github": "@shikijs/themes/github-light-default",
    "gruvbox": "@shikijs/themes/gruvbox-light-medium",
    "linear": "light-linear.json",
    "notion": "light-notion.json",
    "one": "@shikijs/themes/one-light",
    "proof": "light-proof.json",
    "raycast": () => reconstructRaycast("light"),
    "rose-pine": "@shikijs/themes/rose-pine-dawn",
    "solarized": "@shikijs/themes/solarized-light",
    "vercel": "light-vercel.json",
    "vscode-plus": "@shikijs/themes/light-plus",
    "xcode": "light-xcode.json"
  },
  "dark": {
    "absolutely": "dark-absolutely.json",
    "ayu": "@shikijs/themes/ayu-dark",
    "catppuccin": "@shikijs/themes/catppuccin-mocha",
    "codex": () => reconstructCodex("dark"),
    "dracula": "@shikijs/themes/dracula",
    "everforest": "@shikijs/themes/everforest-dark",
    "github": "@shikijs/themes/github-dark-default",
    "gruvbox": "@shikijs/themes/gruvbox-dark-medium",
    "linear": "dark-linear.json",
    "lobster": "dark-lobster.json",
    "material": "@shikijs/themes/material-theme-darker",
    "matrix": "dark-matrix.json",
    "monokai": "@shikijs/themes/monokai",
    "night-owl": "@shikijs/themes/night-owl",
    "nord": "@shikijs/themes/nord",
    "notion": "dark-notion.json",
    "one": "@shikijs/themes/one-dark-pro",
    "oscurange": "dark-oscurange.json",
    "raycast": () => reconstructRaycast("dark"),
    "rose-pine": "@shikijs/themes/rose-pine-moon",
    "sentry": "dark-sentry.json",
    "solarized": "@shikijs/themes/solarized-dark",
    "temple": "dark-temple.json",
    "tokyo-night": "@shikijs/themes/tokyo-night",
    "vercel": "dark-vercel.json",
    "vscode-plus": "@shikijs/themes/dark-plus",
    "xcode": "dark-xcode.json"
  }
};

const chromeThemeSeeds = { light: {}, dark: {} };

for (const [kind, entries] of Object.entries(sourceModules)) {
  for (const [id, sourceFile] of Object.entries(entries)) {
    const theme = typeof sourceFile === "function" ? sourceFile() : sourceFile.startsWith("@shikijs/themes/")
      ? (await import(sourceFile)).default
      : JSON.parse(await readFile(resolve(outputDirectory, sourceFile), "utf8"));
    if (theme == null || typeof theme !== "object") {
      throw new Error(`${sourceFile} did not export a theme object.`);
    }
    if (theme.type != null && theme.type !== kind) {
      throw new Error(`${sourceFile} exported ${String(theme.type)}, expected ${kind}.`);
    }
    chromeThemeSeeds[kind][id] = chromeThemeSeed(theme, kind);

  }
}

const output = `${JSON.stringify(chromeThemeSeeds, null, 2)}\n`;
if (args.includes("--check")) {
  if (await readFile(seedOutputFile, "utf8") !== output) throw new Error("Code theme seeds differ from their inputs");
  console.log("Verified all 43 code theme seeds.");
} else {
  await writeFile(seedOutputFile, output, "utf8");
}

function chromeThemeSeed(theme, kind) {
  const fallback = defaultChromeThemes[kind];
  const surface = firstThemeColor(theme.colors, backgroundColorKeys) ?? fallback.surface;
  const ink = firstThemeColor(theme.colors, foregroundColorKeys) ?? fallback.ink;
  const accent = findAccent(theme, surface, ink) ?? fallback.accent;
  const seed = {
    accent,
    ink,
    semanticColors: {
      diffAdded: firstThemeColor(theme.colors, addedColorKeys)
        ?? findHueColor(theme, surface, ink, { min: 80, max: 170 }, 125)
        ?? fallback.semanticColors.diffAdded,
      diffRemoved: firstThemeColor(theme.colors, removedColorKeys)
        ?? findHueColor(theme, surface, ink, { min: 345, max: 15 }, 0)
        ?? fallback.semanticColors.diffRemoved,
      skill: firstThemeColor(theme.colors, skillColorKeys)
        ?? findHueColor(theme, surface, ink, { min: 210, max: 320 }, 265)
        ?? (!colorsAreClose(accent, surface) && !colorsAreClose(accent, ink)
          ? accent
          : fallback.semanticColors.skill),
    },
    surface,
  };
  return mergeChromeTheme(seed, theme.chromeTheme);
}

function mergeChromeTheme(seed, override) {
  if (override == null) return seed;
  return {
    ...seed,
    ...override,
    fonts: override.fonts == null
      ? seed.fonts
      : { ...seed.fonts, ...override.fonts },
    semanticColors: override.semanticColors == null
      ? seed.semanticColors
      : { ...seed.semanticColors, ...override.semanticColors },
  };
}

function firstThemeColor(colors, keys) {
  if (colors == null) return undefined;
  for (const key of keys) {
    const color = normalizeThemeColor(colors[key]);
    if (color != null) return color;
  }
  return undefined;
}

function findAccent(theme, surface, ink) {
  for (const key of accentColorKeys) {
    const color = normalizeThemeColor(theme.colors?.[key], {
      minimumAlpha: 0.45,
      minimumChromaticRange: 24,
    });
    if (color != null && !colorsAreClose(color, surface) && !colorsAreClose(color, ink)) {
      return color;
    }
  }

  let selected;
  let selectedScore = -1;
  for (const token of themeTokens(theme)) {
    const color = normalizeThemeColor(token.settings?.foreground, {
      minimumAlpha: 0.45,
      minimumChromaticRange: 24,
    });
    if (color == null || colorsAreClose(color, surface) || colorsAreClose(color, ink)) continue;
    const score = contrastScore(color, surface, ink);
    if (score > selectedScore) {
      selected = color;
      selectedScore = score;
    }
  }
  return selected;
}

function findHueColor(theme, surface, ink, range, targetHue) {
  let selected;
  let selectedScore = -1;
  for (const color of allThemeColors(theme)) {
    if (colorsAreClose(color, surface) || colorsAreClose(color, ink)) continue;
    const hue = colorHue(parseHexColor(color));
    if (hue == null || !hueInRange(hue, range)) continue;
    const score = contrastScore(color, surface, ink) - hueDistance(hue, targetHue) * 2;
    if (score > selectedScore) {
      selected = color;
      selectedScore = score;
    }
  }
  return selected;
}

function allThemeColors(theme) {
  const values = [
    ...Object.values(theme.colors ?? {}),
    ...themeTokens(theme).map((token) => token.settings?.foreground),
  ];
  const colors = new Set();
  for (const value of values) {
    const color = normalizeThemeColor(value, {
      minimumAlpha: 0.45,
      minimumChromaticRange: 24,
    });
    if (color != null) colors.add(color);
  }
  return colors;
}

function themeTokens(theme) {
  return [...(theme.tokenColors ?? []), ...(theme.settings ?? [])];
}

function normalizeThemeColor(value, options = {}) {
  const parsed = parseHexColor(value);
  if (parsed == null) return undefined;
  const minimumAlpha = options.minimumAlpha ?? 0.98;
  const minimumChromaticRange = options.minimumChromaticRange ?? 0;
  if (parsed.alpha < minimumAlpha || chromaticRange(parsed) < minimumChromaticRange) {
    return undefined;
  }
  return colorToHex(parsed);
}

function parseHexColor(value) {
  if (value == null || typeof value !== "string") return undefined;
  const color = value.trim();
  if (!/^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(color)) return undefined;
  const alpha = color.length === 9 ? color.slice(7, 9) : "ff";
  return {
    alpha: Number.parseInt(alpha, 16) / 255,
    blue: Number.parseInt(color.slice(5, 7), 16),
    green: Number.parseInt(color.slice(3, 5), 16),
    red: Number.parseInt(color.slice(1, 3), 16),
  };
}

function colorsAreClose(left, right) {
  const leftColor = parseHexColor(left);
  const rightColor = parseHexColor(right);
  return leftColor != null && rightColor != null && colorDistance(leftColor, rightColor) < 42;
}

function contrastScore(color, surface, ink) {
  const parsedColor = parseHexColor(color);
  const parsedSurface = parseHexColor(surface);
  const parsedInk = parseHexColor(ink);
  if (parsedColor == null || parsedSurface == null || parsedInk == null) return 0;
  return chromaticRange(parsedColor)
    + colorDistance(parsedColor, parsedSurface) / 4
    + colorDistance(parsedColor, parsedInk) / 4;
}

function colorDistance(left, right) {
  return Math.sqrt(
    (left.red - right.red) ** 2
    + (left.green - right.green) ** 2
    + (left.blue - right.blue) ** 2,
  );
}

function colorHue(color) {
  const red = color.red / 255;
  const green = color.green / 255;
  const blue = color.blue / 255;
  const max = Math.max(red, green, blue);
  const delta = max - Math.min(red, green, blue);
  if (delta === 0) return null;
  const hue = max === red
    ? (((green - blue) / delta) % 6) * 60
    : max === green
      ? ((blue - red) / delta + 2) * 60
      : ((red - green) / delta + 4) * 60;
  return (hue + 360) % 360;
}

function hueInRange(hue, range) {
  return range.min <= range.max
    ? hue >= range.min && hue <= range.max
    : hue >= range.min || hue <= range.max;
}

function hueDistance(left, right) {
  const distance = Math.abs(left - right);
  return Math.min(distance, 360 - distance);
}

function chromaticRange(color) {
  return Math.max(color.red, color.green, color.blue)
    - Math.min(color.red, color.green, color.blue);
}

function colorToHex(color) {
  return `#${hexPair(color.red)}${hexPair(color.green)}${hexPair(color.blue)}`;
}

function hexPair(value) {
  return value.toString(16).padStart(2, "0");
}
