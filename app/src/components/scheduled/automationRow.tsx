import { Fragment, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { OcMenuItem, OcSearchField, cx } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { OcDropdown } from "../../design-system/OcDropdown";

const ROW_CLASS = "automation-row @container group relative min-h-10 w-full rounded-lg px-3 py-3 text-start text-base";
const ROW_HIT_CLASS = "focus-visible:ring-ring absolute inset-0 cursor-interaction rounded-lg outline-none focus-visible:ring-2 focus-visible:ring-offset-0 disabled:cursor-default";
const ROW_ICON_BUTTON_CLASS = "border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-default rounded-md hover:bg-transparent border-transparent flex size-5 items-center justify-center p-0.5 aspect-square !px-0 text-text-description hover:text-default";
const ROW_MENU_BUTTON_CLASS = "border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-lg text-token-text-tertiary enabled:hover:bg-primary-ghost-hover data-[state=open]:bg-primary-ghost-hover border-transparent h-token-button-composer px-2 py-0 text-base leading-[18px] aspect-square items-center justify-center !px-0 text-text-description hover:text-default data-[state=open]:text-default";

export const SCHEDULED_SEARCH_ROW_CLASS = "sticky top-0 z-30 bg-surface after:pointer-events-none after:absolute after:top-full after:right-0 after:left-0 after:h-8 after:bg-linear-to-b after:from-surface after:to-transparent after:content-['']";

export type AutomationRowAction = {
  disabled?: boolean;
  glyph: GlyphName;
  label: string;
  onSelect: () => void;
  tone?: "default" | "danger";
};

export function AutomationRow({
  actions,
  ariaLabel,
  dimmed,
  onSelect,
  secondaryTitle,
  segments,
  statusIcon,
  title,
}: {
  actions?: AutomationRowAction[];
  ariaLabel: string;
  dimmed?: boolean;
  onSelect: () => void;
  secondaryTitle?: string;
  segments: string[];
  statusIcon: ReactNode;
  title: string;
}) {
  const descriptionId = useId();
  return (
    <div
      className={cx(
        ROW_CLASS,
        "cursor-interaction hover:bg-primary-ghost-hover",
        dimmed ? "opacity-60 hover:opacity-100 focus-within:opacity-100" : null,
      )}
    >
      <button aria-describedby={descriptionId} aria-label={ariaLabel} className={ROW_HIT_CLASS} onClick={onSelect} type="button" />
      <div className="pointer-events-none relative">
        <div className="relative flex min-w-0 items-start gap-2 pe-16">
          <span className="flex min-h-6 shrink-0 items-center">{statusIcon}</span>
          <div className="flex min-w-0 flex-1 flex-col gap-0">
            <div className="flex min-w-0 items-baseline gap-3">
              <div className="flex min-w-0 flex-1 items-baseline gap-2 text-base leading-6">
                <span className="min-w-0 truncate text-default">{title}</span>
                {secondaryTitle ? <span className="max-w-48 shrink-0 truncate text-text-description">{secondaryTitle}</span> : null}
              </div>
            </div>
            <div className="flex min-w-0 items-center justify-between gap-3 text-sm leading-4 text-text-description">
              <div className="min-w-0 flex-1">
                <span className="inline-flex max-w-full items-center gap-1.5 overflow-hidden whitespace-nowrap" id={descriptionId}>
                  {segments.map((segment, index) => (
                    <Fragment key={segment}>
                      <span className={index === 0 ? "min-w-0 truncate" : "shrink-0"}>{segment}</span>
                      <BulletSeparator />
                    </Fragment>
                  ))}
                </span>
              </div>
            </div>
          </div>
          <div className="absolute top-1/2 right-0 flex min-h-6 -translate-y-1/2 items-center text-base text-text-description">
            <span className="relative inline-flex min-w-16 items-center justify-end">
              {actions?.length ? <AutomationRowMenu actions={actions} /> : null}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}

export function AutomationStatusIcon({ glyph, tone }: { glyph: GlyphName; tone?: string }) {
  return <Glyph className={cx("icon-sm shrink-0", tone)} name={glyph} size={18} />;
}

export function AutomationStatusToggle({
  disabled,
  glyph,
  hoverGlyph,
  label,
  onToggle,
}: {
  disabled?: boolean;
  glyph: GlyphName;
  hoverGlyph?: GlyphName;
  label: string;
  onToggle: () => void;
}) {
  return (
    <span className={cx("pointer-events-auto inline-flex", hoverGlyph ? "group/status-toggle" : null)}>
      <button
        aria-label={label}
        className={ROW_ICON_BUTTON_CLASS}
        disabled={disabled}
        onClick={onToggle}
        title={label}
        type="button"
      >
        <span className="inline-flex">
          {hoverGlyph ? (
            <span className="relative inline-flex">
              <Glyph className="icon-sm group-focus-within/status-toggle:opacity-0 group-hover/status-toggle:opacity-0" name={glyph} size={18} />
              <Glyph className="icon-sm absolute inset-0 opacity-0 group-focus-within/status-toggle:opacity-100 group-hover/status-toggle:opacity-100" name={hoverGlyph} size={18} />
            </span>
          ) : (
            <Glyph className="icon-sm" name={glyph} size={18} />
          )}
        </span>
      </button>
    </span>
  );
}

function AutomationRowMenu({ actions }: { actions: AutomationRowAction[] }) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);

  return (
    <span className="pointer-events-none absolute right-0 inline-flex items-center justify-end opacity-0 group-focus-within:pointer-events-auto group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 has-[[data-state=open]]:pointer-events-auto has-[[data-state=open]]:opacity-100">
      <button
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label="Scheduled task actions"
        className={ROW_MENU_BUTTON_CLASS}
        data-state={open ? "open" : "closed"}
        onClick={(event) => {
          event.stopPropagation();
          setOpen((current) => !current);
        }}
        ref={triggerRef}
        type="button"
      >
        <Glyph className="icon-xs" name="more" size={16} />
      </button>
      <OcDropdown anchorRef={triggerRef} align="end" aria-label="Scheduled task actions" onOpenChange={setOpen} open={open} sideOffset={4} style={{ width: 208 }}>
        {actions.map((action) => (
          <OcMenuItem disabled={action.disabled} key={action.label} label={action.label} leadingIcon={action.glyph} onClick={action.onSelect} role="menuitem" tone={action.tone} />
        ))}
      </OcDropdown>
    </span>
  );
}

export function ScheduledSearchField({ searchFocusTick = 0, ...props }: {
  searchFocusTick?: number;
  id: string;
  label: string;
  onChange: (next: string) => void;
  placeholder: string;
  value: string;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => { if (searchFocusTick > 0) inputRef.current?.focus(); }, [searchFocusTick]);
  return <OcSearchField {...props} className="flex-1" inputRef={inputRef} />;
}

function BulletSeparator() {
  return <span aria-hidden="true" className="last:hidden">·</span>;
}
