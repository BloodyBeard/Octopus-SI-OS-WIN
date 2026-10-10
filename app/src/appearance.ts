import { useSyncExternalStore } from "react";
import codeThemeSeedData from "./design-system/code-theme-seeds.json";
import octopusAccentProfile from "./design-system/accent-profile.json";

export type ThemeMode = "system" | "light" | "dark";
export type AccentSource = "codex" | "custom";
export type MotionMode = "system" | "reduce" | "full";
export type Density = "compact" | "default" | "comfortable";
export type ChatTextSize = "small" | "default" | "large";
export type CornerSoftness = "sharp" | "default" | "round";
export type DiffMarkerStyle = "color" | "symbols";
export type CodeThemeId =
  | "absolutely"
  | "ayu"
  | "catppuccin"
  | "codex"
  | "dracula"
  | "everforest"
  | "github"
  | "gruvbox"
  | "linear"
  | "lobster"
  | "material"
  | "matrix"
  | "monokai"
  | "night-owl"
  | "nord"
  | "notion"
  | "one"
  | "oscurange"
  | "proof"
  | "raycast"
  | "rose-pine"
  | "sentry"
  | "solarized"
  | "temple"
  | "tokyo-night"
  | "vercel"
  | "vscode-plus"
  | "xcode";

export type CodeThemeDefinition = Readonly<{
  id: CodeThemeId;
  label: string;
  background: string;
  accent: string;
  foreground: string;
}>;

export type CodeThemeKind = "light" | "dark";
export type CodeThemeSeed = Readonly<{
  accent: string;
  ink: string;
  surface: string;
  contrast?: number;
  fonts?: Readonly<{ code?: string | null; ui?: string | null }>;
  opaqueWindows?: boolean;
  semanticColors: Readonly<{
    diffAdded: string;
    diffRemoved: string;
    skill: string;
  }>;
}>;

const codeThemeSeeds = codeThemeSeedData as unknown as Record<
  CodeThemeKind,
  Partial<Record<CodeThemeId, CodeThemeSeed>>
>;

export const lightCodeThemes = codeThemeDefinitions("light", [
  ["absolutely", "Absolutely"], ["catppuccin", "Catppuccin"], ["codex", "Codex"],
  ["everforest", "Everforest"], ["github", "GitHub"], ["gruvbox", "Gruvbox"],
  ["linear", "Linear"], ["notion", "Notion"], ["one", "One"], ["proof", "Proof"],
  ["raycast", "Raycast"], ["rose-pine", "Rose Pine"], ["solarized", "Solarized"],
  ["vercel", "Vercel"], ["vscode-plus", "VS Code Plus"], ["xcode", "Xcode"],
]);

export const darkCodeThemes = codeThemeDefinitions("dark", [
  ["absolutely", "Absolutely"], ["ayu", "Ayu"], ["catppuccin", "Catppuccin"],
  ["codex", "Codex"], ["dracula", "Dracula"], ["everforest", "Everforest"],
  ["github", "GitHub"], ["gruvbox", "Gruvbox"], ["linear", "Linear"],
  ["lobster", "Lobster"], ["material", "Material"], ["matrix", "Matrix"],
  ["monokai", "Monokai"], ["night-owl", "Night Owl"], ["nord", "Nord"],
  ["notion", "Notion"], ["one", "One"], ["oscurange", "Oscurange"],
  ["raycast", "Raycast"], ["rose-pine", "Rose Pine"], ["sentry", "Sentry"],
  ["solarized", "Solarized"], ["temple", "Temple"], ["tokyo-night", "Tokyo Night"],
  ["vercel", "Vercel"], ["vscode-plus", "VS Code Plus"], ["xcode", "Xcode"],
]);

export function codeThemeSeed(kind: CodeThemeKind, id: CodeThemeId): CodeThemeSeed {
  const seed = codeThemeSeeds[kind][id];
  if (!seed) throw new Error(`Code theme ${id} is unavailable for ${kind} mode.`);
  return seed;
}

function codeThemeDefinitions(
  kind: CodeThemeKind,
  values: ReadonlyArray<readonly [CodeThemeId, string]>,
): readonly CodeThemeDefinition[] {
  return Object.freeze(values.map(([id, label]) => {
    const seed = codeThemeSeed(kind, id);
    return {
      id,
      label,
      background: seed.surface,
      accent: seed.accent,
      foreground: seed.ink,
    };
  }));
}

const lightCodeThemeIds = lightCodeThemes.map((theme) => theme.id);
const darkCodeThemeIds = darkCodeThemes.map((theme) => theme.id);

export type AppearanceSettings = {
  themeMode: ThemeMode;
  lightAccent: string;
  lightAccentSource: AccentSource;
  lightBackground: string;
  lightForeground: string;
  lightDiffAdded: string;
  lightDiffRemoved: string;
  lightSkill: string;
  lightTranslucentSidebar: boolean;
  lightContrast: number;
  lightUiFont: string;
  lightCodeFont: string;
  darkAccent: string;
  darkAccentSource: AccentSource;
  darkBackground: string;
  darkForeground: string;
  darkDiffAdded: string;
  darkDiffRemoved: string;
  darkSkill: string;
  darkTranslucentSidebar: boolean;
  darkContrast: number;
  darkUiFont: string;
  darkCodeFont: string;
  lightCodeTheme: CodeThemeId;
  darkCodeTheme: CodeThemeId;
  uiFontSize: number;
  codeFontSize: number;
  pointerCursors: boolean;
  motionMode: MotionMode;
  diffMarkerStyle: DiffMarkerStyle;
  fontSmoothing: boolean;
  density: Density;
  chatTextSize: ChatTextSize;
  cornerSoftness: CornerSoftness;
};

const STORAGE_KEY = "octopus.appearance.v3";

