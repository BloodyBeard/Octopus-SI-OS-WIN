import { useEffect, useMemo, useRef, useState } from "react";
import type { OctopusOsFileEntry, OctopusOsFileRoot } from "../../electron";
import { OcDropdown } from "../../design-system/OcDropdown";
import { OctopusFileTree } from "../OctopusSurface";
import { Glyph } from "../IconGlyphs";

export function FilesBreadcrumb({ root, path, label, onSelectFile }: {
  root: OctopusOsFileRoot | null;
  path?: string;
  label?: string;
  onSelectFile: (rootId: string, path: string) => void;
}) {
  const rootLabel = root?.path.split(/[\\/]/).filter(Boolean).at(-1);
  const parts = label ? [label] : path && root
    ? [...(rootLabel ? [rootLabel] : []), ...path.split("/").filter(Boolean)]
    : [root?.path.replaceAll("\\", "/").match(/^\/?([a-z]:)(?:\/|$)/i)?.[1] ?? "/"];
  const displayPath = parts.join("/");
  return <div className="os-files-breadcrumb" title={label || path || root?.path}>
    <ol className="os-files-breadcrumb-path" onCopy={(event) => {
      const text = breadcrumbSelection(event.currentTarget);
      if (text === null) return;
      event.preventDefault();
      event.clipboardData.setData("text/plain", text === displayPath ? displayPath.replaceAll("\\", "/") : text);
    }}>
      {parts.map((part, index) => {
        const current = index === parts.length - 1;
        const pathStart = rootLabel ? 1 : 0;
        const activePath = index < pathStart ? null : parts.slice(pathStart, index + 1).join("/");
        const directoryPath = index <= pathStart ? "" : parts.slice(pathStart, index).join("/");
        return <li key={`${root?.id}:${path}:${index}`}>
          {path && root && !label ? <BreadcrumbSegment
            activePath={activePath}
            current={current}
            directoryPath={directoryPath}
            label={part}
            onSelectFile={onSelectFile}
            root={root}
          /> : <span className={current ? "is-current" : undefined}>{part}</span>}
          {!current ? <Glyph aria-hidden="true" name="chevronDown" size={14} /> : null}
        </li>;
      })}
    </ol>
  </div>;
}

function BreadcrumbSegment({ activePath, current, directoryPath, label, onSelectFile, root }: {
  activePath: string | null;
  current: boolean;
  directoryPath: string;
  label: string;
  onSelectFile: (rootId: string, path: string) => void;
  root: OctopusOsFileRoot;
}) {
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLButtonElement>(null);
  return <>
    <button className={`os-files-breadcrumb-segment${current ? " is-current" : ""}`} onClick={() => setOpen((value) => !value)} ref={anchorRef} type="button">{label}</button>
    <OcDropdown anchorRef={anchorRef} aria-label={label} className="os-files-breadcrumb-menu" onOpenChange={setOpen} open={open} role="dialog" surface="opaque">
      {open ? <BreadcrumbDirectory
        activePath={activePath}
        directoryPath={directoryPath}
        expandActivePath={!current}
        onSelectFile={(path) => {
          setOpen(false);
          onSelectFile(root.id, path);
        }}
        rootId={root.id}
      /> : null}
    </OcDropdown>
  </>;
}

function BreadcrumbDirectory({ activePath, directoryPath, expandActivePath, onSelectFile, rootId }: {
  activePath: string | null;
  directoryPath: string;
  expandActivePath: boolean;
  onSelectFile: (path: string) => void;
  rootId: string;
}) {
  const prefix = directoryPath ? `${directoryPath}/` : "";
  const selectedPath = activePath?.slice(prefix.length);
  const [expanded, setExpanded] = useState<string[]>(() => expandActivePath && selectedPath ? [selectedPath] : []);
  const [directories, setDirectories] = useState<Record<string, OctopusOsFileEntry[]>>({});
  const [error, setError] = useState("");
  const requested = useRef(new Set<string>());
  const generationRef = useRef(0);
  useEffect(() => {
    generationRef.current += 1;
    return () => { generationRef.current += 1; requested.current.clear(); };
  }, []);
  useEffect(() => {
    const load = async (path: string) => {
      if (requested.current.has(path)) return;
      requested.current.add(path);
      const generation = generationRef.current;
      try {
        const transport = window.octopusTransport;
        if (!transport?.listOsFiles) throw new Error("OS Files directory transport is unavailable");
        const result = await transport.listOsFiles({ rootId, path });
        if (result.rootId !== rootId || result.path !== path) throw new Error("OS Files returned a directory for a different root or path");
        if (generation === generationRef.current) setDirectories((value) => ({ ...value, [path]: result.entries }));
      } catch (failure) {
        if (generation === generationRef.current && path === directoryPath) setError(failure instanceof Error ? failure.message : String(failure));
      }
    };
    for (const path of [directoryPath, ...expanded.map((path) => prefix + path)]) void load(path);
  }, [directoryPath, expanded, prefix, rootId]);
  const paths = useMemo(() => {
    const result = new Set<string>();
    for (const path of [directoryPath, ...expanded.map((path) => prefix + path)]) {
      for (const entry of directories[path] ?? []) {
        if (!entry.path.startsWith(prefix)) continue;
        const relative = entry.path.slice(prefix.length);
        if (entry.kind === "directory") result.add(`${relative}/`);
        else if (entry.kind === "file") result.add(relative);
      }
    }
    return [...result];
  }, [directories, directoryPath, expanded, prefix]);
  if (error) return <div className="os-files-breadcrumb-message is-error" role="alert" title={error}>Couldn't load folder contents</div>;
  if (!directories[directoryPath]) return <div className="os-files-breadcrumb-message" role="status">Loading…</div>;
  if (!paths.length) return <div className="os-files-breadcrumb-message">This folder is empty</div>;
  return <OctopusFileTree
    initialExpandedPaths={expanded}
    onExpandedPathsChange={(paths) => setExpanded((current) => current.length === paths.length && current.every((path, index) => path === paths[index]) ? current : paths)}
    onSelectPath={(path) => onSelectFile(prefix + path)}
    paths={paths}
    revealSelectedPath
    revealSelectedPathScrollOffset="center"
    selectedPath={selectedPath}
    surface="dropdown"
  />;
}

function breadcrumbSelection(element: HTMLOListElement): string | null {
  const selection = element.ownerDocument.getSelection();
  if (!selection || selection.rangeCount === 0) return null;
  const range = selection.getRangeAt(0);
  if (range.collapsed || !element.contains(range.commonAncestorContainer)) return null;
  const items = range.cloneContents().querySelectorAll("li");
  return (items.length === 0 ? range.toString() : Array.from(items, (item) => item.textContent ?? "").filter((text) => text.length > 0).join("/")) || null;
}
