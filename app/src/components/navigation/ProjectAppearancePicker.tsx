import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useIsPresent } from "motion/react";
import { OcButton } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { DEFAULT_PROJECT_COLOR, PROJECT_COLOR_OPTIONS, projectColor, projectColorKey } from "../../design-system/projectColors";
import "./projectAppearancePicker.css";

type ProjectAppearancePickerProps = {
  projectName: string;
  selectedColor: string;
  selectedIcon: string;
  trigger: ReactNode;
  iconOptions: readonly { id: string; label: string; icon: ReactNode }[];
  disabled?: boolean;
  onColorChange: (color: string) => void;
  onIconChange: (icon: string) => void;
};

export function ProjectAppearancePicker({ projectName, selectedColor, selectedIcon, trigger, iconOptions, disabled = false, onColorChange, onIconChange }: ProjectAppearancePickerProps) {
  const [open, setOpen] = useState(false);
  const present = useIsPresent();
  const anchorRef = useRef<HTMLButtonElement>(null);
  const id = useId();
  const colorKey = projectColorKey(selectedColor);

  useEffect(() => {
    if (!present) setOpen(false);
  }, [present]);

  return (
    <>
      <button
        aria-label={`Change icon and color for ${projectName}`}
        className="project-appearance-trigger no-drag cursor-interaction"
        disabled={disabled || !present}
        onClick={(event) => {
          event.stopPropagation();
          setOpen(!open);
        }}
        ref={anchorRef}
        type="button"
      >
        {trigger}
      </button>
      <OcDropdown
        anchorRef={anchorRef}
        align="center"
        aria-label="Project appearance"
        className="project-appearance-picker"
        id={id}
        onClick={(event) => event.stopPropagation()}
        onCloseAutoFocus={(event) => {
          if (!present || !anchorRef.current?.isConnected) event.preventDefault();
        }}
        onKeyDown={(event) => {
          event.stopPropagation();
          if (event.key === "Escape") {
            event.preventDefault();
            setOpen(false);
          } else if (event.key === "Tab") {
            const buttons = event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)");
            const first = buttons[0];
            const last = buttons[buttons.length - 1];
            if (event.shiftKey && event.target === first) {
              event.preventDefault();
              last?.focus({ preventScroll: true });
            } else if (!event.shiftKey && event.target === last) {
              event.preventDefault();
              first?.focus({ preventScroll: true });
            }
          }
        }}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          document.getElementById(id)?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus({ preventScroll: true });
        }}
        onOpenChange={setOpen}
        open={open && present}
        role="dialog"
        sideOffset={6}
        surface="opaque"
      >
        <div aria-label="Project color" className="project-appearance-picker__colors" role="group">
          {PROJECT_COLOR_OPTIONS.map((option) => (
            <button
              aria-label={option === DEFAULT_PROJECT_COLOR ? "Use default color" : `Use ${option}`}
              aria-pressed={colorKey === option}
              className="project-appearance-picker__color cursor-interaction"
              disabled={disabled}
              key={option}
              onClick={() => onColorChange(option)}
              type="button"
            >
              <span aria-hidden="true" className="project-appearance-picker__ring">
                <span className="project-appearance-picker__dot" style={{ backgroundColor: projectColor(option) }} />
              </span>
            </button>
          ))}
        </div>
        <div className="project-appearance-picker__divider" />
        <div aria-label="Project icon" className="project-appearance-picker__icons" role="group" style={{ color: projectColor(selectedColor) }}>
          {iconOptions.map((option) => (
            <button
              aria-label={option.label}
              aria-pressed={selectedIcon === option.id}
              className="project-appearance-picker__icon cursor-interaction"
              disabled={disabled}
              key={option.id}
              onClick={() => onIconChange(option.id)}
              type="button"
            >
              <span aria-hidden="true" className="project-appearance-picker__artwork">{option.icon}</span>
            </button>
          ))}
        </div>
        <div className="project-appearance-picker__footer">
          <OcButton onClick={() => setOpen(false)} variant="secondary">Done</OcButton>
        </div>
      </OcDropdown>
    </>
  );
}