export const defaultAppearance: AppearanceSettings = {
  themeMode: "system",
  lightAccent: "#339CFF",
  lightAccentSource: "codex",
  lightBackground: "#FFFFFF",
  lightForeground: "#1A1C1F",
  lightDiffAdded: "#00A240",
  lightDiffRemoved: "#BA2623",
  lightSkill: "#924FF7",
  lightTranslucentSidebar: true,
  lightContrast: 45,
  lightUiFont: "",
  lightCodeFont: "",
  darkAccent: "#339CFF",
  darkAccentSource: "codex",
  darkBackground: "#181818",
  darkForeground: "#FFFFFF",
  darkDiffAdded: "#40C977",
  darkDiffRemoved: "#FA423E",
  darkSkill: "#AD7BF9",
  darkTranslucentSidebar: true,
  darkContrast: 60,
  darkUiFont: "",
  darkCodeFont: "",
  lightCodeTheme: "codex",
  darkCodeTheme: "codex",
  uiFontSize: 14,
  codeFontSize: 12,
  pointerCursors: false,
  motionMode: "system",
  diffMarkerStyle: "color",
  fontSmoothing: true,
  density: "default",
  chatTextSize: "default",
  cornerSoftness: "default",
};

const densityScale: Record<Density, number> = { compact: 0.84, default: 1, comfortable: 1.16 };
const chatFontPx: Record<ChatTextSize, number> = { small: 13, default: 14, large: 16 };
const cornerScale: Record<CornerSoftness, number> = { sharp: 0.64, default: 1, round: 1.32 };
const spacingTokens: Record<string, number> = {
  "--oc-space-1": 2,
  "--oc-space-2": 4,
  "--oc-space-3": 6,
  "--oc-space-4": 8,
  "--oc-space-5": 12,
  "--oc-space-6": 16,
  "--oc-space-7": 20,
  "--oc-space-8": 24,
  "--oc-space-9": 32,
  "--oc-control-px-xs": 7,
  "--oc-control-px-sm": 10,
  "--oc-control-px-md": 12,
};
const controlHeightTokens: Record<string, number> = {
  "--oc-control-h-xs": 24,
  "--oc-control-h-sm": 28,
  "--oc-control-h-md": 32,
};
const radiusTokens: Record<string, number> = {
  "--oc-radius-xs": 4,
  "--oc-radius-sm": 7,
  "--oc-radius-md": 10,
  "--oc-radius-lg": 14,
  "--oc-radius-card": 20,
};
const uiFontBases = [
  6.8, 7, 7.2, 7.3, 7.4, 7.5, 7.6, 7.7, 7.8, 8, 8.2, 8.4, 8.5, 8.6, 8.8,
  9, 9.2, 9.5, 10, 10.5, 11, 11.5, 12, 12.5, 13, 13.5, 14, 14.5, 15, 16, 17,
  18, 19, 20, 22, 24, 25, 26,
];
const inlineVscodeTypographyVariables = new Set([
  "--vscode-chat-editor-font-size",
  "--vscode-chat-font-size",
  "--vscode-editor-font-family",
  "--vscode-editor-font-size",
  "--vscode-font-family",
  "--vscode-font-size",
]);
const listeners = new Set<() => void>();
let observersInstalled = false;
let current = load();

function load(): AppearanceSettings {
  try {
    if (typeof window === "undefined") return { ...defaultAppearance };
    const raw = window.localStorage.getItem(STORAGE_KEY)
      ?? window.localStorage.getItem("octopus.appearance.v2")
      ?? window.localStorage.getItem("octopus.appearance.v1");
    if (!raw) return { ...defaultAppearance };
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const legacyReduced = parsed.reduceMotion === true ? "reduce" : defaultAppearance.motionMode;
    const legacyUiFont = stringValue(parsed.uiFont);
    const legacyCodeFont = stringValue(parsed.codeFont);
    return {
      themeMode: choice(parsed.themeMode, ["system", "light", "dark"], defaultAppearance.themeMode),
      lightAccent: color(parsed.lightAccent, defaultAppearance.lightAccent),
      lightAccentSource: readAccentSource(parsed, "light"),
      lightBackground: color(parsed.lightBackground, defaultAppearance.lightBackground),
      lightForeground: color(parsed.lightForeground, defaultAppearance.lightForeground),
      lightDiffAdded: color(parsed.lightDiffAdded, defaultAppearance.lightDiffAdded),
      lightDiffRemoved: color(parsed.lightDiffRemoved, defaultAppearance.lightDiffRemoved),
      lightSkill: color(parsed.lightSkill, defaultAppearance.lightSkill),
      lightTranslucentSidebar: booleanValue(parsed.lightTranslucentSidebar, defaultAppearance.lightTranslucentSidebar),
      lightContrast: numberValue(parsed.lightContrast, defaultAppearance.lightContrast, 0, 100),
      lightUiFont: stringValue(parsed.lightUiFont) || legacyUiFont,
      lightCodeFont: stringValue(parsed.lightCodeFont) || legacyCodeFont,
      darkAccent: color(parsed.darkAccent, defaultAppearance.darkAccent),
      darkAccentSource: readAccentSource(parsed, "dark"),
      darkBackground: color(parsed.darkBackground, defaultAppearance.darkBackground),
      darkForeground: color(parsed.darkForeground, defaultAppearance.darkForeground),
      darkDiffAdded: color(parsed.darkDiffAdded, defaultAppearance.darkDiffAdded),
      darkDiffRemoved: color(parsed.darkDiffRemoved, defaultAppearance.darkDiffRemoved),
      darkSkill: color(parsed.darkSkill, defaultAppearance.darkSkill),
      darkTranslucentSidebar: booleanValue(parsed.darkTranslucentSidebar, defaultAppearance.darkTranslucentSidebar),
      darkContrast: numberValue(parsed.darkContrast, defaultAppearance.darkContrast, 0, 100),
      darkUiFont: stringValue(parsed.darkUiFont) || legacyUiFont,
      darkCodeFont: stringValue(parsed.darkCodeFont) || legacyCodeFont,
      lightCodeTheme: choice(parsed.lightCodeTheme, lightCodeThemeIds, defaultAppearance.lightCodeTheme),
      darkCodeTheme: choice(parsed.darkCodeTheme, darkCodeThemeIds, defaultAppearance.darkCodeTheme),
      uiFontSize: numberValue(parsed.uiFontSize, defaultAppearance.uiFontSize, 11, 20),
      codeFontSize: numberValue(parsed.codeFontSize, defaultAppearance.codeFontSize, 10, 20),
      pointerCursors: booleanValue(parsed.pointerCursors, defaultAppearance.pointerCursors),
      motionMode: choice(parsed.motionMode, ["system", "reduce", "full"], legacyReduced),
      diffMarkerStyle: choice(parsed.diffMarkerStyle, ["color", "symbols"], defaultAppearance.diffMarkerStyle),
      fontSmoothing: booleanValue(parsed.fontSmoothing, defaultAppearance.fontSmoothing),
      density: choice(parsed.density, ["compact", "default", "comfortable"], defaultAppearance.density),
      chatTextSize: choice(parsed.chatTextSize, ["small", "default", "large"], defaultAppearance.chatTextSize),
      cornerSoftness: choice(parsed.cornerSoftness, ["sharp", "default", "round"], defaultAppearance.cornerSoftness),
    };
  } catch {
    return { ...defaultAppearance };
  }
}

