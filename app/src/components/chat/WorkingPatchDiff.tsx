import { parsePatchFiles, preloadHighlighter } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import { useEffect, useMemo, useState } from "react";
import { useAppearance } from "../../appearance";
import { copyText } from "../../runtimeFormat";
import { Glyph } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import { octopusDiffRawUnsafeCss } from "../settings/octopusDiffUnsafeCss";
import { octopusDiffThemeName, registerOctopusDiffThemes } from "../settings/octopusDiffThemes";

const registrationKey = Symbol.for("octopus.diff-themes.registered");
const registry = globalThis as typeof globalThis & { [registrationKey]?: boolean };
if (!registry[registrationKey]) {
  registerOctopusDiffThemes();
  registry[registrationKey] = true;
}

const workingPatchUnsafeCss = `${octopusDiffRawUnsafeCss}
[data-diff] {
  --octopus-diffs-surface: var(--color-token-main-surface-primary);
  --diffs-addition-color-override: var(--color-token-git-decoration-added-resource-foreground);
  --diffs-deletion-color-override: var(--color-token-git-decoration-deleted-resource-foreground);
  --diffs-gap-block: 0;
  --diffs-min-number-column-width: 1ch;
  --diffs-font-family: var(--font-mono);
  --diffs-font-size: var(--octopus-chat-code-font-size, 12px);
  --diffs-line-height: round(nearest, calc(var(--diffs-font-size, 12px) * 1.8), 1px);
  line-height: var(--diffs-line-height);
}`;

export function parseWorkingPatch(patch: string, path: string, changeKind: "add" | "delete" | "update") {
  const hunksOnly = patch.startsWith("@@ ");
  const fileName = JSON.stringify(path);
  const renderedPatch = hunksOnly
    ? `--- ${changeKind === "add" ? "/dev/null" : fileName}\n+++ ${changeKind === "delete" ? "/dev/null" : fileName}\n${patch}`
    : patch;
  try {
    const parsed = parsePatchFiles(renderedPatch, undefined, true);
    if (parsed.length !== 1 || parsed[0].files.length !== 1) {
      throw new Error("Expected one recorded file diff.");
    }
    const fileDiff = parsed[0].files[0];
    return { patch: renderedPatch, fileDiff: hunksOnly ? { ...fileDiff, name: path } : fileDiff, error: null };
  } catch (error) {
    return { patch, fileDiff: null, error: error instanceof Error ? error.message : String(error) };
  }
}

export default function WorkingPatchDiff({ added, changeKind, deleted, patch, path }: {
  added?: number;
  changeKind: "add" | "delete" | "update";
  deleted?: number;
  patch: string;
  path: string;
}) {
  const appearance = useAppearance();
  const darkTheme = octopusDiffThemeName("dark", appearance.darkCodeTheme);
  const lightTheme = octopusDiffThemeName("light", appearance.lightCodeTheme);
  const [ready, setReady] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const parsed = useMemo(() => parseWorkingPatch(patch, path, changeKind), [patch, path, changeKind]);

  useEffect(() => {
    let active = true;
    setReady(false);
    setLoadError(null);
    void preloadHighlighter({
      langs: ["typescript", "tsx", "javascript", "jsx", "json", "markdown", "css", "html", "rust", "python", "shell"],
      themes: [darkTheme, lightTheme],
    }).then(() => {
      if (active) setReady(true);
    }).catch((error) => {
      if (active) setLoadError(error instanceof Error ? error.message : String(error));
    });
    return () => { active = false; };
  }, [darkTheme, lightTheme]);

  if (!ready && !parsed.error && !loadError) return <div className="activity-file-diff-loading">Loading diff…</div>;
  return (
    <div className="activity-file-patch">
      <div className="activity-file-patch-header">
        <ShellTooltip label={path}><span className="min-w-0 truncate">{path.split("/").at(-1) || path}</span></ShellTooltip>
        <span className="activity-file-patch-actions">
          {added !== undefined || deleted !== undefined ? (
            <span className="activity-file-stats">
              <b>+{added ?? 0}</b>
              <i>−{deleted ?? 0}</i>
            </span>
          ) : null}
          <ShellTooltip label={copied ? "Copied" : "Copy diff"}>
            <button
              aria-label={copied ? "Copied diff" : "Copy diff"}
              className="activity-file-diff-copy"
              onClick={() => {
                void copyText(parsed.patch).then((didCopy) => {
                  setCopied(didCopy);
                  if (!didCopy) return;
                  window.setTimeout(() => setCopied(false), 1_500);
                }).catch(() => setCopied(false));
              }}
              type="button"
            >
              <Glyph name={copied ? "check" : "copy"} size={12} />
            </button>
          </ShellTooltip>
        </span>
      </div>
      <div className="activity-file-patch-content">
        {parsed.fileDiff && !loadError ? <FileDiff
          disableWorkerPool
          options={{
            diffIndicators: appearance.diffMarkerStyle === "symbols" ? "classic" : "bars",
            diffStyle: "unified",
            disableFileHeader: true,
            expansionLineCount: 8,
            hunkSeparators: "simple",
            lineDiffType: "word",
            overflow: "scroll",
            theme: { dark: darkTheme, light: lightTheme },
            themeType: appearance.themeMode,
            unsafeCSS: workingPatchUnsafeCss,
          }}
          fileDiff={parsed.fileDiff}
        /> : <div className="activity-file-empty-detail" role="alert">Couldn’t render this diff: {parsed.error || loadError}</div>}
      </div>
    </div>
  );
}
