import { useMemo, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useClientPreferences } from "../../clientPreferences";
import { OcButton, OcCard, OcEmptyState, OcSettingsRow, cx } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import type { OctopusClientAccessProfile } from "../../electron";
import { appToast, useAppErrorToast } from "../../toasts";
import { SettingsHeading } from "./SettingsHeading";
import { clientCommands, type ClientCommandId } from "../../clientActions";
import { defaultKeyboardShortcuts, formatKeyboardShortcut, keyboardShortcutFromEvent, keyboardShortcutOwner, keyboardShortcutReservedReason, useKeyboardShortcuts, useReplaceKeyboardShortcuts, useResetKeyboardShortcuts, useSetKeyboardShortcuts } from "../../keyboardShortcuts";
import { settingsTargetProps } from "./settingsCatalog";

export function ShortcutsSection({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const preferences = useClientPreferences();
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const setShortcuts = useSetKeyboardShortcuts(accessProfile);
  const resetShortcuts = useResetKeyboardShortcuts(accessProfile);
  const replaceShortcuts = useReplaceKeyboardShortcuts(accessProfile);
  const [query, setQuery] = useState("");
  const [searchByKeystroke, setSearchByKeystroke] = useState(false);
  const [capturingCommand, setCapturingCommand] = useState<ClientCommandId | null>(null);
  const [shortcutError, setShortcutError] = useState("");
  useAppErrorToast(shortcutError);
  const rows = useMemo(() => {
    const sendKeys = preferences.sendShortcut === "enter"
      ? ["↵"]
      : preferences.sendShortcut === "cmd-if-multiline" ? ["↵ (single line)", "⌘↵ (multiline)"] : ["⌘↵"];
    const newlineKeys = preferences.sendShortcut === "enter"
      ? ["⇧↵"]
      : preferences.sendShortcut === "cmd-if-multiline" ? ["⇧↵ (single line)", "↵ (multiline)"] : ["↵"];
    return [
      ...clientCommands.map((command) => ({
        id: `command:${command.id}`,
        action: command.label,
        command: command.id as ClientCommandId,
        keys: (shortcuts[command.id] ?? []).map(formatKeyboardShortcut),
        detail: command.detail,
      })),
      { id: "surface-index", action: "Jump to surface 1–9", command: null, keys: ["⌘1–9"], detail: "Open a fixed product surface" },
      { id: "send", action: "Send message", command: null, keys: sendKeys, detail: "Send the current composer message" },
      { id: "queue", action: "Queue message while replying", command: null,
        keys: [formatKeyboardShortcut(preferences.sendShortcut === "enter" ? "Mod+Enter" : "Mod+Shift+Enter")],
        detail: "Queue this message after the current reply; ordinary Send still steers immediately" },
      { id: "newline", action: "New line in message", command: null, keys: newlineKeys, detail: "Insert a new line in the composer" },
      { id: "escape", action: "Close settings or dialog", command: null, keys: ["Esc"], detail: "Close the active dialog" },
    ];
  }, [preferences.sendShortcut, shortcuts]);
  const normalized = query.trim().toLowerCase();
  const displayRows = rows.filter((row) => `${row.action} ${row.detail} ${row.keys.join(" ")} ${row.keys.length ? "" : "unassigned"}`.toLowerCase().includes(normalized));

  function captureShortcut(event: ReactKeyboardEvent<HTMLButtonElement>, command: ClientCommandId) {
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape") {
      setCapturingCommand(null);
      return;
    }
    if ((event.key === "Backspace" || event.key === "Delete") && !event.metaKey && !event.ctrlKey && !event.altKey) {
      const previous = shortcuts[command] ?? [];
      setShortcuts(command, []);
      setCapturingCommand(null);
      setShortcutError("");
      if (previous.length) {
        const label = clientCommands.find((candidate) => candidate.id === command)?.label ?? command;
        appToast.info(`${label} unassigned`, 8, {
          action: { label: "Undo", onSelect: () => setShortcuts(command, previous) },
          dedupeKey: `keyboard-shortcut-unassign:${accessProfile}:${command}`,
        });
      }
      return;
    }
    const shortcut = keyboardShortcutFromEvent(event);
    if (!shortcut) return;
    const reservedReason = keyboardShortcutReservedReason(shortcut);
    if (reservedReason) {
      setShortcutError(`${formatKeyboardShortcut(shortcut)} cannot be assigned. ${reservedReason}`);
      return;
    }
    const owner = keyboardShortcutOwner(shortcut, shortcuts, command);
    if (owner) {
      const ownerLabel = clientCommands.find((candidate) => candidate.id === owner)?.label ?? owner;
      setShortcutError(`${formatKeyboardShortcut(shortcut)} is already assigned to ${ownerLabel}.`);
      return;
    }
    setShortcuts(command, [shortcut]);
    setCapturingCommand(null);
    setShortcutError("");
  }

  return (
    <section {...settingsTargetProps("shortcuts.bindings")} className="settings-section settings-shortcuts-section">
      <div className="flex items-start justify-between gap-4">
        <SettingsHeading description="Profile-scoped bindings shared by the app shell and command menu." title="Keyboard shortcuts" />
        <OcButton onClick={() => {
          const previous = shortcuts;
          resetShortcuts();
          setCapturingCommand(null);
          setShortcutError("");
          appToast.info("Keyboard shortcuts restored to defaults", 8, {
            action: { label: "Undo", onSelect: () => replaceShortcuts(previous) },
            dedupeKey: `keyboard-shortcuts-reset:${accessProfile}`,
          });
        }} size="sm" variant="secondary">Restore defaults</OcButton>
      </div>
      <div className="sticky top-[calc(-1*var(--padding-panel))] z-30 bg-token-main-surface-primary pt-panel pb-2 after:pointer-events-none after:absolute after:top-full after:right-0 after:left-0 after:h-8 after:bg-linear-to-b after:from-token-main-surface-primary after:to-transparent after:content-['']">
        <div>
          <div className="no-drag flex h-token-button-composer items-center gap-2 rounded-lg border border-token-input-border bg-token-input-background/75 px-2.5 py-0 text-base leading-[18px] shadow-sm backdrop-blur-sm">
            <Glyph className="icon-sm text-token-text-secondary" name="search" size={16} />
            <label className="sr-only" htmlFor="shortcut-search">Search keyboard shortcuts</label>
            <input
              className="min-w-0 flex-1 bg-transparent text-base leading-[18px] text-token-input-foreground outline-none select-text placeholder:text-token-input-placeholder-foreground [&::placeholder]:select-none"
              id="shortcut-search"
              onChange={(event) => setQuery(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (!searchByKeystroke) return;
                event.preventDefault();
                const shortcut = capturedShortcut(event);
                if (shortcut) setQuery(shortcut);
              }}
              placeholder={searchByKeystroke ? "Press shortcut" : "Search shortcuts"}
              type="text"
              value={query}
            />
            <div className="flex shrink-0 items-center">
              <button
                aria-label="Search by keystrokes"
                aria-pressed={searchByKeystroke}
                className={cx(
                  "border-token-border no-drag cursor-interaction flex aspect-square h-token-button-composer items-center justify-center gap-1 whitespace-nowrap rounded-lg border border-transparent !px-0 py-0 text-base leading-[18px] text-token-text-tertiary select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 enabled:hover:bg-token-list-hover-background",
                  searchByKeystroke && "bg-token-foreground/5 text-token-foreground",
                )}
                onClick={() => setSearchByKeystroke((current) => !current)}
                type="button"
              >
                <Glyph className="icon-sm" name="keyboard" size={16} />
              </button>
            </div>
          </div>
        </div>
      </div>
      <div className="flex flex-col gap-10 pt-5">
        <OcCard className="settings-shortcuts-card">
          {displayRows.map((row) => (
            <OcSettingsRow
              className="max-sm:flex-col max-sm:items-stretch max-sm:gap-2"
              control={(
                <div aria-label={row.action} className="flex w-96 max-w-full flex-col max-sm:w-full" role="group">
                  <div className="group flex min-h-8 items-center" role="group">
                    <div className="flex min-w-0 flex-1 items-center gap-1">
                      <span className="flex min-h-8 items-center gap-1 text-sm text-token-text-secondary">
                        {row.command ? (
                          <button
                            className="cursor-interaction rounded-md px-1 py-0.5 outline-none hover:bg-token-list-hover-background focus-visible:ring-1 focus-visible:ring-token-focus-border"
                            data-shortcut-capture={capturingCommand === row.command || undefined}
                            onClick={() => {
                              setCapturingCommand(row.command);
                              setShortcutError("");
                            }}
                            onKeyDown={(event) => capturingCommand === row.command && captureShortcut(event, row.command)}
                            type="button"
                          >
                            <kbd className="inline-flex !rounded-md !border-0 !bg-current/10 !font-sans !text-xs !text-current !shadow-none !px-2 !py-1 !leading-none !text-sm">
                              {capturingCommand === row.command ? "Press shortcut" : row.keys.join("  ") || "Unassigned"}
                            </kbd>
                          </button>
                        ) : (
                          <kbd className="inline-flex !rounded-md !border-0 !bg-current/10 !font-sans !text-xs !text-current !shadow-none !px-2 !py-1 !leading-none !text-sm">
                            {row.keys.join("  ") || "Unassigned"}
                          </kbd>
                        )}
                      </span>
                      {row.command && !sameShortcutLists(shortcuts[row.command] ?? [], defaultKeyboardShortcuts[row.command] ?? []) ? (
                        <OcButton onClick={() => {
                          const previous = shortcuts[row.command!] ?? [];
                          setShortcuts(row.command!, defaultKeyboardShortcuts[row.command!] ?? []);
                          appToast.info(`${row.action} restored to default`, 8, {
                            action: { label: "Undo", onSelect: () => setShortcuts(row.command!, previous) },
                            dedupeKey: `keyboard-shortcut-reset:${accessProfile}:${row.command}`,
                          });
                        }} size="sm" variant="ghost">Reset</OcButton>
                      ) : null}
                    </div>
                  </div>
                </div>
              )}
              description={row.detail}
              key={row.id}
              label={<span className="block truncate">{row.action}</span>}
            />
          ))}
          {displayRows.length === 0 ? <OcEmptyState className="min-h-0 py-6" size="compact" title="No matching shortcuts" /> : null}
        </OcCard>

      </div>
    </section>
  );
}

function capturedShortcut(event: ReactKeyboardEvent<HTMLInputElement>): string {
  if (["Meta", "Control", "Alt", "Shift"].includes(event.key)) return "";
  const parts = [
    event.ctrlKey ? "⌃" : "",
    event.altKey ? "⌥" : "",
    event.shiftKey ? "⇧" : "",
    event.metaKey ? "⌘" : "",
  ];
  const aliases: Record<string, string> = { Enter: "↵", Escape: "Esc", ArrowLeft: "Left", ArrowRight: "Right", " ": "Space" };
  return `${parts.join("")}${aliases[event.key] ?? (event.key.length === 1 ? event.key.toUpperCase() : event.key)}`;
}

function sameShortcutLists(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((shortcut, index) => shortcut === right[index]);
}
