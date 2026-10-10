import { useMemo, type ReactNode } from "react";
import { cx } from "../design-system/components";
import { jsonText } from "../runtimeFormat";
import { SurfaceCopyButton } from "./SurfaceCopyButton";

type JsonPanelProps = {
  value?: unknown;
  text?: string;
  label?: ReactNode;
  className?: string;
  preClassName?: string;
  copyLabel?: string;
};

export function JsonPanel({
  value,
  text,
  label,
  className,
  preClassName = "registry-json",
  copyLabel = "Copy JSON",
}: JsonPanelProps) {
  const body = useMemo(() => text ?? jsonText(value ?? {}), [text, value]);

  return (
    <div className={cx("json-panel", className)}>
      {label ? <div className="json-panel-label">{label}</div> : null}
      <div className="json-panel-shell">
        <SurfaceCopyButton label={copyLabel} value={body} />
        <pre className={preClassName}>{body}</pre>
      </div>
    </div>
  );
}
