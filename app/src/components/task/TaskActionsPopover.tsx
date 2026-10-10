import { useEffect, useId, useLayoutEffect, useRef, useState, type ComponentProps, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { OcMenuItem, OcMenuSurface } from "../../design-system/components";
import { restoreModalFocus } from "../../design-system/modalFocus";
import { formatKeyboardShortcuts, keyboardShortcutFromEvent } from "../../keyboardShortcuts";
import type { ClientTaskActionsMenuRequest } from "../../shell/taskActionsMenu";
import { Glyph } from "../IconGlyphs";
import "./task-actions.css";

export type TaskAction = {
  id: string;
  label: string;
  description?: string;
  type?: "normal" | "radio";
  enabled?: boolean;
  checked?: boolean;
  icon?: ComponentProps<typeof OcMenuItem>["leadingIcon"];
  tone?: "danger";
  shortcuts?: readonly string[];
  submenu?: TaskAction[];
  onSelect?: () => void;
} | { type: "separator" };

export function TaskActionsPopover({ anchor, actions, title, onClose, onSelect }: {
  anchor: ClientTaskActionsMenuRequest;
  actions: TaskAction[];
  title: string;
  onClose: () => void;
  onSelect: (id: string) => void;
}) {
  const id = useId();
  const layerRef = useRef<HTMLDivElement>(null);
  const [path, setPath] = useState<string[]>([]);
  const focusDepthRef = useRef<number | null>(0);
  const restoreFocusRef = useRef(true);
  const previousFocusRef = useRef(anchor.anchorElement ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null));
  const callbacks = useRef({ onClose, onSelect });
  callbacks.current = { onClose, onSelect };
  const levels = [{ title: `Actions for ${title}`, actions }];
  for (const selected of path) {
    const parent = levels.at(-1)!.actions.find((item) => item.type !== "separator" && item.id === selected);
    if (!parent || parent.type === "separator" || parent.enabled === false || !parent.submenu) break;
    levels.push({ title: parent.label, actions: parent.submenu });
  }
  const panel = (depth: number) => layerRef.current?.querySelector<HTMLElement>(`[data-task-menu-depth="${depth}"]`);
  const items = (depth: number) => [...(panel(depth)?.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]:not(:disabled)') ?? [])];
  const parentItem = (depth: number) => items(depth).find((item) => item.dataset.taskAction === path[depth]);

  useLayoutEffect(() => {
    const layer = layerRef.current;
    if (!layer) return;
    const place = () => {
      if (anchor.anchorElement && !anchor.anchorElement.isConnected) { callbacks.current.onClose(); return; }
      const panels = [...layer.querySelectorAll<HTMLElement>('[data-task-menu-depth]')];
      for (const [depth, current] of panels.entries()) {
        current.style.maxWidth = `${Math.max(0, window.innerWidth - 16)}px`;
        current.style.maxHeight = `${Math.max(0, window.innerHeight - 16)}px`;
        const bounds = current.getBoundingClientRect();
        let left: number, top: number;
        if (depth === 0) {
          const trigger = anchor.anchorElement?.getBoundingClientRect();
          left = anchor.pointer?.x ?? (trigger?.right ?? anchor.anchorRight) - bounds.width;
          top = anchor.pointer?.y ?? (trigger?.bottom ?? anchor.anchorBottom) + 4;
        } else {
          const trigger = parentItem(depth - 1)?.getBoundingClientRect();
          if (!trigger) continue;
          left = trigger.right + 6 + bounds.width <= window.innerWidth - 8 ? trigger.right + 6 : trigger.left - bounds.width - 6;
          top = trigger.top - 4;
        }
        current.style.left = `${Math.max(8, Math.min(left, window.innerWidth - bounds.width - 8))}px`;
        current.style.top = `${Math.max(8, Math.min(top, window.innerHeight - bounds.height - 8))}px`;
      }
    };
    place();
    const observer = new ResizeObserver(place);
    layer.querySelectorAll<HTMLElement>('[data-task-menu-depth]').forEach((element) => observer.observe(element));
    if (anchor.anchorElement) observer.observe(anchor.anchorElement);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    if (focusDepthRef.current !== null) {
      items(focusDepthRef.current)[0]?.focus({ preventScroll: true });
      focusDepthRef.current = null;
    }
    return () => { observer.disconnect(); window.removeEventListener("resize", place); window.removeEventListener("scroll", place, true); };
  }, [anchor, path]);

  useEffect(() => {
    const trigger = anchor.anchorElement;
    const original = new Map(["aria-expanded", "aria-controls", "data-state"].map((name) => [name, trigger?.getAttribute(name)]));
    trigger?.setAttribute("aria-expanded", "true");
    trigger?.setAttribute("aria-controls", id);
    trigger?.setAttribute("data-state", "open");
    const outside = (event: Event) => {
      if (event.composedPath().includes(layerRef.current!) || trigger && event.composedPath().includes(trigger)) return;
      restoreFocusRef.current = false;
      callbacks.current.onClose();
    };
    const blur = () => { restoreFocusRef.current = false; callbacks.current.onClose(); };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("focusin", outside);
    window.addEventListener("blur", blur);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("focusin", outside);
      window.removeEventListener("blur", blur);
      for (const [name, value] of original) {
        if (value == null) trigger?.removeAttribute(name);
        else trigger?.setAttribute(name, value);
      }
      if (restoreFocusRef.current && document.hasFocus()) restoreModalFocus(previousFocusRef.current);
    };
  }, [anchor, id]);

  const expand = (depth: number, action: Exclude<TaskAction, { type: "separator" }>, focus: boolean) => {
    if (action.enabled === false) return;
    if (focus) focusDepthRef.current = depth + 1;
    setPath((current) => current[depth] === action.id && current.length === depth + 1
      ? current : [...current.slice(0, depth), action.id]);
    if (focus && path[depth] === action.id) items(depth + 1)[0]?.focus({ preventScroll: true });
  };
  const collapse = (depth: number) => {
    const parent = parentItem(depth - 1);
    setPath((current) => current.slice(0, depth - 1));
    parent?.focus({ preventScroll: true });
  };
  const keyDown = (event: KeyboardEvent<HTMLDivElement>, depth: number) => {
    if (event.defaultPrevented) return;
    const shortcut = keyboardShortcutFromEvent(event);
    const shortcutAction = (entries: TaskAction[]): string | undefined => {
      for (const action of entries) {
        if (action.type === "separator" || action.enabled === false) continue;
        if (shortcut && action.shortcuts?.includes(shortcut) && !action.submenu) return action.id;
        const nested = action.submenu && shortcutAction(action.submenu);
        if (nested) return nested;
      }
    };
    const actionId = shortcutAction(actions);
    if (actionId && !event.repeat && !event.nativeEvent.isComposing) {
      event.preventDefault(); event.stopPropagation(); callbacks.current.onSelect(actionId); return;
    }
    const rows = items(depth), current = document.activeElement as HTMLButtonElement, index = rows.indexOf(current);
    if (event.key === "Escape" || event.key === "ArrowLeft" && depth > 0) {
      event.preventDefault(); event.stopPropagation();
      if (depth > 0) collapse(depth); else callbacks.current.onClose();
      return;
    }
    if (event.key === "Tab") { event.preventDefault(); callbacks.current.onClose(); return; }
    if (event.key === "ArrowRight") {
      const action = levels[depth].actions.find((item) => item.type !== "separator" && item.id === current?.dataset.taskAction);
      if (action && action.type !== "separator" && action.submenu) { event.preventDefault(); expand(depth, action, true); }
      return;
    }
    const next = event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1
      : event.key === "ArrowDown" ? (index + 1) % rows.length : event.key === "ArrowUp" ? index < 0 ? rows.length - 1 : (index + rows.length - 1) % rows.length : null;
    if (next !== null) { event.preventDefault(); rows[next]?.focus({ preventScroll: true }); }
  };

  return createPortal(<div className="task-actions-layer" data-settings-layer="true" ref={layerRef}>
    {levels.map((level, depth) => <OcMenuSurface key={depth} id={depth === 0 ? id : `${id}-${depth}`}
      aria-label={level.title} aria-orientation="vertical" className="task-actions-menu" data-task-menu-depth={depth}
      role="menu" tabIndex={-1} onKeyDown={(event) => keyDown(event, depth)}>
      {level.actions.map((action, index) => action.type === "separator"
        ? <div key={`separator:${index}`} role="separator" className="task-actions-separator" />
        : <OcMenuItem key={action.id} label={action.label} description={action.description} leadingIcon={action.icon}
          disabled={action.enabled === false} role={action.type === "radio" ? "menuitemradio" : "menuitem"}
          aria-checked={action.type === "radio" ? action.checked : undefined} data-task-action={action.id}
          aria-haspopup={action.submenu ? "menu" : undefined} aria-expanded={action.submenu ? path[depth] === action.id : undefined}
          aria-controls={action.submenu && path[depth] === action.id ? `${id}-${depth + 1}` : undefined}
          tone={action.tone} tabIndex={-1}
          trailing={action.submenu ? <Glyph className="-rotate-90" name="chevronDown" size={16} /> : action.checked ? <Glyph name="check" size={16} /> : action.shortcuts?.length ? <span className="task-action-shortcut">{formatKeyboardShortcuts(action.shortcuts)}</span> : undefined}
          onPointerEnter={(event) => {
            if (event.pointerType !== "mouse" || action.enabled === false) return;
            event.currentTarget.focus({ preventScroll: true });
            if (action.submenu) expand(depth, action, false); else setPath((current) => current.slice(0, depth));
          }}
          onClick={() => action.submenu ? expand(depth, action, true) : callbacks.current.onSelect(action.id)} />)}
    </OcMenuSurface>)}
  </div>, document.body);
}
