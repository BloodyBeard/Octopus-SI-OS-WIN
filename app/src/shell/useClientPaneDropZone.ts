import { useEffect } from "react";
import { appToast } from "../toasts";

export function useClientPaneDropZone(enabled: boolean) {
  useEffect(() => {
    if (!enabled) return;
    const bridge = window.octopusTransport;
    const element = document.querySelector<HTMLElement>(".workspace-pane-manager");
    if (!bridge || !element) return;
    const update = () => {
      const { x, y, width, height } = element.getBoundingClientRect();
      void bridge.registerClientPaneDropZone(width > 0 && height > 0 ? { x, y, width, height } : null).catch((error) => appToast.danger(`Pane drop zone registration failed: ${String(error)}`));
    };
    const observer = new ResizeObserver(update);
    observer.observe(element);
    window.addEventListener("resize", update);
    update();
    return () => { observer.disconnect(); window.removeEventListener("resize", update); void bridge.registerClientPaneDropZone(null).catch(() => undefined); };
  }, [enabled]);
}
