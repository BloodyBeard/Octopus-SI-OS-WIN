import { useMemo } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import { Glyph } from "../IconGlyphs";
import { OctopusTerminal } from "./OctopusTerminal";
import { createShellSession } from "./shellSession";
import "./terminal.css";

export function TerminalTab({ accessProfile, active, cwd, onNewTerminalTab, onTitleChange, terminalId }: {
  accessProfile: OctopusClientAccessProfile;
  active: boolean;
  cwd?: string;
  onNewTerminalTab: () => void;
  onTitleChange: (title: string) => void;
  terminalId: string;
}) {
  const session = useMemo(() => createShellSession(() => ({ terminalId, ...(cwd ? { cwd } : {}) })), [cwd, terminalId]);

  if (accessProfile !== "live") {
    return (
      <div className="terminal-unavailable" role="status">
        <Glyph name="terminal" size={20} />
        <strong>Terminal unavailable</strong>
        <span>A host shell is disabled in the isolated Test profile.</span>
      </div>
    );
  }

  return (
    <div className="h-full min-h-0 w-full overflow-hidden bg-[var(--vscode-terminal-background)]">
      <OctopusTerminal
        autoFocus={active}
        id={`terminal-panel-${terminalId}`}
        onNewTerminalTab={onNewTerminalTab}
        onTitleChange={onTitleChange}
        session={session}
      />
    </div>
  );
}
