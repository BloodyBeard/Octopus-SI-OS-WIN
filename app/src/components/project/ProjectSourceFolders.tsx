import { useState } from "react";
import { OcButton, OcIconButton } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";

export function ProjectSourceFolders({ folders, onChange, disabled, onError }: {
  folders: string[];
  onChange: (folders: string[]) => void;
  disabled: boolean;
  onError: (message: string) => void;
}) {
  const [choosing, setChoosing] = useState(false);
  const add = async () => {
    setChoosing(true);
    try {
      if (!window.octopusTransport?.selectClientDirectory) throw new Error("Folder selection is unavailable.");
      const result = await window.octopusTransport.selectClientDirectory({ mode: "directory", allowMultiple: true, title: "Choose source folders" });
      if (!result.canceled) onChange([...new Set([...folders, ...result.paths])]);
    } catch (failure) {
      onError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setChoosing(false);
    }
  };
  return <div className="flex w-full flex-col gap-2 pt-3">
    <span className="text-sm font-medium text-token-foreground">Source folders</span>
    <div className="max-h-72 divide-y divide-token-border overflow-y-auto rounded-xl border border-token-border bg-token-main-surface-primary">
      {folders.map((folder, index) => <div className="group flex h-12 min-w-0 items-center gap-2 px-3" key={folder}>
        <Glyph name="folder" size={16} className="shrink-0 text-token-description-foreground" />
        <span className="min-w-0 flex-1 truncate text-sm" title={folder}>{folder.split(/[\\/]/).filter(Boolean).at(-1) || folder}</span>
        {folders.length > 1 && (index === 0
          ? <span className="rounded-lg border border-token-border px-2.5 py-1 text-sm text-token-description-foreground" title="Tasks start in the primary folder.">Primary</span>
          : <OcButton disabled={disabled} onClick={() => onChange([folder, ...folders.filter((item) => item !== folder)])} size="sm" variant="ghost">Make primary</OcButton>)}
        <OcIconButton glyph="close" disabled={disabled} label={`Remove ${folder}`} onClick={() => onChange(folders.filter((item) => item !== folder))} />
      </div>)}
      <button type="button" disabled={disabled || choosing} onClick={() => void add()}
        className={`flex w-full items-center gap-2 px-3 text-sm text-token-description-foreground hover:bg-token-foreground/5 disabled:opacity-50 ${folders.length ? "h-12" : "h-24 flex-col justify-center text-center"}`}>
        <Glyph name="plus" size={16} />
        {folders.length ? "Add folder" : "Add folders Octopus can read and edit"}
      </button>
    </div>
  </div>;
}
