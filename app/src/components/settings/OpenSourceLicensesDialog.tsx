import { useEffect, useMemo, useRef, useState } from "react";
import {
  OcButton,
  OcDialog,
  OcDialogFooter,
  OcDialogHeader,
  OcEmptyState,
  OcSearchField,
} from "../../design-system/components";

type LicenseEntry = {
  name: string;
  version: string;
  license: string;
  notice: string;
};

export function OpenSourceLicensesDialog({ onClose, open }: { onClose: () => void; open: boolean }) {
  const searchRef = useRef<HTMLInputElement>(null);
  const [entries, setEntries] = useState<LicenseEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [query, setQuery] = useState("");
  const [selectedKey, setSelectedKey] = useState("");

  useEffect(() => {
    if (!open || entries.length) return;
    let cancelled = false;
    setLoading(true);
    setLoadError("");
    void import("../../generated/client-licenses.json")
      .then((module) => {
        if (cancelled) return;
        const next = (module.default as { entries?: LicenseEntry[] }).entries ?? [];
        setEntries(next);
        setSelectedKey(next[0] ? licenseKey(next[0]) : "");
      })
      .catch((failure: unknown) => {
        if (!cancelled) setLoadError(failure instanceof Error ? failure.message : "The dependency inventory could not be loaded.");
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [entries.length, open]);

  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) return entries;
    return entries.filter((entry) => `${entry.name} ${entry.version} ${entry.license}`.toLocaleLowerCase().includes(needle));
  }, [entries, query]);
  const selected = filtered.find((entry) => licenseKey(entry) === selectedKey) ?? filtered[0] ?? null;

  if (!open) return null;
  return (
    <OcDialog
      ariaLabelledBy="open-source-licenses-title"
      initialFocusRef={searchRef}
      onClose={onClose}
      open={open}
      panelClassName="h-[min(720px,calc(100vh-3rem))] w-[min(980px,calc(100vw-3rem))] max-w-none"
    >
      <OcDialogHeader
        closeLabel="Close open-source licenses"
        description="Production dependencies bundled into the Octopus desktop renderer"
        onClose={onClose}
        title="Open-source licenses"
        titleId="open-source-licenses-title"
      />
        <OcSearchField
          className="mx-4 shrink-0"
          id="open-source-licenses-search"
          inputRef={searchRef}
          label="Search open-source licenses"
          onChange={setQuery}
          placeholder="Search packages or licenses"
          trailing={<span className="shrink-0 text-xs tabular-nums text-token-description-foreground">{filtered.length}</span>}
          value={query}
        />
        <div className="mt-3 grid min-h-0 flex-1 grid-cols-[minmax(220px,0.34fr)_minmax(0,1fr)] border-t border-token-border">
          <nav aria-label="Bundled dependencies" className="min-h-0 overflow-y-auto border-r border-token-border p-2">
            {filtered.map((entry) => {
              const key = licenseKey(entry);
              return (
                <button
                  aria-current={selected && licenseKey(selected) === key ? "page" : undefined}
                  className={`flex w-full min-w-0 items-center gap-2 rounded-lg px-2 py-1.5 text-left hover:bg-token-list-hover-background focus:outline-none focus-visible:ring-1 focus-visible:ring-token-focus-border${selected && licenseKey(selected) === key ? " bg-token-list-hover-background" : ""}`}
                  key={key}
                  onClick={() => setSelectedKey(key)}
                  type="button"
                >
                  <span className="min-w-0 flex-1 truncate text-sm">{entry.name}</span>
                  <span className="shrink-0 text-xs text-token-description-foreground">{entry.license}</span>
                </button>
              );
            })}
            {!loading && !loadError && filtered.length === 0 ? (
              <OcEmptyState className="min-h-0 py-6" size="compact" title="No matching dependencies" />
            ) : null}
          </nav>
          <main className="min-h-0 overflow-y-auto p-5">
            {loading ? <OcEmptyState className="h-full min-h-0" size="compact" title="Loading dependency notices…" /> : null}
            {loadError ? <OcEmptyState className="h-full min-h-0" detail={loadError} glyph="warning" size="compact" title="Dependency inventory unavailable" tone="danger" /> : null}
            {selected ? (
              <article>
                <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                  <h3 className="text-lg font-medium">{selected.name}</h3>
                  <span className="font-mono text-sm text-token-description-foreground">{selected.version}</span>
                  <span className="rounded-md bg-token-list-hover-background px-1.5 py-0.5 text-xs">{selected.license}</span>
                </div>
                {selected.notice ? (
                  <pre className="mt-5 whitespace-pre-wrap break-words font-mono text-xs leading-5 text-token-text-secondary">{selected.notice}</pre>
                ) : (
                  <p className="mt-5 text-sm text-token-description-foreground">This package declares {selected.license}, but its installed package does not include a root LICENSE or NOTICE file.</p>
                )}
              </article>
            ) : null}
          </main>
        </div>
        <OcDialogFooter leading="Generated from the production dependency graph at build time">
          <OcButton onClick={onClose} size="sm" variant="secondary">Done</OcButton>
        </OcDialogFooter>
    </OcDialog>
  );
}

function licenseKey(entry: LicenseEntry): string {
  return `${entry.name}@${entry.version}`;
}