function readAccentSource(value: Record<string, unknown>, mode: CodeThemeKind): AccentSource {
  const native = color(value[`${mode}Accent`], defaultAppearance[`${mode}Accent`]).toLowerCase() === defaultAppearance[`${mode}Accent`].toLowerCase()
    && color(value[`${mode}Background`], defaultAppearance[`${mode}Background`]).toLowerCase() === defaultAppearance[`${mode}Background`].toLowerCase()
    && color(value[`${mode}Foreground`], defaultAppearance[`${mode}Foreground`]).toLowerCase() === defaultAppearance[`${mode}Foreground`].toLowerCase()
    && numberValue(value[`${mode}Contrast`], defaultAppearance[`${mode}Contrast`], 0, 100) === defaultAppearance[`${mode}Contrast`];
  return choice(value[`${mode}AccentSource`], ["codex", "custom"], native ? "codex" : "custom");
}

export function appearanceAccentColor(settings: AppearanceSettings, mode: CodeThemeKind): string {
  return settings[`${mode}AccentSource`] === "codex" ? octopusAccentProfile[mode].accent : settings[`${mode}Accent`];
}

export function applyAppearance(settings: AppearanceSettings = current): void {
  if (typeof document === "undefined") return;
  installObservers();
  const root = document.documentElement;
  const theme = resolveTheme(settings.themeMode);
  const palette = theme === "dark"
    ? {
        accent: appearanceAccentColor(settings, "dark"),
        background: settings.darkBackground,
        foreground: settings.darkForeground,
        diffAdded: settings.darkDiffAdded,
        diffRemoved: settings.darkDiffRemoved,
        skill: settings.darkSkill,
        contrast: settings.darkContrast,
        translucentSidebar: settings.darkTranslucentSidebar,
        uiFont: settings.darkUiFont,
        codeFont: settings.darkCodeFont,
      }
    : {
        accent: appearanceAccentColor(settings, "light"),
        background: settings.lightBackground,
        foreground: settings.lightForeground,
        diffAdded: settings.lightDiffAdded,
        diffRemoved: settings.lightDiffRemoved,
        skill: settings.lightSkill,
        contrast: settings.lightContrast,
        translucentSidebar: settings.lightTranslucentSidebar,
        uiFont: settings.lightUiFont,
        codeFont: settings.lightCodeFont,
      };
  const reduced = shouldReduceMotion(settings);
  const uiScale = settings.uiFontSize / defaultAppearance.uiFontSize;
  const foregroundRgb = hexRgb(palette.foreground);
  const octopusTheme = resolveOctopusTheme(theme, palette, settings[`${theme}AccentSource`] === "codex");
  const selectedCodeThemeId = theme === "dark" ? settings.darkCodeTheme : settings.lightCodeTheme;
  const codeThemes = theme === "dark" ? darkCodeThemes : lightCodeThemes;
  const selectedCodeTheme = codeThemes.find((candidate) => candidate.id === selectedCodeThemeId)
    ?? codeThemes.find((candidate) => candidate.id === "codex")
    ?? codeThemes[0];

  root.dataset.theme = theme;
  root.dataset.themePreference = settings.themeMode;
  root.dataset.codeTheme = selectedCodeTheme.id;
  root.dataset.diffMarkerStyle = settings.diffMarkerStyle;
  root.dataset.density = settings.density;
  root.dataset.octopusOs = "darwin";
  root.dataset.octopusWindowType = "electron";
  root.dataset.windowType = "electron";
  root.dataset.octopusWindowChrome = "native";
  root.classList.toggle("electron-dark", theme === "dark");
  root.classList.toggle("electron-light", theme === "light");
  root.classList.toggle("reduce-motion", reduced);
  root.classList.toggle("full-motion", settings.motionMode === "full");
  root.classList.toggle("pointer-cursors", settings.pointerCursors);
  root.classList.toggle("font-smoothing", settings.fontSmoothing);
  root.classList.toggle("translucent-sidebar", palette.translucentSidebar);
  window.octopusTransport?.setWindowAppearance({ theme, preference: settings.themeMode, translucent: palette.translucentSidebar });
  root.style.setProperty("--ui-font-scale", String(uiScale));
  root.style.setProperty("--chat-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--octopus-chat-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--octopus-chat-code-font-size", `${settings.codeFontSize}px`);
  root.style.setProperty("--vscode-chat-editor-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--vscode-editor-font-size", `${settings.codeFontSize}px`);
  root.style.setProperty("--vscode-chat-font-size", `${chatFontPx[settings.chatTextSize]}px`);
  root.style.setProperty("--corner-scale", String(cornerScale[settings.cornerSoftness]));
  root.style.setProperty("--ui-font-size", `${settings.uiFontSize}px`);
  root.style.setProperty("--code-font-size", `${settings.codeFontSize}px`);
  const textSizes: Record<string, number> = {
    "--text-4xl": 72,
    "--text-3xl": 48,
    "--text-2xl": 36,
    "--text-xl": 28,
    "--text-lg": 16,
    "--text-base": defaultAppearance.uiFontSize,
    "--text-sm": defaultAppearance.uiFontSize - 1,
    "--text-xs": defaultAppearance.uiFontSize - 2,
    "--text-heading-lg": 24,
    "--text-heading-md": 20,
    "--text-heading-sm": 18,
  };
  for (const element of [root, document.body]) {
    element.style.setProperty("--vscode-font-size", `${settings.uiFontSize}px`);
    for (const [name, size] of Object.entries(textSizes)) {
      element.style.setProperty(name, `${Math.round(size * uiScale)}px`);
    }
  }
  root.style.setProperty("--oc-type-xs", scaledPx(10, uiScale));
  root.style.setProperty("--oc-type-sm", scaledPx(12, uiScale));
  root.style.setProperty("--oc-type-md", scaledPx(14, uiScale));
  root.style.setProperty("--oc-type-lg", scaledPx(16, uiScale));
  root.style.setProperty("--oc-type-xl", scaledPx(20, uiScale));
  for (const base of uiFontBases) {
    root.style.setProperty(`--ui-font-${String(base).replace(".", "_")}`, scaledPx(base, uiScale));
  }
  setScaledTokens(root, spacingTokens, densityScale[settings.density]);
  setScaledTokens(root, controlHeightTokens, Math.max(densityScale[settings.density], uiScale * 0.82));
  setScaledTokens(root, radiusTokens, cornerScale[settings.cornerSoftness]);
  const uiFont = [palette.uiFont, '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif'].filter(Boolean).join(", ");
  const codeFont = [palette.codeFont, 'ui-monospace, "SFMono-Regular", "SF Mono", Menlo, Consolas, "Liberation Mono", monospace'].filter(Boolean).join(", ");
  for (const element of [root, document.body]) {
    element.style.setProperty("--font-ui", uiFont);
    element.style.setProperty("--font-mono", codeFont);
    if (palette.uiFont) element.style.setProperty("--vscode-font-family", uiFont);
    else element.style.removeProperty("--vscode-font-family");
    if (palette.codeFont) element.style.setProperty("--vscode-editor-font-family", codeFont);
    else element.style.removeProperty("--vscode-editor-font-family");
  }
  clearDerivedOctopusVariables(root);
  for (const [name, value] of Object.entries(octopusTheme.variables)) {
    root.style.setProperty(name, value);
  }
  root.style.setProperty("--code-theme-background", selectedCodeTheme.background);
  root.style.setProperty("--code-theme-accent", selectedCodeTheme.accent);
  root.style.setProperty("--code-theme-foreground", selectedCodeTheme.foreground);
  root.style.setProperty("--oc-color-canvas", palette.background);
  root.style.setProperty("--oc-color-sidebar", palette.background);
  root.style.setProperty("--oc-color-surface", octopusTheme.elevatedSecondaryOpaque);
  root.style.setProperty("--oc-color-card", octopusTheme.elevatedPrimaryOpaque);
  root.style.setProperty("--oc-color-raised", octopusTheme.controlBackgroundOpaque);
  root.style.setProperty("--oc-color-control", octopusTheme.buttonSecondaryBackground);
  root.style.setProperty("--oc-color-control-hover", octopusTheme.buttonSecondaryBackgroundHover);
  root.style.setProperty("--oc-color-control-active", octopusTheme.buttonSecondaryBackgroundActive);
  root.style.setProperty("--oc-color-text", octopusTheme.textForeground);
  root.style.setProperty("--oc-color-text-soft", octopusTheme.textForegroundSecondary);
  root.style.setProperty("--oc-color-text-muted", octopusTheme.textForegroundTertiary);
  root.style.setProperty("--oc-color-border", octopusTheme.border);
  root.style.setProperty("--oc-color-border-strong", octopusTheme.borderHeavy);
  root.style.setProperty("--oc-color-accent", palette.accent);
  root.style.setProperty("--oc-color-accent-hover", mixHex(palette.accent, palette.foreground, 0.12));
  root.style.setProperty("--oc-color-danger", theme === "dark" ? "#FF6764" : "#E02E2A");
  root.style.setProperty("--oc-focus-color", rgba(hexRgb(palette.accent), 0.9));
  root.style.setProperty("--bg", palette.background);
  root.style.setProperty("--bg-deep", palette.background);
  root.style.setProperty("--stage", palette.background);
  root.style.setProperty("--workspace-material", palette.translucentSidebar
    ? colorAlpha(rgbColor(palette.background), 0.9)
    : palette.background);
  root.style.setProperty("--welcome-material", palette.translucentSidebar
    ? colorAlpha(rgbColor(palette.background), 0.88)
    : palette.background);
  root.style.setProperty("--sidebar", palette.translucentSidebar
    ? colorAlpha(octopusTheme.editorBackground, 0.78)
    : rgbString(octopusTheme.editorBackground));
  root.style.setProperty("--sidebar-soft", colorAlpha(octopusTheme.editorBackground, 0.78));
  root.style.setProperty("--panel", octopusTheme.elevatedSecondaryOpaque);
  root.style.setProperty("--panel-soft", octopusTheme.elevatedPrimaryOpaque);
  root.style.setProperty("--panel-raised", octopusTheme.controlBackgroundOpaque);
  root.style.setProperty("--field", rgba(foregroundRgb, 0.035));
  root.style.setProperty("--line", rgba(foregroundRgb, 0.084));
  root.style.setProperty("--line-light", rgba(foregroundRgb, 0.042));
  root.style.setProperty("--line-strong", rgba(foregroundRgb, 0.156));
  root.style.setProperty("--line-bright", rgba(foregroundRgb, 0.29));
  root.style.setProperty("--text", octopusTheme.textForeground);
  root.style.setProperty("--text-soft", rgba(foregroundRgb, 0.72));
  root.style.setProperty("--muted", rgba(foregroundRgb, 0.62));
  root.style.setProperty("--copy", rgba(foregroundRgb, 0.62));
  root.style.setProperty("--copy-dim", rgba(foregroundRgb, 0.5));
  root.style.setProperty("--dim", rgba(foregroundRgb, 0.42));
  root.style.setProperty("--faint", rgba(foregroundRgb, 0.36));
  root.style.setProperty("--text-muted", rgba(foregroundRgb, 0.62));
  root.style.setProperty("--text-faint", rgba(foregroundRgb, 0.42));
  root.style.setProperty("--icon", rgba(foregroundRgb, 0.9));
  root.style.setProperty("--icon-muted", rgba(foregroundRgb, 0.7));
  root.style.setProperty("--icon-dim", rgba(foregroundRgb, 0.5));
  root.style.setProperty("--button-secondary", rgba(foregroundRgb, 0.05));
  root.style.setProperty("--button-secondary-hover", rgba(foregroundRgb, 0.08));
  root.style.setProperty("--button-secondary-active", rgba(foregroundRgb, 0.12));
  root.style.setProperty("--row-hover", rgba(foregroundRgb, 0.055));
  root.style.setProperty("--row-active", rgba(foregroundRgb, 0.11));
  root.style.setProperty("--surface-0", octopusTheme.surfaceUnder);
  root.style.setProperty("--surface-1", palette.background);
  root.style.setProperty("--surface-2", octopusTheme.elevatedSecondaryOpaque);
  root.style.setProperty("--surface-3", octopusTheme.elevatedPrimaryOpaque);
  root.style.setProperty("--blue", palette.accent);
}

