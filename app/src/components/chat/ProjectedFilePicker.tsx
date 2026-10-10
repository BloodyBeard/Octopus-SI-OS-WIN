import { useEffect, useMemo, useRef, useState } from "react";
import type { OctopusWorkerEnvironmentFileEntry } from "../../electron";
import { Glyph } from "../IconGlyphs";

export type ProjectedFileSource = {
  workerEnvironmentId: string;
  surfaceId: string;
  label: string;
};

export function ProjectedFilePicker({ onAttach, onClose, source }: {
  onAttach: (file: File, path: string) => void;
  onClose: () => void;
  source: ProjectedFileSource;
}) {
  const [currentPath, setCurrentPath] = useState("");
  const [entries, setEntries] = useState<OctopusWorkerEnvironmentFileEntry[]>([]);
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [loading, setLoading] = useState(true);
  const [attachingPath, setAttachingPath] = useState("");
  const [error, setError] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const filteredEntries = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    return entries.filter((entry) => !needle || entry.name.toLocaleLowerCase().includes(needle));
  }, [entries, query]);

  useEffect(() => {
    setCurrentPath("");
    setQuery("");
    setSelectedIndex(0);
  }, [source.surfaceId, source.workerEnvironmentId]);

  useEffect(() => {
    let cancelled = false;
    const transport = window.octopusTransport;
    setLoading(true);
    setError("");
    if (!transport?.listWorkerEnvironmentFiles) {
      setEntries([]);
      setLoading(false);
      setError("Projected file attachment requires the Octopus desktop interface.");
      return () => { cancelled = true; };
    }
    void transport.listWorkerEnvironmentFiles({
      workerEnvironmentId: source.workerEnvironmentId,
      surfaceId: source.surfaceId,
      path: currentPath,
    }).then((directory) => {
      if (cancelled) return;
      assertProjectedFileResponse(directory, source, currentPath);
      setEntries(directory.entries);
      setSelectedIndex(0);
    }).catch((failure) => {
      if (cancelled) return;
      setEntries([]);
      setError(formatFailure(failure));
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [currentPath, source.surfaceId, source.workerEnvironmentId]);

  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  useEffect(() => {
    setSelectedIndex((current) => Math.min(current, Math.max(0, filteredEntries.length - 1)));
  }, [filteredEntries.length]);

  const choose = (entry: OctopusWorkerEnvironmentFileEntry) => {
    if (entry.kind === "directory") {
      setCurrentPath(entry.path);
      setQuery("");
      return;
    }
    if (entry.kind !== "file" || attachingPath) return;
    const transport = window.octopusTransport;
    if (!transport?.readWorkerEnvironmentFileAttachment) {
      setError("Projected file attachment requires the Octopus desktop interface.");
      return;
    }
    setAttachingPath(entry.path);
    setError("");
    void transport.readWorkerEnvironmentFileAttachment({
      workerEnvironmentId: source.workerEnvironmentId,
      surfaceId: source.surfaceId,
      path: entry.path,
    }).then((attachment) => {
      assertProjectedFileResponse(attachment, source, entry.path);
      const modifiedAt = Date.parse(attachment.modifiedAt);
      const file = new File([new Uint8Array(attachment.bytes)], attachment.path, {
        type: attachment.mimeType,
        lastModified: Number.isFinite(modifiedAt) ? modifiedAt : Date.now(),
      });
      setAttachingPath("");
      onAttach(file, attachment.path);
      onClose();
    }).catch((failure) => {
      setAttachingPath("");
      setError(formatFailure(failure));
    });
  };

  const parentPath = currentPath.includes("/") ? currentPath.slice(0, currentPath.lastIndexOf("/")) : "";
  return (
    <div className="projected-file-picker" role="dialog" aria-label="Attach projected file">
      <header className="projected-file-picker-header">
        <button aria-label="Back" disabled={!currentPath || Boolean(attachingPath)} onClick={() => {
          setCurrentPath(parentPath);
          setQuery("");
        }} type="button"><Glyph name="arrowLeft" size={14} /></button>
        <div><strong>Attach from Worker Environment</strong><span>{source.label}</span></div>
        <button aria-label="Close projected file picker" onClick={onClose} type="button"><Glyph name="close" size={14} /></button>
      </header>
      <div className="projected-file-picker-path" title={currentPath || "Root"}>
        <Glyph name="folder" size={13} />
        <span>{currentPath || "Root"}</span>
      </div>
      <label className="projected-file-picker-search">
        <Glyph name="search" size={14} />
        <span className="sr-only">Search current directory</span>
        <input
          onChange={(event) => {
            setQuery(event.currentTarget.value);
            setSelectedIndex(0);
          }}
          onKeyDown={(event) => {
            event.stopPropagation();
            if (event.key === "Escape") {
              event.preventDefault();
              onClose();
              return;
            }
            if ((event.key === "ArrowDown" || event.key === "ArrowUp") && filteredEntries.length) {
              event.preventDefault();
              const direction = event.key === "ArrowDown" ? 1 : -1;
              setSelectedIndex((current) => (current + direction + filteredEntries.length) % filteredEntries.length);
              return;
            }
            if (event.key === "Enter") {
              event.preventDefault();
              const entry = filteredEntries[selectedIndex];
              if (entry) choose(entry);
            }
          }}
          placeholder="Search this folder"
          ref={searchRef}
          type="text"
          value={query}
        />
      </label>
      <div className="projected-file-picker-list" role="listbox">
        {loading ? <div className="projected-file-picker-state"><span className="composer-spinner" />Reading projected files…</div> : null}
        {!loading && error ? <div className="projected-file-picker-state is-error"><Glyph name="warning" size={15} />{error}</div> : null}
        {!loading && !error && !filteredEntries.length ? <div className="projected-file-picker-state">No matching files in this directory.</div> : null}
        {!loading && !error ? filteredEntries.map((entry, index) => {
          const disabled = entry.kind !== "directory" && entry.kind !== "file";
          return (
            <button
              aria-disabled={disabled}
              aria-selected={index === selectedIndex}
              className={index === selectedIndex ? "is-selected" : ""}
              disabled={disabled || Boolean(attachingPath)}
              key={`${entry.kind}:${entry.path}`}
              onClick={() => choose(entry)}
              onMouseEnter={() => setSelectedIndex(index)}
              role="option"
              type="button"
            >
              <Glyph name={entry.kind === "directory" ? "folder" : "file"} size={15} />
              <span><strong>{entry.name}</strong><small>{entry.kind === "file" ? formatFileSize(entry.size) : entry.kind}</small></span>
              {attachingPath === entry.path ? <span className="composer-spinner" /> : entry.kind === "directory" ? <Glyph className="-rotate-90" name="chevronDown" size={12} /> : null}
            </button>
          );
        }) : null}
      </div>
      <footer>Selected bytes are attached through the ordinary operator message hand-in.</footer>
    </div>
  );
}

function formatFileSize(value: number | null): string {
  if (value == null) return "file";
  if (value < 1_000) return `${value} B`;
  if (value < 1_000_000) return `${Math.max(1, Math.round(value / 1_000))} KB`;
  return `${(value / 1_000_000).toFixed(value < 10_000_000 ? 1 : 0)} MB`;
}

function formatFailure(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function assertProjectedFileResponse(
  response: { workerEnvironmentId: string; surfaceId: string; path: string },
  source: ProjectedFileSource,
  expectedPath: string,
): void {
  if (
    response.workerEnvironmentId !== source.workerEnvironmentId
    || response.surfaceId !== source.surfaceId
    || response.path !== expectedPath
  ) {
    throw new Error("Projected file response no longer matches the selected Worker Environment root.");
  }
}
