import { useEffect, useRef, useState } from "react";
import { cx } from "../design-system/components";
import { copyText } from "../runtimeFormat";
import { appToast } from "../toasts";
import { Glyph } from "./IconGlyphs";

const COPY_BUTTON_CLASS = "border-token-border no-drag cursor-interaction flex items-center justify-center border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full electron:rounded-md text-token-text-tertiary enabled:hover:bg-token-list-hover-background border-transparent electron:p-1 electron:[&>svg]:icon-sm p-0.5 focus-visible:ring-2 focus-visible:ring-token-focus-border focus-visible:ring-offset-0";

type SurfaceCopyButtonProps = {
  value: string;
  label: string;
  className?: string;
};

export function SurfaceCopyButton({ value, label, className }: SurfaceCopyButtonProps) {
  const [copied, setCopied] = useState(false);
  const resetTimerRef = useRef<number | null>(null);

  useEffect(() => () => {
    if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
  }, []);

  async function copy() {
    try {
      if (!await copyText(value)) throw new Error("Clipboard access is unavailable.");
      setCopied(true);
      if (resetTimerRef.current !== null) window.clearTimeout(resetTimerRef.current);
      resetTimerRef.current = window.setTimeout(() => {
        setCopied(false);
        resetTimerRef.current = null;
      }, 1_500);
    } catch (error) {
      appToast.warning(error instanceof Error ? error.message : String(error));
    }
  }

  const title = copied ? "Copied" : label;

  return (
    <button
      aria-label={title}
      className={cx("surface-copy-button", COPY_BUTTON_CLASS, copied && "text-token-foreground", className)}
      onClick={() => void copy()}
      title={title}
      type="button"
    >
      <Glyph className="icon-xs" name={copied ? "check" : "copy"} size={16} />
    </button>
  );
}