type OctopusPalette = {
  accent: string;
  background: string;
  foreground: string;
  diffAdded: string;
  diffRemoved: string;
  skill: string;
  contrast: number;
  translucentSidebar: boolean;
  uiFont: string;
  codeFont: string;
};

type RgbColor = { red: number; green: number; blue: number };

function resolveOctopusTheme(theme: "light" | "dark", palette: OctopusPalette, linkedAccent = false) {
  const contrast = octopusContrast(palette.contrast, theme);
  const surface = rgbColor(palette.background);
  const ink = rgbColor(palette.foreground);
  const accent = rgbColor(palette.accent);
  const black = rgbColor("#000000");
  const white = rgbColor("#ffffff");
  const editorBackground = theme === "light"
    ? mixRgb(surface, white, 0.12)
    : mixRgb(surface, ink, 0.07);
  const applicationMenu = mixRgb(surface, ink, 0.02 + contrast * 0.02);
  const composerUtilityBackground = theme === "light"
    ? mixRgb(surface, ink, 0.055 + contrast * 0.025)
    : mixRgb(surface, ink, 0.02 + contrast * 0.02);
  const surfaceUnderRatio = theme === "light"
    ? 0.04 + (palette.contrast - 45) * 0.0012
    : 0.16 + (palette.contrast - 60) * 0.0015;
  const surfaceUnder = colorHex(mixRgb(surface, theme === "light" ? ink : black, surfaceUnderRatio));
  const panel = colorHex(mixRgb(
    surface,
    theme === "light" ? white : ink,
    (theme === "light" ? 0.18 : 0.03) + contrast * (theme === "light" ? 0.008 : 0.03),
  ));
  const skill = color(palette.skill, theme === "light" ? "#924FF7" : "#AD7BF9").toLowerCase();
  const diffAdded = color(palette.diffAdded, theme === "light" ? "#00A240" : "#40C977").toLowerCase();
  const diffRemoved = color(palette.diffRemoved, theme === "light" ? "#BA2623" : "#FA423E").toLowerCase();
  const values = theme === "light"
    ? octopusLightColors(surface, ink, accent, contrast)
    : octopusDarkColors(surface, ink, accent, contrast);
  if (theme === "dark" && isDefaultDarkPalette(palette)) {
    values.textForeground = "var(--gray-150)";
  }
  const accentHex = colorHex(accent);
  const foregroundHex = colorHex(ink);
  const surfaceHex = colorHex(surface);
  const variables: Record<string, string> = {
    "--color-background-composer-primary": "var(--color-background-primary-solid)",
    "--color-text-composer-primary": "var(--color-text-primary-solid)",
    "--color-background-user-message": "color-mix(in oklab, var(--color-text) 5%, transparent)",
    "--color-text-user-message": "var(--color-text)",
    "--color-background-text-selection": "var(--color-background-info-soft)",
    "--color-background-attribution-highlight": "var(--color-background-info-soft)",
    "--octopus-base-accent": accentHex,
    "--octopus-base-contrast": String(palette.contrast),
    "--octopus-base-ink": foregroundHex,
    "--octopus-base-surface": surfaceHex,
    "--color-accent-blue": accentHex,
    "--color-accent-purple": skill,
    "--color-background-accent": values.accentBackground,
    "--color-background-accent-active": values.accentBackgroundActive,
    "--color-background-accent-hover": values.accentBackgroundHover,
    "--color-background-application-menu": colorHex(applicationMenu),
    "--color-background-button-primary": values.buttonPrimaryBackground,
    "--color-background-button-primary-active": values.buttonPrimaryBackgroundActive,
    "--color-background-button-primary-hover": values.buttonPrimaryBackgroundHover,
    "--color-background-button-primary-inactive": values.buttonPrimaryBackgroundInactive,
    "--color-background-button-secondary": values.buttonSecondaryBackground,
    "--color-background-button-secondary-active": values.buttonSecondaryBackgroundActive,
    "--color-background-button-secondary-hover": values.buttonSecondaryBackgroundHover,
    "--color-background-button-secondary-inactive": values.buttonSecondaryBackgroundInactive,
    "--color-background-button-tertiary": values.buttonTertiaryBackground,
    "--color-background-button-tertiary-active": values.buttonTertiaryBackgroundActive,
    "--color-background-button-tertiary-hover": values.buttonTertiaryBackgroundHover,
    "--color-background-control": values.controlBackground,
    "--color-background-control-opaque": values.controlBackgroundOpaque,
    "--color-background-callout-surface": values.elevatedPrimary,
    "--color-background-composer-action-bar": theme === "light" ? surfaceUnder : values.elevatedSecondary,
    "--color-token-composer-utility-background": rgbString(composerUtilityBackground),
    "--color-background-editor-opaque": rgbString(editorBackground),
    "--color-background-elevated-primary": values.elevatedPrimary,
    "--color-background-elevated-primary-opaque": values.elevatedPrimaryOpaque,
    "--color-background-elevated-secondary": values.elevatedSecondary,
    "--color-background-elevated-secondary-opaque": values.elevatedSecondaryOpaque,
    "--color-background-mode-toggle-track": theme === "light" ? surfaceUnder : values.buttonTertiaryBackgroundHover,
    "--color-background-mode-toggle-selected": theme === "dark" ? values.controlBackgroundOpaque : `color-mix(in oklab, ${values.controlBackground} 90%, transparent)`,
    "--color-background-panel": panel,
    "--color-background-surface": surfaceHex,
    "--color-background-surface-under": surfaceUnder,
    "--color-border": values.border,
    "--color-border-application-menu-separator": theme === "dark" ? colorHex(mixRgb(applicationMenu, ink, 0.28)) : values.borderHeavy,
    "--color-border-focus": values.borderFocus,
    "--color-border-heavy": values.borderHeavy,
    "--color-border-light": values.borderLight,
    "--color-border-mode-toggle-selected": values.border,
    "--color-decoration-added": diffAdded,
    "--color-decoration-deleted": diffRemoved,
    "--color-editor-added": colorAlpha(rgbColor(diffAdded), theme === "light" ? 0.15 : 0.23),
    "--color-editor-deleted": colorAlpha(rgbColor(diffRemoved), theme === "light" ? 0.15 : 0.23),
    "--color-icon-accent": values.iconAccent,
    "--color-icon-primary": values.iconPrimary,
    "--color-icon-secondary": values.iconSecondary,
    "--color-icon-tertiary": values.iconTertiary,
    "--color-simple-scrim": values.simpleScrim,
    "--color-text-accent": values.textAccent,
    "--color-text-on-accent": textOnAccent(accent),
    "--color-text-button-primary": values.textButtonPrimary,
    "--color-text-button-secondary": values.textButtonSecondary,
    "--color-text-button-tertiary": values.textButtonTertiary,
    "--color-foreground-application-menu": theme === "dark" ? colorHex(mixRgb(applicationMenu, ink, 0.875)) : values.textForeground,
    "--color-text-foreground": values.textForeground,
    "--color-text-foreground-secondary": values.textForegroundSecondary,
    "--color-text-foreground-tertiary": values.textForegroundTertiary,
    "--color-text-mode-toggle-inactive": `color-mix(in oklab, ${values.textForeground} 80%, transparent)`,
    "--shadow-mode-toggle-selected": "var(--shadow-md)",
  };
  if (linkedAccent) {
    const profile = octopusAccentProfile[theme];
    Object.assign(variables, {
      "--color-background-composer-primary": profile.submitBackground,
      "--color-text-composer-primary": profile.submitText,
      "--color-background-user-message": profile.userMessageBackground,
      "--color-text-user-message": profile.userMessageText,
      "--color-background-text-selection": profile.selection,
      "--color-background-attribution-highlight": profile.attributionHighlight,
      "--color-background-accent": profile.soft,
      "--color-background-accent-active": profile.soft,
      "--color-background-accent-hover": profile.soft,
      "--color-border-focus": profile.accent,
      "--color-icon-accent": profile.text,
      "--color-text-accent": profile.text,
    });
  }
  return { ...values, surfaceUnder, editorBackground, variables };
}

