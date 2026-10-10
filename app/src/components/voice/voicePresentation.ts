import type { VoicePhase } from "./useVoiceSession";

export type VoiceTranscriptEntry = Readonly<{
  id: string;
  role: "user" | "assistant";
  text: string;
  final: boolean;
}>;

type CallPresentation = Readonly<{
  active: boolean;
  canMute: boolean;
  canEnd: boolean;
  canStart: boolean;
  title: string;
  detail: string;
}>;

const presentations: Record<VoicePhase, CallPresentation> = {
  idle: { active: false, canMute: false, canEnd: false, canStart: true, title: "Talk to Home", detail: "Start a voice call with Home." },
  requesting: { active: true, canMute: false, canEnd: true, canStart: false, title: "Opening microphone", detail: "Allow microphone access to start the call." },
  connecting: { active: true, canMute: false, canEnd: true, canStart: false, title: "Connecting call", detail: "Waiting for the voice connection." },
  listening: { active: true, canMute: true, canEnd: true, canStart: false, title: "Connected", detail: "The voice connection is active." },
  muted: { active: true, canMute: true, canEnd: true, canStart: false, title: "Microphone muted", detail: "You can still hear Home. Unmute to speak." },
  recovering: { active: true, canMute: false, canEnd: true, canStart: false, title: "Reconnecting call", detail: "Waiting for the voice connection to recover." },
  ending: { active: false, canMute: false, canEnd: false, canStart: false, title: "Closing connection…", detail: "Your microphone is off. The voice connection is still closing." },
  closure_unconfirmed: { active: false, canMute: false, canEnd: true, canStart: false, title: "Closure unconfirmed", detail: "Your microphone is off. Retry closing this call before starting another." },
  ended: { active: false, canMute: false, canEnd: false, canStart: true, title: "Call ended", detail: "Your conversation and tasks remain available." },
  error: { active: false, canMute: false, canEnd: false, canStart: true, title: "Voice unavailable", detail: "You can keep typing, or try calling again." },
};

export function voicePresentation(phase: VoicePhase): CallPresentation {
  return presentations[phase];
}

export function voiceTranscriptActivity(previous: VoiceTranscriptEntry | undefined, payload: Record<string, unknown>): boolean | null {
  if (typeof payload.text !== "string" || !payload.text.trim()) return null;
  if (payload.role === "user" && payload.final === true && !previous?.final && payload.provider_turn_complete !== false) return true;
  if (payload.role === "assistant" && previous?.text !== payload.text) return false;
  return null;
}

export function isVoiceCallActive(phase: VoicePhase): boolean {
  return presentations[phase].active;
}

export function formatCallDuration(elapsedMs: number): string {
  const seconds = Math.floor(Math.max(0, elapsedMs) / 1_000);
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  const pad = (value: number) => String(value).padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds % 60)}` : `${minutes}:${pad(seconds % 60)}`;
}
