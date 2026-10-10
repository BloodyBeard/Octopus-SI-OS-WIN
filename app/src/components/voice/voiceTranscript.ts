import type { VoiceTranscriptEntry } from "./voicePresentation";

export function createVoiceTranscript() {
  let entries: readonly VoiceTranscriptEntry[] = [];
  return {
    artifact(payload: Record<string, unknown>): readonly VoiceTranscriptEntry[] {
      const { role, text, id, final } = payload;
      if ((role !== "user" && role !== "assistant") || typeof text !== "string" || typeof id !== "string" || !id || typeof final !== "boolean") {
        throw new Error("Voice transcript artifact has no valid role, text, entry identity or completion state.");
      }
      const previous = entries.find((entry) => entry.id === id);
      if (previous && (previous.role !== role || (previous.final && (previous.text !== text || !final)))) {
        throw new Error("Voice transcript changed an already completed entry.");
      }
      if (previous?.text === text && previous.final === final) return entries;
      const entry: VoiceTranscriptEntry = { id, role, text, final };
      entries = previous ? entries.map((item) => item.id === id ? entry : item) : [...entries, entry];
      return entries;
    },
  };
}