function isDefaultDarkPalette(palette: OctopusPalette): boolean {
  return palette.accent.toLowerCase() === defaultAppearance.darkAccent.toLowerCase()
    && palette.background.toLowerCase() === defaultAppearance.darkBackground.toLowerCase()
    && palette.foreground.toLowerCase() === defaultAppearance.darkForeground.toLowerCase()
    && palette.contrast === defaultAppearance.darkContrast
    && palette.uiFont === defaultAppearance.darkUiFont
    && palette.codeFont === defaultAppearance.darkCodeFont
    && palette.translucentSidebar === defaultAppearance.darkTranslucentSidebar
    && palette.diffAdded.toLowerCase() === defaultAppearance.darkDiffAdded.toLowerCase()
    && palette.diffRemoved.toLowerCase() === defaultAppearance.darkDiffRemoved.toLowerCase()
    && palette.skill.toLowerCase() === defaultAppearance.darkSkill.toLowerCase();
}

function clearDerivedOctopusVariables(root: HTMLElement): void {
  for (const name of Array.from(root.style)) {
    const derivedToken = name.startsWith("--color-token-");
    const derivedVscode = name.startsWith("--vscode-") && !inlineVscodeTypographyVariables.has(name);
    if (derivedToken || derivedVscode) root.style.removeProperty(name);
  }
}

