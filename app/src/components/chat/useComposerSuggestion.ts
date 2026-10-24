import { useEffect, useState } from "react";
import type { ComposerSuggestion } from "./composerSuggestion";

const suggestions = new Map<string, ComposerSuggestion | null>();
const requests = new Map<string, Promise<ComposerSuggestion | null>>();

function requestSuggestion(key: string, runId: string): Promise<ComposerSuggestion | null> {
  const pending = requests.get(key);
  if (pending) return pending;
  const request = window.octopusTransport!.requestComposerPrediction!({ runId })
    .then((result) => {
      const suggestion = result?.text ? { id: `${runId}:${result.id}`, text: result.text } : null;
      suggestions.set(key, suggestion);
      while (suggestions.size > 128) suggestions.delete(suggestions.keys().next().value!);
      return suggestion;
    })
    .catch(() => null)
    .finally(() => requests.delete(key));
  requests.set(key, request);
  return request;
}

export function useComposerSuggestion(runId: string, sourceSequence: number | undefined, enabled: boolean, runtimeHome: string): ComposerSuggestion | null {
  const key = JSON.stringify([runtimeHome, runId, sourceSequence]);
  const [, setRevision] = useState(0);
  useEffect(() => {
    if (!enabled || !runtimeHome || !runId || sourceSequence === undefined || !window.octopusTransport?.requestComposerPrediction) return;
    if (suggestions.has(key)) {
      const value = suggestions.get(key)!;
      suggestions.delete(key);
      suggestions.set(key, value);
      return;
    }
    let current = true;
    const resolve = () => {
      const result = suggestions.has(key) ? Promise.resolve(suggestions.get(key)) : requestSuggestion(key, runId);
      void result.then(() => { if (current) setRevision((value) => value + 1); });
    };
    const timer = requests.has(key) ? null : setTimeout(resolve, 500);
    if (timer === null) resolve();
    return () => { current = false; if (timer !== null) clearTimeout(timer); };
  }, [key, runtimeHome, runId, sourceSequence, enabled]);
  return enabled ? suggestions.get(key) ?? null : null;
}
