import { useEffect, useMemo, useRef } from "react";
import type { ITheme, Terminal as XtermTerminal } from "@xterm/xterm";
import { useAppearance } from "../../appearance";
import "./terminal.css";

export type OctopusTerminalSessionHandlers = {
  onInitLog(log: string): void;
  onData(data: string): void;
};

export type OctopusTerminalSession = {
  open(dimensions: { cols: number; rows: number }): Promise<void>;
  write(data: string): void;
  resize(dimensions: { cols: number; rows: number }): void;
  subscribe(handlers: OctopusTerminalSessionHandlers): () => void;
};

const DEFAULT_TERMINAL_FONT_FAMILY = 'ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace';
const DEFAULT_TERMINAL_FONT_SIZE = 12;
const MAC_HOME_SEQUENCE = "\x01";
const MAC_END_SEQUENCE = "\x05";
const MAC_KILL_FORWARD_SEQUENCE = "\v";
const MAC_KILL_BACKWARD_SEQUENCE = "\x15";

export function OctopusTerminal({ autoFocus = true, id, onNewTerminalTab, onTitleChange, readOnly = false, session }: {
  autoFocus?: boolean;
  id?: string;
  onNewTerminalTab?: () => void;
  onTitleChange?: (title: string) => void;
  readOnly?: boolean;
  session: OctopusTerminalSession;
}) {
  const appearance = useAppearance();
  const panelRef = useRef<HTMLDivElement>(null);
  const hostRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<XtermTerminal | null>(null);
  const autoFocusRef = useRef(autoFocus);
  const onNewTerminalTabRef = useRef(onNewTerminalTab);
  const onTitleChangeRef = useRef(onTitleChange);
  const readOnlyRef = useRef(readOnly);
  autoFocusRef.current = autoFocus;
  onNewTerminalTabRef.current = onNewTerminalTab;
  onTitleChangeRef.current = onTitleChange;
  readOnlyRef.current = readOnly;
  const panelStyle = useMemo(
    () => ({
      backgroundColor: "var(--vscode-terminal-background)",
      color: "var(--vscode-terminal-foreground)",
    }),
    [],
  );

  useEffect(() => {
    const panel = panelRef.current;
    const terminal = terminalRef.current;
    if (!panel || !terminal) return;
    terminal.options.theme = readTerminalTheme(panel);
    if (terminal.rows > 0) terminal.refresh(0, terminal.rows - 1);
  }, [appearance]);

  useEffect(() => {
    if (autoFocus) terminalRef.current?.focus();
  }, [autoFocus]);

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    terminal.options.cursorBlink = !readOnly;
    terminal.options.cursorStyle = readOnly ? "block" : "bar";
    terminal.options.disableStdin = readOnly;
  }, [readOnly]);

  useEffect(() => {
    const panel = panelRef.current;
    const host = hostRef.current;
    if (!panel || !host) return;

    let disposed = false;
    let scrollFrame: number | null = null;
    let fitFrame: number | null = null;
    let disposeTerminal: (() => void) | undefined;

    void Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
      import("@xterm/addon-clipboard"),
      import("@xterm/addon-web-links"),
    ])
      .then(async ([{ Terminal }, { FitAddon }, { ClipboardAddon }, { WebLinksAddon }]) => {
        if (disposed) return;
        const style = getComputedStyle(panel);
        const configuredFont = style.getPropertyValue("--vscode-editor-font-family").trim()
          || style.getPropertyValue("--font-mono").trim()
          || DEFAULT_TERMINAL_FONT_FAMILY;
        const fontFamily = normalizeFontFamily(configuredFont);
        const fontSize = parseFontSize(style.getPropertyValue("--vscode-editor-font-size")) ?? DEFAULT_TERMINAL_FONT_SIZE;
        await preloadFont(fontFamily.split(",")[0]?.trim() ?? "", fontSize);
        if (disposed) return;

        const openLink = (_event: Event | undefined, uri: string) => {
          window.open(uri, "_blank");
        };
        const terminal = new Terminal({
          allowTransparency: true,
          cursorStyle: readOnlyRef.current ? "block" : "bar",
          fontSize,
          allowProposedApi: true,
          cursorBlink: !readOnlyRef.current,
          disableStdin: readOnlyRef.current,
          fontFamily,
          letterSpacing: 0,
          lineHeight: 1.2,
          linkHandler: { activate: (event, uri) => openLink(event, uri) },
          theme: readTerminalTheme(panel),
        });
        terminalRef.current = terminal;
        terminal.loadAddon(new ClipboardAddon());
        const fit = new FitAddon();
        terminal.loadAddon(fit);
        terminal.loadAddon(new WebLinksAddon((event, uri) => openLink(event, uri)));
        const isMac = navigator.platform.toUpperCase().includes("MAC");
        terminal.attachCustomKeyEventHandler((event) => handleTerminalKeyEvent({
          clipboard: !isMac && typeof navigator !== "undefined" && navigator.clipboard != null ? navigator.clipboard : undefined,
          event,
          onNewTerminalTab: onNewTerminalTabRef.current,
          pasteOnCtrlV: navigator.platform.toUpperCase().includes("WIN"),
          sendText: (text) => {
            if (!readOnlyRef.current) session.write(text);
          },
          term: terminal,
        }));
        terminal.open(host);

        const scrollToBottomSoon = () => {
          scrollFrame ??= requestAnimationFrame(() => {
            scrollFrame = null;
            terminal.scrollToBottom();
          });
        };
        const fitSoon = () => {
          if (disposed || !host.isConnected || fitFrame !== null) return;
          fitFrame = requestAnimationFrame(() => {
            fitFrame = null;
            if (disposed || !host.isConnected || terminalRef.current !== terminal) return;
            const bounds = host.getBoundingClientRect();
            if (bounds.width <= 0 || bounds.height <= 0) return;
            try {
              fit.fit();
              session.resize({ cols: terminal.cols, rows: terminal.rows });
            } catch {
            }
          });
        };
        const observer = new ResizeObserver(fitSoon);
        observer.observe(host);

        const unsubscribe = session.subscribe({
          onInitLog: (log) => {
            const followTail = isViewportAtBottom(terminal);
            terminal.reset();
            terminal.write(log);
            if (followTail) scrollToBottomSoon();
          },
          onData: (data) => {
            const followTail = isViewportAtBottom(terminal);
            terminal.write(data);
            if (followTail) scrollToBottomSoon();
          },
        });
        const input = terminal.onData((data) => {
          if (!readOnlyRef.current) session.write(data);
        });
        const title = terminal.onTitleChange((value) => onTitleChangeRef.current?.(value));
        disposeTerminal = () => {
          if (scrollFrame != null) cancelAnimationFrame(scrollFrame);
          if (fitFrame != null) cancelAnimationFrame(fitFrame);
          observer.disconnect();
          unsubscribe();
          input.dispose();
          title.dispose();
          terminal.dispose();
          if (terminalRef.current === terminal) terminalRef.current = null;
        };

        try {
          const bounds = host.getBoundingClientRect();
          if (bounds.width > 0 && bounds.height > 0) fit.fit();
        } catch {
        }
        await session.open({ cols: terminal.cols, rows: terminal.rows });
        if (disposed) return;
        if (autoFocusRef.current) terminal.focus();
      })
      .catch((error) => {
        if (!disposed) host.textContent = error instanceof Error ? error.message : String(error);
      });

    return () => {
      disposed = true;
      disposeTerminal?.();
      host.replaceChildren();
    };
  }, [session]);

  return (
    <div
      className="relative flex h-full w-full flex-col app-theme"
      data-octopus-terminal
      data-octopus-xterm
      id={id}
      ref={panelRef}
      style={panelStyle}
    >
      <div className="flex-1 overflow-hidden pb-3 pl-4 tracking-normal">
        <div className="h-full w-full overflow-hidden" ref={hostRef} />
      </div>
    </div>
  );
}