function octopusLightColors(surface: RgbColor, ink: RgbColor, accent: RgbColor, contrast: number) {
  const white = { red: 255, green: 255, blue: 255 };
  const control = mixRgb(surface, white, 0.09 + contrast * 0.04);
  const elevatedSecondary = mixRgb(surface, white, 0.08 + contrast * 0.08);
  const elevatedPrimary = mixRgb(surface, white, 0.16 + contrast * 0.12);
  return {
    accentBackground: colorHex(mixRgb(surface, accent, 0.11 + contrast * 0.04)),
    accentBackgroundActive: colorHex(mixRgb(surface, accent, 0.13 + contrast * 0.05)),
    accentBackgroundHover: colorHex(mixRgb(surface, accent, 0.12 + contrast * 0.045)),
    border: colorAlpha(ink, 0.06 + contrast * 0.04),
    borderFocus: colorHex(accent),
    borderHeavy: colorAlpha(ink, 0.09 + contrast * 0.06),
    borderLight: colorAlpha(ink, 0.04 + contrast * 0.02),
    buttonPrimaryBackground: colorHex(ink),
    buttonPrimaryBackgroundActive: colorAlpha(ink, 0.1 + contrast * 0.12),
    buttonPrimaryBackgroundHover: colorAlpha(ink, 0.05 + contrast * 0.06),
    buttonPrimaryBackgroundInactive: colorAlpha(ink, 0.18 + contrast * 0.14),
    buttonSecondaryBackground: colorAlpha(ink, 0.04 + contrast * 0.02),
    buttonSecondaryBackgroundActive: colorAlpha(ink, 0.03 + contrast * 0.02),
    buttonSecondaryBackgroundHover: colorAlpha(ink, 0.04 + contrast * 0.03),
    buttonSecondaryBackgroundInactive: colorAlpha(ink, 0.01 + contrast * 0.02),
    buttonTertiaryBackground: colorAlpha(ink, 0),
    buttonTertiaryBackgroundActive: colorAlpha(ink, 0.16 + contrast * 0.08),
    buttonTertiaryBackgroundHover: colorAlpha(ink, 0.08 + contrast * 0.04),
    controlBackground: colorAlpha(control, 0.96),
    controlBackgroundOpaque: rgbString(control),
    elevatedPrimary: colorAlpha(elevatedPrimary, 0.96),
    elevatedPrimaryOpaque: rgbString(elevatedPrimary),
    elevatedSecondary: colorAlpha(elevatedSecondary, 0.96),
    elevatedSecondaryOpaque: rgbString(elevatedSecondary),
    iconAccent: colorHex(accent),
    iconPrimary: colorHex(ink),
    iconSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    iconTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    simpleScrim: colorAlpha({ red: 0, green: 0, blue: 0 }, 0.08 + contrast * 0.04),
    textAccent: colorHex(accent),
    textButtonPrimary: colorHex(surface),
    textButtonSecondary: colorHex(ink),
    textButtonTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    textForeground: colorHex(ink),
    textForegroundSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    textForegroundTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
  };
}

