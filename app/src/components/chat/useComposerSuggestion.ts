import { useEffect, useState } from "react";
import type { ComposerSuggestion } from "./composerSuggestion";

export function useComposerSuggestion(runId: string, enabled: boolean): ComposerSuggestion | null {
  const [suggestion, setSuggestion] = useState<ComposerSuggestion | null>(null);
  useEffect(() => {
    setSuggestion(null);
    if (!enabled || !runId || !window.octopusTransport?.requestComposerPrediction) return;
    let current = true;
    const timer = setTimeout(() => {
      void window.octopusTransport!.requestComposerPrediction({ runId }).then((result) => {
        if (current && result?.text) setSuggestion({ id: `${runId}:${result.id}`, text: result.text });
      }).catch(() => { if (current) setSuggestion(null); });
    }, 500);
    return () => { current = false; clearTimeout(timer); };
  }, [runId, enabled]);
  return enabled ? suggestion : null;
}