function readTerminalTheme(element: HTMLElement): ITheme {
  const elementStyle = getComputedStyle(element);
  const rootStyle = getComputedStyle(document.documentElement);
  const probe = document.createElement("div");
  element.appendChild(probe);
  const resolve = (name: string): string | undefined => {
    const value = elementStyle.getPropertyValue(name).trim() || rootStyle.getPropertyValue(name).trim();
    if (!value) return undefined;
    probe.style.color = value;
    return getComputedStyle(probe).color || value || undefined;
  };
  const theme: ITheme = {
    background: resolve("--vscode-terminal-background"),
    foreground: resolve("--vscode-terminal-foreground"),
    cursor: resolve("--vscode-terminal-foreground"),
    selectionBackground: resolve("--vscode-terminal-selectionBackground"),
    selectionInactiveBackground: resolve("--vscode-terminal-inactiveSelectionBackground"),
    black: resolve("--vscode-terminal-ansiBlack"),
    red: resolve("--vscode-terminal-ansiRed"),
    green: resolve("--vscode-terminal-ansiGreen"),
    yellow: resolve("--vscode-terminal-ansiYellow"),
    blue: resolve("--vscode-terminal-ansiBlue"),
    magenta: resolve("--vscode-terminal-ansiMagenta"),
    cyan: resolve("--vscode-terminal-ansiCyan"),
    white: resolve("--vscode-terminal-ansiWhite"),
    brightBlack: resolve("--vscode-terminal-ansiBrightBlack"),
    brightRed: resolve("--vscode-terminal-ansiBrightRed"),
    brightGreen: resolve("--vscode-terminal-ansiBrightGreen"),
    brightYellow: resolve("--vscode-terminal-ansiBrightYellow"),
    brightBlue: resolve("--vscode-terminal-ansiBrightBlue"),
    brightMagenta: resolve("--vscode-terminal-ansiBrightMagenta"),
    brightCyan: resolve("--vscode-terminal-ansiBrightCyan"),
    brightWhite: resolve("--vscode-terminal-ansiBrightWhite"),
  };
  probe.remove();
  return theme;
}