function octopusDarkColors(surface: RgbColor, ink: RgbColor, accent: RgbColor, contrast: number) {
  const black = { red: 0, green: 0, blue: 0 };
  const white = { red: 255, green: 255, blue: 255 };
  const control = mixRgb(surface, ink, 0.06 + contrast * 0.05);
  const tunedAccent = mixRgb(accent, white, 0.3 + contrast * 0.15);
  const primaryButton = mixRgb(surface, black, 0.38 + contrast * 0.12);
  const elevatedPrimary = mixRgb(surface, ink, 0.08 + contrast * 0.08);
  return {
    accentBackground: colorHex(mixRgb(black, accent, 0.2 + contrast * 0.08)),
    accentBackgroundActive: colorHex(mixRgb(black, accent, 0.22 + contrast * 0.12)),
    accentBackgroundHover: colorHex(mixRgb(black, accent, 0.21 + contrast * 0.1)),
    border: colorAlpha(ink, 0.06 + contrast * 0.04),
    borderFocus: colorAlpha(tunedAccent, 0.7 + contrast * 0.1),
    borderHeavy: colorAlpha(ink, 0.12 + contrast * 0.06),
    borderLight: colorAlpha(ink, 0.03 + contrast * 0.02),
    buttonPrimaryBackground: rgbString(primaryButton),
    buttonPrimaryBackgroundActive: colorAlpha(ink, 0.07 + contrast * 0.05),
    buttonPrimaryBackgroundHover: colorAlpha(ink, 0.04 + contrast * 0.03),
    buttonPrimaryBackgroundInactive: colorAlpha(ink, 0.02 + contrast * 0.02),
    buttonSecondaryBackground: colorAlpha(ink, 0.04 + contrast * 0.02),
    buttonSecondaryBackgroundActive: colorAlpha(ink, 0.09 + contrast * 0.05),
    buttonSecondaryBackgroundHover: colorAlpha(ink, 0.06 + contrast * 0.03),
    buttonSecondaryBackgroundInactive: colorAlpha(ink, 0.02 + contrast * 0.03),
    buttonTertiaryBackground: colorAlpha(ink, 0.02 + contrast * 0.015),
    buttonTertiaryBackgroundActive: colorAlpha(ink, 0.07 + contrast * 0.05),
    buttonTertiaryBackgroundHover: colorAlpha(ink, 0.05 + contrast * 0.03),
    controlBackground: colorAlpha(control, 0.96),
    controlBackgroundOpaque: rgbString(control),
    elevatedPrimary: colorAlpha(elevatedPrimary, 0.96),
    elevatedPrimaryOpaque: rgbString(elevatedPrimary),
    elevatedSecondary: colorAlpha(ink, 0.02 + contrast * 0.02),
    elevatedSecondaryOpaque: colorHex(mixRgb(surface, ink, 0.04 + contrast * 0.05)),
    iconAccent: rgbString(tunedAccent),
    iconPrimary: colorAlpha(ink, 0.82 + contrast * 0.14),
    iconSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    iconTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    simpleScrim: colorAlpha(ink, 0.08 + contrast * 0.04),
    textAccent: rgbString(tunedAccent),
    textButtonPrimary: rgbString(primaryButton),
    textButtonSecondary: colorHex(mixRgb(ink, surface, 0.7 + contrast * 0.1)),
    textButtonTertiary: colorAlpha(ink, 0.45 + contrast * 0.1),
    textForeground: colorHex(ink),
    textForegroundSecondary: colorAlpha(ink, 0.65 + contrast * 0.1),
    textForegroundTertiary: colorAlpha(ink, 0.42 + contrast * 0.13),
  };
}

function octopusContrast(value: number, theme: "light" | "dark"): number {
  const baseline = theme === "dark" ? 60 : 45;
  const normalized = value / 100 + ((value - baseline) / 60) * 0.7;
  if (value <= baseline) return normalized;
  const base = baseline / 100;
  return base + (normalized - base) * 2;
}

function rgbColor(value: string): RgbColor {
  const [red, green, blue] = hexRgb(value);
  return { red, green, blue };
}

function mixRgb(from: RgbColor, to: RgbColor, ratio: number): RgbColor {
  const clamped = Math.min(1, Math.max(0, ratio));
  return {
    red: Math.round(from.red + (to.red - from.red) * clamped),
    green: Math.round(from.green + (to.green - from.green) * clamped),
    blue: Math.round(from.blue + (to.blue - from.blue) * clamped),
  };
}

