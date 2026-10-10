import type { ReactNode } from "react";
import { cx } from "./components";

type OcPaneGroupProps = {
  main: ReactNode;
  aside?: ReactNode;
  asideOpen?: boolean;
  mainLabel?: string;
  asideLabel?: string;
  className?: string;
};

export function OcPaneGroup({
  main,
  aside,
  asideOpen = true,
  mainLabel = "Primary pane",
  asideLabel = "Detail pane",
  className,
}: OcPaneGroupProps) {
  return (
    <div
      className={cx("oc-pane-group", !asideOpen && "is-aside-collapsed", className)}
      data-oc-layout="pane-group"
      data-state={asideOpen ? "expanded" : "collapsed"}
    >
      <section className="oc-pane-group__main" aria-label={mainLabel}>
        {main}
      </section>
      {aside ? <div className="oc-pane-group__separator" aria-hidden="true" /> : null}
      {aside ? (
        <aside className="oc-pane-group__aside" aria-hidden={!asideOpen || undefined} inert={!asideOpen || undefined} aria-label={asideLabel}>
          {aside}
        </aside>
      ) : null}
    </div>
  );
}
