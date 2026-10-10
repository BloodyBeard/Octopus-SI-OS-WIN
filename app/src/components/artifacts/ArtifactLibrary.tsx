import { useEffect, useLayoutEffect, useMemo, useState, type KeyboardEvent } from "react";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { useFixedVirtualRange } from "../useFixedVirtualRange";
import { artifactKindHint } from "./rendererRegistry";
import type { ArtifactLibraryRow, ArtifactRendererKind } from "./types";
import "./artifact-workbench.css";

type ArtifactFilter = "all" | "code" | "data" | "documents" | "images" | "media" | "opaque" | "text";
type ArtifactSort = "newest" | "name" | "type";
type ArtifactView = "grid" | "list";

type Props = {
  onSelect: (artifactId: string | null) => void;
  rows: ArtifactLibraryRow[];
  selectedId?: string;
};

type LibraryEntry = {
  artifactId: string;
  createdAtMs: number;
  kind: ArtifactRendererKind;
  label: string;
  routeLabel: string;
  runId: string;
  source: string;
  status: string;
};

const GRID_CARD_HEIGHT = 126;
const GRID_GAP = 8;
const GRID_MIN_COLUMN_WIDTH = 180;
const LIST_ROW_HEIGHT = 42;

export function ArtifactLibrary({ onSelect, rows, selectedId }: Props) {
  const [filter, setFilter] = useState<ArtifactFilter>("all");
  const [sort, setSort] = useState<ArtifactSort>("newest");
  const [view, setView] = useState<ArtifactView>("grid");
  const entries = useMemo(() => rows.map(libraryEntry).filter((entry): entry is LibraryEntry => entry !== null), [rows]);
  const filtered = useMemo(() => entries
    .filter((entry) => matchesFilter(entry.kind, filter))
    .sort(sorter(sort)), [entries, filter, sort]);
  const selectedIndex = filtered.findIndex((entry) => entry.artifactId === selectedId);
  const [focusedIndex, setFocusedIndex] = useState(() => Math.max(0, selectedIndex));
  const [gridColumns, setGridColumns] = useState(1);
  const gridRowCount = Math.ceil(filtered.length / gridColumns);
  const listVirtualRows = useFixedVirtualRange({ count: filtered.length, enabled: view === "list", itemSize: LIST_ROW_HEIGHT });
  const gridVirtualRows = useFixedVirtualRange({
    count: gridRowCount,
    enabled: view === "grid",
    itemSize: GRID_CARD_HEIGHT + GRID_GAP,
  });
  useLayoutEffect(() => {
    setGridColumns(Math.max(1, Math.floor((gridVirtualRows.viewportWidth - GRID_GAP) / (GRID_MIN_COLUMN_WIDTH + GRID_GAP))));
  }, [gridVirtualRows.viewportWidth]);
  const activeVirtualRows = view === "grid" ? gridVirtualRows : listVirtualRows;

  useEffect(() => {
    setFocusedIndex(Math.max(0, selectedIndex));
  }, [filter, filtered.length, selectedIndex, sort, view]);

  const focusEntry = (nextIndex: number) => {
    const target = Math.max(0, Math.min(filtered.length - 1, nextIndex));
    const viewport = activeVirtualRows.scrollRef.current;
    if (!viewport) return;
    const targetRow = view === "grid" ? Math.floor(target / gridColumns) : target;
    const rowHeight = view === "grid" ? GRID_CARD_HEIGHT + GRID_GAP : LIST_ROW_HEIGHT;
    const top = targetRow * rowHeight;
    const bottom = top + rowHeight;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (bottom > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = bottom - viewport.clientHeight;
    setFocusedIndex(target);
    window.requestAnimationFrame(() => {
      viewport.querySelector<HTMLButtonElement>(`[data-artifact-index="${target}"]`)?.focus();
    });
  };

  const onEntryKeyDown = (event: KeyboardEvent<HTMLButtonElement>, entryIndex: number) => {
    if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
    const step = view === "grid" ? gridColumns : 1;
    const page = Math.max(1, Math.floor(activeVirtualRows.viewportHeight / (view === "grid" ? GRID_CARD_HEIGHT + GRID_GAP : LIST_ROW_HEIGHT))) * step;
    const target = event.key === "ArrowRight" && view === "grid" ? entryIndex + 1
      : event.key === "ArrowLeft" && view === "grid" ? entryIndex - 1
        : event.key === "ArrowDown" ? entryIndex + step
          : event.key === "ArrowUp" ? entryIndex - step
            : event.key === "PageDown" ? entryIndex + page
              : event.key === "PageUp" ? entryIndex - page
                : event.key === "Home" ? 0
                  : event.key === "End" ? filtered.length - 1
                    : null;
    if (target === null) return;
    event.preventDefault();
    focusEntry(target);
  };

  const renderEntry = (entry: LibraryEntry, entryIndex: number) => (
    <button
      aria-posinset={entryIndex + 1}
      aria-pressed={selectedId === entry.artifactId}
      aria-setsize={filtered.length}
      className={`artifact-library-item ${selectedId === entry.artifactId ? "is-active" : ""}`}
      data-artifact-index={entryIndex}
      key={entry.artifactId}
      onClick={() => onSelect(selectedId === entry.artifactId ? null : entry.artifactId)}
      onFocus={() => setFocusedIndex(entryIndex)}
      onKeyDown={(event) => onEntryKeyDown(event, entryIndex)}
      tabIndex={focusedIndex === entryIndex ? 0 : -1}
      type="button"
    >
      <span className={`artifact-library-thumbnail is-${entry.kind}`}><Glyph name={kindGlyph(entry.kind)} size={view === "grid" ? 22 : 14} /></span>
      <span className="artifact-library-copy">
        <strong title={entry.label}>{entry.label}</strong>
        <small>{entry.routeLabel || entry.source || entry.status || rendererLabel(entry.kind)}</small>
      </span>
      <span className="artifact-library-meta">
        <em>{rendererLabel(entry.kind)}</em>
        <time>{formatArtifactTime(entry.createdAtMs)}</time>
      </span>
      {view === "list" ? <span className="artifact-library-run" title={entry.runId}>{entry.runId || "Not bound to a run"}</span> : null}
    </button>
  );

  return (
    <div className="artifact-library">
      <div className="artifact-library-toolbar">
        <div className="artifact-library-filters" role="group" aria-label="Filter artifacts by renderer">
          {(["all", "documents", "images", "code", "data", "media", "text", "opaque"] as const).map((value) => (
            <button aria-pressed={filter === value} className={filter === value ? "is-active" : ""} key={value} onClick={() => setFilter(value)} type="button">
              {filterLabel(value)}
            </button>
          ))}
        </div>
        <div className="artifact-library-display-controls">
          <label>
            <span>Sort</span>
            <select aria-label="Sort artifacts" onChange={(event) => setSort(event.target.value as ArtifactSort)} value={sort}>
              <option value="newest">Newest</option>
              <option value="name">Name</option>
              <option value="type">Type</option>
            </select>
          </label>
          <button aria-label="Grid view" aria-pressed={view === "grid"} className={view === "grid" ? "is-active" : ""} onClick={() => setView("grid")} type="button"><Glyph name="viewAll" size={13} /></button>
          <button aria-label="List view" aria-pressed={view === "list"} className={view === "list" ? "is-active" : ""} onClick={() => setView("list")} type="button"><Glyph name="registry" size={13} /></button>
        </div>
      </div>

      {filtered.length ? (
        view === "grid" ? (
          <div className="artifact-library-viewport" onScroll={gridVirtualRows.onScroll} ref={gridVirtualRows.setScrollRef}>
            <div className="artifact-library-virtual-grid" style={{ height: gridRowCount * (GRID_CARD_HEIGHT + GRID_GAP) + GRID_GAP }}>
              {Array.from({ length: gridVirtualRows.end - gridVirtualRows.start }, (_, offset) => {
                const rowIndex = gridVirtualRows.start + offset;
                const firstEntry = rowIndex * gridColumns;
                return (
                  <div className="artifact-library-virtual-grid-row" key={rowIndex} style={{ gridTemplateColumns: `repeat(${gridColumns}, minmax(0, 1fr))`, transform: `translateY(${GRID_GAP + rowIndex * (GRID_CARD_HEIGHT + GRID_GAP)}px)` }}>
                    {filtered.slice(firstEntry, firstEntry + gridColumns).map((entry, entryOffset) => renderEntry(entry, firstEntry + entryOffset))}
                  </div>
                );
              })}
            </div>
          </div>
        ) : (
          <div className="artifact-library-viewport" onScroll={listVirtualRows.onScroll} ref={listVirtualRows.setScrollRef}>
            <div className="artifact-library-virtual-list" style={{ height: listVirtualRows.totalSize }}>
              <div style={{ transform: `translateY(${listVirtualRows.start * LIST_ROW_HEIGHT}px)` }}>
                {filtered.slice(listVirtualRows.start, listVirtualRows.end).map((entry, offset) => renderEntry(entry, listVirtualRows.start + offset))}
              </div>
            </div>
          </div>
        )
      ) : (
        <div className="artifact-library-empty"><Glyph name="artifact" size={22} /><strong>No matching artifacts</strong><span>Choose another renderer filter or clear the registry search.</span></div>
      )}
    </div>
  );
}

function libraryEntry(row: ArtifactLibraryRow): LibraryEntry | null {
  const artifactId = text(row.artifact_id) || text(row.id);
  if (!artifactId) return null;
  const label = text(row.label) || artifactId.split(":", 1)[0] || "Unlabelled artifact";
  return {
    artifactId,
    createdAtMs: timestamp(row.created_at),
    kind: artifactKindHint(label),
    label,
    routeLabel: text(row.route_label),
    runId: text(row.run_id),
    source: text(row.source),
    status: text(row.status),
  };
}

function matchesFilter(kind: ArtifactRendererKind, filter: ArtifactFilter): boolean {
  if (filter === "all") return true;
  if (filter === "data") return kind === "json" || kind === "notebook";
  if (filter === "documents") return kind === "document" || kind === "pdf";
  if (filter === "images") return kind === "image";
  if (filter === "opaque") return kind === "unsupported";
  return kind === filter;
}

function sorter(sort: ArtifactSort): (left: LibraryEntry, right: LibraryEntry) => number {
  if (sort === "name") return (left, right) => left.label.localeCompare(right.label) || right.createdAtMs - left.createdAtMs;
  if (sort === "type") return (left, right) => left.kind.localeCompare(right.kind) || left.label.localeCompare(right.label);
  return (left, right) => right.createdAtMs - left.createdAtMs || left.label.localeCompare(right.label);
}

function filterLabel(value: ArtifactFilter): string {
  if (value === "all") return "All";
  if (value === "data") return "Data";
  if (value === "documents") return "Docs";
  if (value === "images") return "Images";
  if (value === "opaque") return "Other";
  return value[0].toLocaleUpperCase() + value.slice(1);
}

function rendererLabel(kind: ArtifactRendererKind): string {
  if (kind === "unsupported") return "Opaque";
  return kind[0].toLocaleUpperCase() + kind.slice(1);
}

function kindGlyph(kind: ArtifactRendererKind): GlyphName {
  if (kind === "image") return "image";
  if (kind === "media") return "play";
  if (kind === "code" || kind === "notebook") return "terminal";
  if (kind === "json") return "descriptor";
  if (kind === "document" || kind === "pdf" || kind === "text") return "file";
  return "artifact";
}

function formatArtifactTime(value: number): string {
  if (!value) return "Unknown time";
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }).format(value);
}

function timestamp(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const parsed = new Date(value).valueOf();
    if (Number.isFinite(parsed)) return parsed;
  }
  return 0;
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