function colorHex(value: RgbColor): string {
  return `#${[value.red, value.green, value.blue].map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

function rgbString(value: RgbColor): string {
  return `rgb(${value.red}, ${value.green}, ${value.blue})`;
}

function colorAlpha(value: RgbColor, alpha: number): string {
  const clamped = Math.min(1, Math.max(0, alpha));
  const formatted = clamped.toFixed(3).replace(/0+$/, "").replace(/\.$/, "");
  return `rgba(${value.red}, ${value.green}, ${value.blue}, ${formatted})`;
}

function textOnAccent(accent: RgbColor): string {
  const white = { red: 255, green: 255, blue: 255 };
  const black = { red: 0, green: 0, blue: 0 };
  if (accent.blue > accent.red && accent.blue > accent.green) {
    const chroma = accent.blue - Math.min(accent.red, accent.green);
    const hue = ((accent.red - accent.green) / chroma + 4) * 60;
    const saturation = chroma / accent.blue;
    if ((saturation >= 0.8 && hue >= 205 && hue <= 212)
      || (saturation >= 0.6 && hue >= 218 && hue <= 233 && relativeLuminance(accent) <= 0.21)) {
      return rgbString(white);
    }
  }
  return rgbString(relativeLuminance(accent) > 0.179 ? black : white);
}

function relativeLuminance(color: RgbColor): number {
  return linearChannel(color.red) * 0.2126
    + linearChannel(color.green) * 0.7152
    + linearChannel(color.blue) * 0.0722;
}

function linearChannel(channel: number): number {
  const normalized = channel / 255;
  return normalized <= 0.04045
    ? normalized / 12.92
    : ((normalized + 0.055) / 1.055) ** 2.4;
}

export function getAppearance(): AppearanceSettings {
  return current;
}

export function shouldReduceMotion(settings: AppearanceSettings = current): boolean {
  return settings.motionMode === "reduce"
    || (settings.motionMode === "system" && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
}

export function setAppearance(next: Partial<AppearanceSettings>): AppearanceSettings {
  current = normalize({
    ...current,
    ...next,
    ...(Object.hasOwn(next, "lightAccent") && !Object.hasOwn(next, "lightAccentSource") ? { lightAccentSource: "custom" } : {}),
    ...(Object.hasOwn(next, "darkAccent") && !Object.hasOwn(next, "darkAccentSource") ? { darkAccentSource: "custom" } : {}),
  });
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(current));
  } catch {
    void 0;
  }
  applyAppearance(current);
  for (const listener of [...listeners]) listener();
  return current;
}

export function resetAppearance(): AppearanceSettings {
  return setAppearance({ ...defaultAppearance });
}

export function useAppearance(): AppearanceSettings {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getAppearance,
    () => defaultAppearance,
  );
}

export function useShouldReduceMotion(): boolean {
  const settings = useAppearance();
  const systemReduced = useSyncExternalStore(
    subscribeToReducedMotion,
    systemPrefersReducedMotion,
    () => false,
  );
  return settings.motionMode === "reduce"
    || (settings.motionMode === "system" && systemReduced);
}

function normalize(value: AppearanceSettings): AppearanceSettings {
  return {
    ...value,
    themeMode: choice(value.themeMode, ["system", "light", "dark"], defaultAppearance.themeMode),
    lightAccent: color(value.lightAccent, defaultAppearance.lightAccent),
    lightAccentSource: choice(value.lightAccentSource, ["codex", "custom"], "custom"),
    lightBackground: color(value.lightBackground, defaultAppearance.lightBackground),
    lightForeground: color(value.lightForeground, defaultAppearance.lightForeground),
    lightDiffAdded: color(value.lightDiffAdded, defaultAppearance.lightDiffAdded),
    lightDiffRemoved: color(value.lightDiffRemoved, defaultAppearance.lightDiffRemoved),
    lightSkill: color(value.lightSkill, defaultAppearance.lightSkill),
    lightContrast: numberValue(value.lightContrast, defaultAppearance.lightContrast, 0, 100),
    lightUiFont: stringValue(value.lightUiFont),
    lightCodeFont: stringValue(value.lightCodeFont),
    darkAccent: color(value.darkAccent, defaultAppearance.darkAccent),
    darkAccentSource: choice(value.darkAccentSource, ["codex", "custom"], "custom"),
    darkBackground: color(value.darkBackground, defaultAppearance.darkBackground),
    darkForeground: color(value.darkForeground, defaultAppearance.darkForeground),
    darkDiffAdded: color(value.darkDiffAdded, defaultAppearance.darkDiffAdded),
    darkDiffRemoved: color(value.darkDiffRemoved, defaultAppearance.darkDiffRemoved),
    darkSkill: color(value.darkSkill, defaultAppearance.darkSkill),
    darkContrast: numberValue(value.darkContrast, defaultAppearance.darkContrast, 0, 100),
    darkUiFont: stringValue(value.darkUiFont),
    darkCodeFont: stringValue(value.darkCodeFont),
    uiFontSize: numberValue(value.uiFontSize, defaultAppearance.uiFontSize, 11, 20),
    codeFontSize: numberValue(value.codeFontSize, defaultAppearance.codeFontSize, 10, 20),
    motionMode: choice(value.motionMode, ["system", "reduce", "full"], defaultAppearance.motionMode),
    density: choice(value.density, ["compact", "default", "comfortable"], defaultAppearance.density),
    chatTextSize: choice(value.chatTextSize, ["small", "default", "large"], defaultAppearance.chatTextSize),
    cornerSoftness: choice(value.cornerSoftness, ["sharp", "default", "round"], defaultAppearance.cornerSoftness),
  };
}

function installObservers() {
  if (observersInstalled || typeof window === "undefined") return;
  observersInstalled = true;
  for (const query of ["(prefers-color-scheme: dark)", "(prefers-reduced-motion: reduce)"]) {
    window.matchMedia(query).addEventListener("change", () => applyAppearance(current));
  }
}

function subscribeToReducedMotion(listener: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return () => undefined;
  const media = window.matchMedia("(prefers-reduced-motion: reduce)");
  media.addEventListener("change", listener);
  return () => media.removeEventListener("change", listener);
}

function systemPrefersReducedMotion(): boolean {
  return typeof window !== "undefined"
    && typeof window.matchMedia === "function"
    && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function resolveTheme(mode: ThemeMode): "light" | "dark" {
  if (mode !== "system") return mode;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function choice<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return allowed.includes(value as T) ? value as T : fallback;
}

function booleanValue(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function numberValue(value: unknown, fallback: number, min: number, max: number): number {
  const number = Number(value);
  return Number.isFinite(number) ? Math.min(max, Math.max(min, Math.round(number))) : fallback;
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function color(value: unknown, fallback: string): string {
  const text = stringValue(value).toUpperCase();
  return /^#[0-9A-F]{6}$/.test(text) ? text : fallback;
}

function hexRgb(value: string): [number, number, number] {
  const normalized = color(value, "#000000").slice(1);
  return [
    Number.parseInt(normalized.slice(0, 2), 16),
    Number.parseInt(normalized.slice(2, 4), 16),
    Number.parseInt(normalized.slice(4, 6), 16),
  ];
}

function mixHex(background: string, foreground: string, foregroundRatio: number): string {
  const from = hexRgb(background);
  const to = hexRgb(foreground);
  const ratio = Math.min(1, Math.max(0, foregroundRatio));
  const mixed = from.map((channel, index) => Math.round(channel + (to[index] - channel) * ratio));
  return `#${mixed.map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

function rgba([red, green, blue]: [number, number, number], alpha: number): string {
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`;
}

function setScaledTokens(root: HTMLElement, tokens: Record<string, number>, scale: number): void {
  for (const [name, value] of Object.entries(tokens)) {
    root.style.setProperty(name, scaledPx(value, scale));
  }
}

function scaledPx(value: number, scale: number): string {
  return `${Math.round(value * scale * 10) / 10}px`;
}