function normalizeFontFamily(value: string): string {
  const trimmed = value.trim();
  if (trimmed.length === 0) return "monospace";
  return endsWithMonospace(trimmed) ? trimmed : `${trimmed}, monospace`;
}

function endsWithMonospace(value: string): boolean {
  let segmentStart = 0;
  let quote: string | null = null;
  let escaped = false;
  for (let index = 0; index < value.length; index++) {
    const character = value[index];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (character === "\\") {
      escaped = true;
      continue;
    }
    if (quote != null) {
      if (character === quote) quote = null;
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      continue;
    }
    if (character === ",") {
      if (value.slice(segmentStart, index).trim().toLowerCase() === "monospace") return true;
      segmentStart = index + 1;
    }
  }
  return value.slice(segmentStart).trim().toLowerCase() === "monospace";
}

function isViewportAtBottom(terminal: XtermTerminal): boolean {
  return terminal.buffer.active.viewportY >= terminal.buffer.active.baseY;
}

async function preloadFont(fontFamily: string, fontSize: number): Promise<void> {
  if (!fontFamily || !("fonts" in document)) return;
  try {
    if (!document.fonts.check(`${fontSize}px ${fontFamily}`)) {
      await document.fonts.load(`${fontSize}px ${fontFamily}`);
    }
  } catch {
  }
}

function parseFontSize(value: string): number | null {
  const parsed = Number.parseFloat(value.trim());
  return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
}

type TerminalKeyEventOptions = {
  clipboard?: Clipboard;
  event: KeyboardEvent;
  onNewTerminalTab?: () => void;
  pasteOnCtrlV?: boolean;
  sendText: (text: string) => void;
  term: XtermTerminal;
};

function handleTerminalKeyEvent({ clipboard, event, onNewTerminalTab, pasteOnCtrlV = false, sendText, term }: TerminalKeyEventOptions): boolean {
  if (event.type !== "keydown") return true;
  if (onNewTerminalTab != null && isCommandChord(event, ["t"])) {
    swallowKeyEvent(event);
    onNewTerminalTab();
    return false;
  }
  if (clipboard != null) {
    if (isCopyChord(event, term.hasSelection())) {
      swallowKeyEvent(event);
      const selection = term.getSelection();
      if (selection.length > 0) clipboard.writeText(selection).catch(() => undefined);
      return false;
    }
    if (isPasteChord(event, pasteOnCtrlV)) {
      swallowKeyEvent(event);
      clipboard
        .readText()
        .then((text) => {
          if (text.length > 0) term.paste(text);
        })
        .catch(() => undefined);
      return false;
    }
  }
  const sequence = macLineEditSequence(event);
  if (sequence == null) return true;
  swallowKeyEvent(event);
  sendText(sequence);
  return false;
}

function swallowKeyEvent(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();
}

function isCopyChord(event: KeyboardEvent, hasSelection: boolean): boolean {
  return (
    (hasSelection && isControlChord(event, "c"))
    || isControlShiftChord(event, "c")
    || isInsertChord(event, { ctrlKey: true, shiftKey: false })
  );
}

function isPasteChord(event: KeyboardEvent, pasteOnCtrlV: boolean): boolean {
  return (
    (pasteOnCtrlV && isControlChord(event, "v"))
    || isControlShiftChord(event, "v")
    || isInsertChord(event, { ctrlKey: false, shiftKey: true })
  );
}

function macLineEditSequence(event: KeyboardEvent): string | null {
  return isCommandChord(event, ["ArrowLeft", "ArrowUp"])
    ? MAC_HOME_SEQUENCE
    : isCommandChord(event, ["ArrowRight", "ArrowDown"])
      ? MAC_END_SEQUENCE
      : isCommandChord(event, ["Backspace"])
        ? MAC_KILL_BACKWARD_SEQUENCE
        : isCommandChord(event, ["Delete"])
          ? MAC_KILL_FORWARD_SEQUENCE
          : null;
}

function isCommandChord(event: KeyboardEvent, keys: string[]): boolean {
  return event.metaKey && !event.altKey && !event.ctrlKey && !event.shiftKey && keys.includes(event.key);
}

function isControlShiftChord(event: KeyboardEvent, key: string): boolean {
  return event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey && event.key.toLowerCase() === key;
}

function isControlChord(event: KeyboardEvent, key: string): boolean {
  return event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey && event.key.toLowerCase() === key;
}

function isInsertChord(event: KeyboardEvent, modifiers: { ctrlKey: boolean; shiftKey: boolean }): boolean {
  return (
    event.ctrlKey === modifiers.ctrlKey
    && event.shiftKey === modifiers.shiftKey
    && !event.altKey
    && !event.metaKey
    && event.key.toLowerCase() === "insert"
  );
}
