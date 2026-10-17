import { useEffect, useRef, useState } from "react";
import type { ComposerSuggestion } from "./composerSuggestion";

export function useComposerSuggestion(runId: string, sourceSequence: number | undefined, enabled: boolean): ComposerSuggestion | null {
  const key = JSON.stringify([runId, sourceSequence]);
  const [resolved, setResolved] = useState<{ key: string; suggestion: ComposerSuggestion | null } | null>(null);
  const request = useRef<{ key: string; result: Promise<ComposerSuggestion | null> } | null>(null);
  useEffect(() => {
    if (!enabled || !runId || sourceSequence === undefined || !window.octopusTransport?.requestComposerPrediction) return;
    let current = true;
    const timer = setTimeout(() => {
      if (request.current?.key !== key) {
        request.current = { key, result: window.octopusTransport!.requestComposerPrediction({ runId })
          .then((result) => result?.text ? { id: `${runId}:${result.id}`, text: result.text } : null)
          .catch(() => null) };
      }
      void request.current.result.then((suggestion) => {
        if (current) setResolved({ key, suggestion });
      });
    }, 500);
    return () => { current = false; clearTimeout(timer); };
  }, [key, runId, sourceSequence, enabled]);
  return enabled && resolved?.key === key ? resolved.suggestion : null;
}
