import { useEffect, useLayoutEffect, useRef, type RefObject } from "react";
import { restoreModalFocus } from "./modalFocus";

/** Focus, dismissal and viewport bounds for menus opened at a pointer position. */
export function useFloatingMenu(
  open: boolean,
  menuRef: RefObject<HTMLDivElement | null>,
  onClose: () => void,
  position: { left: number; top: number } | null,
) {
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const left = position?.left ?? 0, top = position?.top ?? 0;

  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!open || !menu) return;
    const place = () => {
      menu.style.maxHeight = `${Math.max(0, window.innerHeight - 8)}px`;
      menu.style.maxWidth = `${Math.max(0, window.innerWidth - 8)}px`;
      menu.style.overflowY = "auto";
      const bounds = menu.getBoundingClientRect();
      menu.style.left = `${Math.max(4, Math.min(left, window.innerWidth - bounds.width - 4))}px`;
      menu.style.top = `${Math.max(4, Math.min(top, window.innerHeight - bounds.height - 4))}px`;
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(menu);
    window.addEventListener("resize", place);
    return () => { observer.disconnect(); window.removeEventListener("resize", place); };
  }, [left, menuRef, open, top]);

  useEffect(() => {
    const menu = menuRef.current;
    if (!open || !menu) return;
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    let restore = true;
    const items = () => [...menu.querySelectorAll<HTMLElement>('[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]')]
      .filter((item) => !item.matches(':disabled, [aria-disabled="true"]') && !item.closest('[inert], [hidden], [aria-hidden="true"]'));
    const focus = (item: HTMLElement | undefined) => {
      for (const candidate of items()) candidate.tabIndex = candidate === item ? 0 : -1;
      item?.focus({ preventScroll: true });
    };
    const frame = window.requestAnimationFrame(() => focus(items()[0]));
    const dismiss = (event: PointerEvent) => {
      if (menu.contains(event.target as Node)) return;
      restore = false;
      closeRef.current();
    };
    const keydown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || menu.closest('[inert], [hidden], [aria-hidden="true"]')) return;
      if (event.key === "Escape") {
        event.preventDefault();
        closeRef.current();
        return;
      }
      if (!menu.contains(event.target as Node) || event.metaKey || event.ctrlKey || event.altKey) return;
      if (event.key === "Tab") { event.preventDefault(); closeRef.current(); return; }
      const available = items(), index = available.indexOf(document.activeElement as HTMLElement);
      const next = event.key === "Home" ? 0 : event.key === "End" ? available.length - 1
        : event.key === "ArrowDown" ? (index + 1) % available.length
          : event.key === "ArrowUp" ? (index - 1 + available.length) % available.length : null;
      if (next !== null) { event.preventDefault(); focus(available[next]); }
    };
    const blur = () => { restore = false; closeRef.current(); };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", keydown);
    window.addEventListener("blur", blur);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", keydown);
      window.removeEventListener("blur", blur);
      if (restore && (document.activeElement === document.body || menu.contains(document.activeElement))) restoreModalFocus(previousFocus);
    };
  }, [menuRef, open]);
}
