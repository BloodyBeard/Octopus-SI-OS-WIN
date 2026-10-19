import { useId, useRef, useState, type ReactNode } from "react";
import { OcMenuItem, cx } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";

const FORM_GROUP_DIVIDERS = "flex flex-col [&>*:not(:last-child)]:relative [&>*:not(:last-child)]:after:pointer-events-none [&>*:not(:last-child)]:after:absolute [&>*:not(:last-child)]:after:bottom-0 [&>*:not(:last-child)]:after:content-['']";
const FORM_GROUP_VARIANT = {
  form: "border border-default bg-transparent [&>*:not(:last-child)]:after:inset-x-5 [&>*:not(:last-child)]:after:h-px [&>*:not(:last-child)]:after:bg-[var(--color-border-subtle)]",
  input: "border border-primary-outline bg-primary-soft [&>*:not(:last-child)]:after:inset-x-4 [&>*:not(:last-child)]:after:h-px [&>*:not(:last-child)]:after:bg-border",
} as const;
const FORM_VALUE_BUTTON = "no-drag cursor-interaction flex min-h-10 max-w-full min-w-0 items-center gap-2 rounded-md border-0 px-0 py-0 text-form-body font-normal whitespace-nowrap select-none text-tertiary not-disabled:not-aria-disabled:text-secondary not-disabled:not-aria-disabled:hover:text-default data-[state=open]:text-default focus:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0 disabled:cursor-default disabled:opacity-40";

export function ScheduleFormGroup({ children, className, variant = "form" }: { children: ReactNode; className?: string; variant?: keyof typeof FORM_GROUP_VARIANT }) {
  return <div className={cx(FORM_GROUP_DIVIDERS, "overflow-visible rounded-2xl", FORM_GROUP_VARIANT[variant], className)}>{children}</div>;
}

export function ScheduleFormRow({ control, label, labelSizing = "grow" }: { control: ReactNode; label: ReactNode; labelSizing?: "grow" | "content" }) {
  return (
    <div className="@container/settings-row flex min-h-12 items-center justify-between gap-3 px-5 max-sm:min-h-0 max-sm:flex-col max-sm:items-stretch max-sm:py-2">
      <div className={cx("flex min-w-0 items-center gap-3", labelSizing === "grow" && "flex-1")}>
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <div className="min-w-0 break-words text-form-body text-default">{label}</div>
        </div>
      </div>
      <div className="flex min-w-0 flex-1 items-center justify-end max-sm:justify-stretch">{control}</div>
    </div>
  );
}

export function ScheduleFormValue({ children }: { children: ReactNode }) {
  return <span className="min-w-0 truncate text-sm text-tertiary">{children}</span>;
}

export function ScheduleFormMenu<T extends string | number>({ ariaLabel, disabled, onSelect, options, value, valueLabel }: {
  ariaLabel: string;
  disabled?: boolean;
  onSelect: (value: T) => void;
  options: ReadonlyArray<{ id: T; label: string }>;
  value: T;
  valueLabel?: string;
}) {
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLButtonElement>(null);
  const selected = options.find((option) => option.id === value);
  return (
    <>
      <button
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label={ariaLabel}
        className={FORM_VALUE_BUTTON}
        data-state={open ? "open" : "closed"}
        disabled={disabled}
        onClick={() => setOpen(!open)}
        ref={anchorRef}
        type="button"
      >
        <span className="truncate">{valueLabel ?? selected?.label ?? String(value)}</span>
        <Glyph className="icon-xs shrink-0" name="chevronDown" size={16} />
      </button>
      <OcDropdown align="end" anchorRef={anchorRef} aria-label={ariaLabel} className="max-h-72 min-w-40" modal onOpenChange={setOpen} open={open}>
        {options.map((option) => (
          <OcMenuItem
            aria-checked={option.id === value}
            key={String(option.id)}
            label={option.label}
            onClick={() => { setOpen(false); if (option.id !== value) onSelect(option.id); }}
            role="menuitemradio"
            selected={option.id === value}
          />
        ))}
      </OcDropdown>
    </>
  );
}

export function ScheduleFormNumber({ ariaLabel, decimal = false, min, onChange, suffix, value }: {
  ariaLabel: string;
  decimal?: boolean;
  min: number;
  onChange: (value: number) => void;
  suffix?: ReactNode;
  value: number;
}) {
  const text = Number.isFinite(value) ? String(Math.round(value * 1000) / 1000) : "";
  return (
    <label className="flex items-center gap-1.5 text-form-body text-default">
      <input
        aria-label={ariaLabel}
        className="w-12 bg-transparent text-end tabular-nums outline-none"
        defaultValue={text}
        inputMode={decimal ? "decimal" : "numeric"}
        key={text}
        onBlur={(event) => {
          const next = Number(event.currentTarget.value);
          if (!event.currentTarget.value || !Number.isFinite(next) || next < min) { event.currentTarget.value = text; return; }
          const rounded = decimal ? next : Math.round(next);
          event.currentTarget.value = String(rounded);
          if (rounded !== value) onChange(rounded);
        }}
        onChange={(event) => { event.currentTarget.value = event.currentTarget.value.replaceAll(decimal ? /[^0-9.]/g : /[^0-9]/g, ""); }}
        onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }}
        type="text"
      />
      {suffix}
    </label>
  );
}

export function ScheduleFormDate({ ariaLabel, onChange, value }: { ariaLabel: string; onChange: (value: string) => void; value: string }) {
  const id = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const label = value ? new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", year: "numeric" }).format(new Date(`${value}T00:00:00`)) : "Choose a date";
  return (
    <div aria-label={ariaLabel} className="relative -me-1.5 text-base text-secondary" role="group">
      <button
        aria-controls={id}
        className={FORM_VALUE_BUTTON}
        onClick={() => { try { inputRef.current?.showPicker(); } catch { inputRef.current?.focus(); } }}
        type="button"
      >
        <span className="truncate">{label}</span>
        <Glyph className="icon-xs shrink-0" name="chevronDown" size={16} />
      </button>
      <input
        aria-label={ariaLabel}
        className="pointer-events-none absolute inset-0 opacity-0"
        id={id}
        onChange={(event) => { if (event.target.value) onChange(event.target.value); }}
        ref={inputRef}
        tabIndex={-1}
        type="date"
        value={value}
      />
    </div>
  );
}

export const QUARTER_HOURS: ReadonlyArray<{ hour: number; minute: number }> = Array.from({ length: 96 }, (_, index) => ({ hour: Math.floor(index / 4), minute: (index % 4) * 15 }));
