export type VoiceSpatialCorner = "north-west" | "north-east" | "south-east" | "south-west";

export type VoiceSpatialInteraction = Readonly<{
  clientX: number;
  clientY: number;
  corner?: VoiceSpatialCorner;
  documentId: string;
  kind?: "drag" | "resize" | "transform";
  phase: "begin" | "move" | "end";
  scale?: number;
}>;

const VOICE_SPATIAL_INTERACTION_EVENT = "octopus:voice-spatial-interaction";

export function emitVoiceSpatialInteraction(interaction: VoiceSpatialInteraction): void {
  window.dispatchEvent(new CustomEvent(VOICE_SPATIAL_INTERACTION_EVENT, { detail: interaction }));
}

export function subscribeVoiceSpatialInteraction(
  documentId: string,
  listener: (interaction: VoiceSpatialInteraction) => void,
): () => void {
  const handle = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const detail = event.detail as VoiceSpatialInteraction | undefined;
    if (!detail || detail.documentId !== documentId) return;
    if (detail.phase !== "begin" && detail.phase !== "move" && detail.phase !== "end") return;
    listener(detail);
  };
  window.addEventListener(VOICE_SPATIAL_INTERACTION_EVENT, handle);
  return () => window.removeEventListener(VOICE_SPATIAL_INTERACTION_EVENT, handle);
}
