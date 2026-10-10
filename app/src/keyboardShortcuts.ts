import { useCallback, useSyncExternalStore, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { clientCommands, type ClientCommandId } from "./clientActions";
import type { OctopusClientAccessProfile } from "./electron";

export type KeyboardShortcutMap = Readonly<Partial<Record<ClientCommandId, readonly string[]>>>;

const STORAGE_KEY = "octopus.keyboard-shortcuts.v1";

export const defaultKeyboardShortcuts: KeyboardShortcutMap = Object.freeze({
  new_chat: Object.freeze(["Mod+KeyN"]),
  open_voice: Object.freeze(["Mod+Shift+KeyV"]),
  open_settings: Object.freeze(["Mod+Comma"]),
  open_command_menu: Object.freeze(["Mod+KeyK", "Mod+Shift+KeyP"]),
  focus_search: Object.freeze(["Mod+KeyF"]),
  navigate_back: Object.freeze(["Mod+BracketLeft"]),
  navigate_forward: Object.freeze(["Mod+BracketRight"]),
  previous_surface: Object.freeze(["Mod+Alt+ArrowLeft"]),
  next_surface: Object.freeze(["Mod+Alt+ArrowRight"]),
  toggle_sidebar: Object.freeze(["Mod+Shift+KeyB"]),
  toggle_split: Object.freeze(["Mod+Alt+KeyB"]),
  toggle_task_summary: Object.freeze(["Mod+Shift+KeyI"]),
  open_terminal: Object.freeze(["Control+Backquote"]),
  open_browser: Object.freeze(["Mod+KeyT"]),
  open_changes: Object.freeze(["Control+Shift+KeyG"]),
  open_side_task: Object.freeze(["Mod+Alt+KeyS"]),
  start_dictation: Object.freeze(["Control+Shift+KeyD"]),
  previous_operator_message: Object.freeze(["Control+Shift+ArrowUp"]),
  next_operator_message: Object.freeze(["Control+Shift+ArrowDown"]),
});

type KeyboardShortcutStore = {
  current: KeyboardShortcutMap;
  profile: OctopusClientAccessProfile;
  subscribers: Set<() => void>;
};

const stores = new Map<OctopusClientAccessProfile, KeyboardShortcutStore>();
const commandIds = new Set(clientCommands.map((command) => command.id));

export function useKeyboardShortcuts(profile: OctopusClientAccessProfile): KeyboardShortcutMap {
  const store = keyboardShortcutStore(profile);
  return useSyncExternalStore(
    (subscriber) => subscribeStore(store, subscriber),
    () => store.current,
    () => defaultKeyboardShortcuts,
  );
}

export function useSetKeyboardShortcuts(profile: OctopusClientAccessProfile) {
  const store = keyboardShortcutStore(profile);
  return useCallback((command: ClientCommandId, shortcuts: readonly string[]) => {
    const normalized = normalizeShortcutList(shortcuts);
    const next = { ...store.current, [command]: normalized };
    writeStore(store, next);
  }, [store]);
}

export function useResetKeyboardShortcuts(profile: OctopusClientAccessProfile) {
  const store = keyboardShortcutStore(profile);
  return useCallback(() => writeStore(store, defaultKeyboardShortcuts), [store]);
}

export function useReplaceKeyboardShortcuts(profile: OctopusClientAccessProfile) {
  const store = keyboardShortcutStore(profile);
  return useCallback((shortcuts: KeyboardShortcutMap) => writeStore(store, shortcuts), [store]);
}

export function keyboardCommandForEvent(event: KeyboardEvent, shortcuts: KeyboardShortcutMap): ClientCommandId | null {
  if (event.defaultPrevented || event.isComposing || event.repeat) return null;
  for (const command of clientCommands) {
    if ((shortcuts[command.id] ?? []).some((shortcut) => keyboardEventMatchesShortcut(event, shortcut))) return command.id;
  }
  return null;
}

export function keyboardShortcutFromEvent(event: KeyboardEvent | ReactKeyboardEvent): string | null {
  if (["Meta", "Control", "Alt", "Shift"].includes(event.key)) return null;
  const code = event.code || keyCodeFallback(event.key);
  if (!code) return null;
  const modifiers: string[] = [];
  if (isPrimaryModifier(event)) modifiers.push("Mod");
  if (event.ctrlKey && !(isMac() ? false : isPrimaryModifier(event))) modifiers.push("Control");
  if (event.metaKey && !(isMac() ? isPrimaryModifier(event) : false)) modifiers.push("Meta");
  if (event.altKey) modifiers.push("Alt");
  if (event.shiftKey) modifiers.push("Shift");
  if (modifiers.length === 0 && code !== "Escape") return null;
  return normalizeKeyboardShortcut([...modifiers, code].join("+"));
}

export function formatKeyboardShortcut(shortcut: string): string {
  const parts = shortcut.split("+");
  const code = parts.pop() ?? "";
  const modifiers = parts.map((part) => {
    if (part === "Mod") return isMac() ? "⌘" : "Ctrl";
    if (part === "Control") return isMac() ? "⌃" : "Ctrl";
    if (part === "Meta") return isMac() ? "⌘" : "Meta";
    if (part === "Alt") return isMac() ? "⌥" : "Alt";
    if (part === "Shift") return "⇧";
    return part;
  });
  const key = codeLabel(code);
  return isMac() ? `${modifiers.join("")}${key}` : [...modifiers, key].join("+");
}

export function formatKeyboardShortcuts(shortcuts: readonly string[]): string {
  return shortcuts.map(formatKeyboardShortcut).join("  ");
}

export function keyboardShortcutOwner(shortcut: string, shortcuts: KeyboardShortcutMap, except?: ClientCommandId): ClientCommandId | null {
  const normalized = normalizeKeyboardShortcut(shortcut);
  for (const command of clientCommands) {
    if (command.id === except) continue;
    if ((shortcuts[command.id] ?? []).some((candidate) => normalizeKeyboardShortcut(candidate) === normalized)) return command.id;
  }
  return null;
}

export function keyboardShortcutReservedReason(shortcut: string): string {
  const normalized = normalizeKeyboardShortcut(shortcut);
  if (/^Mod\+Digit[1-9]$/.test(normalized)) return "Reserved for direct surface navigation.";
  if (["Mod+KeyB", "Mod+KeyI"].includes(normalized)) {
    return "Reserved for text editing while a composer is focused.";
  }
  if (/(?:^|\+)(?:Enter|Escape)$/.test(normalized)) return "Reserved for composer and dialog behavior.";
  return "";
}

export function keyboardEventHasEditableOwner(event: KeyboardEvent): boolean {
  const target = event.composedPath()[0] ?? event.target;
  return keyboardTargetHasEditableOwner(target);
}

export function keyboardTargetHasEditableOwner(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return Boolean(target.closest("input, textarea, select, [contenteditable]:not([contenteditable='false']), [role='textbox']"));
}

export function keyboardTargetHasRichEditingOwner(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return Boolean(target.closest("textarea, [contenteditable]:not([contenteditable='false']), [role='textbox']"));
}

export function clientDialogOwnsCommands(): boolean {
  return [...document.querySelectorAll<HTMLElement>("[role='dialog'], [role='menu']")].some((surface) => (
    !surface.closest("[inert], [hidden], [aria-hidden='true'], [data-state='closed']")
  ));
}

export function keyboardCommandConflictsWithEditing(command: ClientCommandId, shortcuts: KeyboardShortcutMap): boolean {
  return (shortcuts[command] ?? []).some((shortcut) => [
    "Mod+KeyB",
    "Mod+KeyI",
    "Mod+BracketLeft",
    "Mod+BracketRight",
  ].includes(normalizeKeyboardShortcut(shortcut)));
}

function keyboardEventMatchesShortcut(event: KeyboardEvent, shortcut: string): boolean {
  const parts = normalizeKeyboardShortcut(shortcut).split("+");
  const code = parts.pop();
  const requiresMod = parts.includes("Mod");
  const requiresControl = parts.includes("Control");
  const requiresMeta = parts.includes("Meta");
  const requiresAlt = parts.includes("Alt");
  const requiresShift = parts.includes("Shift");
  const expectedMeta = requiresMeta || (requiresMod && isMac());
  const expectedControl = requiresControl || (requiresMod && !isMac());
  return Boolean(code)
    && event.code === code
    && event.metaKey === expectedMeta
    && event.ctrlKey === expectedControl
    && event.altKey === requiresAlt
    && event.shiftKey === requiresShift;
}

function keyboardShortcutStore(profile: OctopusClientAccessProfile): KeyboardShortcutStore {
  const existing = stores.get(profile);
  if (existing) return existing;
  const created = { current: readShortcuts(profile), profile, subscribers: new Set<() => void>() };
  stores.set(profile, created);
  return created;
}

function subscribeStore(store: KeyboardShortcutStore, subscriber: () => void): () => void {
  store.subscribers.add(subscriber);
  return () => store.subscribers.delete(subscriber);
}

function writeStore(store: KeyboardShortcutStore, shortcuts: KeyboardShortcutMap): void {
  store.current = normalizeShortcutMap(shortcuts);
  window.localStorage.setItem(storageKey(store.profile), JSON.stringify(store.current));
  for (const subscriber of [...store.subscribers]) subscriber();
}

function readShortcuts(profile: OctopusClientAccessProfile): KeyboardShortcutMap {
  try {
    const raw = window.localStorage.getItem(storageKey(profile));
    if (!raw) return defaultKeyboardShortcuts;
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const result: Partial<Record<ClientCommandId, readonly string[]>> = {};
    for (const [command, value] of Object.entries(parsed)) {
      if (!commandIds.has(command as ClientCommandId) || !Array.isArray(value)) continue;
      result[command as ClientCommandId] = Object.freeze(normalizeShortcutList(value));
    }
    for (const command of clientCommands) {
      if (!(command.id in result)) result[command.id] = defaultKeyboardShortcuts[command.id] ?? [];
    }
    return normalizeShortcutMap(result);
  } catch {
    return defaultKeyboardShortcuts;
  }
}

function storageKey(profile: OctopusClientAccessProfile): string {
  return profile === "live" ? STORAGE_KEY : `${STORAGE_KEY}.test`;
}

function normalizeShortcutList(value: readonly unknown[]): string[] {
  return [...new Set(value.flatMap((shortcut) => {
    if (typeof shortcut !== "string") return [];
    const normalized = normalizeKeyboardShortcut(shortcut);
    return normalized ? [normalized] : [];
  }))].slice(0, 3);
}

function normalizeShortcutMap(shortcuts: KeyboardShortcutMap): KeyboardShortcutMap {
  const claimed = new Set<string>();
  return Object.freeze(Object.fromEntries(clientCommands.map((command) => {
    const values = normalizeShortcutList(shortcuts[command.id] ?? []).filter((shortcut) => {
      if (claimed.has(shortcut)) return false;
      claimed.add(shortcut);
      return true;
    });
    return [command.id, Object.freeze(values)];
  }))) as KeyboardShortcutMap;
}

function normalizeKeyboardShortcut(value: string): string {
  const source = value.split("+").map((part) => part.trim()).filter(Boolean);
  if (source.length === 0) return "";
  const code = source.at(-1) ?? "";
  if (!/^(Key[A-Z]|Digit[0-9]|F(?:[1-9]|1[0-2])|Comma|Period|Slash|Semicolon|Quote|BracketLeft|BracketRight|Backslash|Minus|Equal|Backquote|Enter|Escape|Space|Arrow(?:Up|Down|Left|Right)|Tab|Backspace|Delete)$/.test(code)) return "";
  const order = ["Mod", "Control", "Meta", "Alt", "Shift"];
  const modifiers = order.filter((modifier) => source.slice(0, -1).includes(modifier));
  if (modifiers.length === 0 && code !== "Escape") return "";
  return [...modifiers, code].join("+");
}

function isPrimaryModifier(event: Pick<KeyboardEvent, "metaKey" | "ctrlKey"> | Pick<ReactKeyboardEvent, "metaKey" | "ctrlKey">): boolean {
  return isMac() ? event.metaKey : event.ctrlKey;
}

function isMac(): boolean {
  return typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
}

function keyCodeFallback(key: string): string {
  if (key.length === 1 && /[a-z]/i.test(key)) return `Key${key.toUpperCase()}`;
  if (key.length === 1 && /[0-9]/.test(key)) return `Digit${key}`;
  const aliases: Record<string, string> = { ",": "Comma", "`": "Backquote", "[": "BracketLeft", "]": "BracketRight", " ": "Space" };
  return aliases[key] ?? key;
}

function codeLabel(code: string): string {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  const labels: Record<string, string> = {
    Backquote: "`",
    BracketLeft: "[",
    BracketRight: "]",
    Comma: ",",
    Enter: "↵",
    Escape: "Esc",
    Space: "Space",
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
    Backspace: "⌫",
    Delete: "⌦",
  };
  return labels[code] ?? code;
}
