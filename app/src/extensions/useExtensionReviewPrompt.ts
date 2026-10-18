import { useEffect, useRef } from "react";
import { appToast } from "../toasts";
import type { ClientExtensionRegistryState } from "./types";

export function useExtensionReviewPrompt(
  registry: ClientExtensionRegistryState,
  enabled: boolean,
  openExtensions: () => void,
): void {
  const prompted = useRef(new Set<string>());
  const open = useRef(openExtensions);
  open.current = openExtensions;

  useEffect(() => {
    if (!enabled || !registry.hydrated || !registry.mutable) return;
    const review = { label: "Review", onSelect: () => open.current() };
    for (const extension of registry.extensions) {
      if (extension.admission !== "pending" || extension.lifecycle.receiptState === "invalid") continue;
      const key = `review:${extension.manifest.id}:${extension.sourceDigest}`;
      if (prompted.current.has(key)) continue;
      prompted.current.add(key);
      const update = extension.lifecycle.receiptState !== "missing";
      appToast.info(
        `${update ? "Updated" : "New"} extension “${extension.manifest.name}” is ready for review`,
        0,
        { action: review, dedupeKey: `extension-review:${extension.manifest.id}` },
      );
    }
    const refused = new Map<string, string>();
    for (const error of registry.errors) {
      const match = /^packages\/([a-z0-9][a-z0-9._-]{1,63})\/extension\.json: (.+)$/s.exec(error);
      if (match && !refused.has(match[1])) refused.set(match[1], match[2]);
    }
    for (const [id, reason] of refused) {
      const key = `refused:${id}:${reason}`;
      if (prompted.current.has(key)) continue;
      prompted.current.add(key);
      appToast.warning(
        `Extension package “${id}” was refused: ${reason.slice(0, 200)}`,
        0,
        { action: { label: "Details", onSelect: () => open.current() }, dedupeKey: `extension-refused:${id}` },
      );
    }
  }, [enabled, registry.errors, registry.extensions, registry.hydrated, registry.mutable]);
}
