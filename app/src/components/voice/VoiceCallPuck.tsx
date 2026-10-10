import {
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type PointerEvent as ReactPointerEvent,
} from "react";
import "../icon-glyphs/voice";
import type { OctopusClientAccessProfile } from "../../electron";
import { Glyph } from "../IconGlyphs";
import type { VoiceSession } from "./useVoiceSession";
import { formatCallDuration, voicePresentation } from "./voicePresentation";
import "./voice.css";

const PUCK_STORAGE_KEY = "octopus.voice-call-puck.v1";
const PUCK_MARGIN = 8;
const DRAG_THRESHOLD = 4;
const VU_BAR_COUNT = 11;
const LEVEL_FLOOR = 0.012;

type PuckPosition = Readonly<{ left: number; top: number }>;

type DragState = {
  pointerId: number;
  startX: number;
  startY: number;
  originLeft: number;
  originTop: number;
  moved: boolean;
};

export function VoiceCallPuck({
  accessProfile,
  onOpenHome,
  session,
}: {
  accessProfile: OctopusClientAccessProfile;
  onOpenHome: () => void;
  session: VoiceSession;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<DragState | null>(null);
  const suppressClickRef = useRef(false);
  const [position, setPosition] = useState<PuckPosition | null>(() => readStoredPosition(accessProfile));
  const [dragging, setDragging] = useState(false);
  const [nowMs, setNowMs] = useState(() => performance.now());

  useEffect(() => {
    const timer = window.setInterval(() => setNowMs(performance.now()), 1_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const clampToViewport = () => {
      setPosition((current) => {
        if (!current) return current;
        const clamped = clampPosition(current, rootRef.current);
        return clamped.left === current.left && clamped.top === current.top ? current : clamped;
      });
    };
    window.addEventListener("resize", clampToViewport);
    return () => window.removeEventListener("resize", clampToViewport);
  }, []);

  const beginDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 && event.pointerType === "mouse") return;
    if (event.target instanceof Element && event.target.closest("button")) return;
    const root = rootRef.current;
    if (!root) return;
    const rect = root.getBoundingClientRect();
    dragRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      originLeft: rect.left,
      originTop: rect.top,
      moved: false,
    };
  };

  const moveDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    const deltaX = event.clientX - drag.startX;
    const deltaY = event.clientY - drag.startY;
    if (!drag.moved) {
      if (Math.abs(deltaX) < DRAG_THRESHOLD && Math.abs(deltaY) < DRAG_THRESHOLD) return;
      drag.moved = true;
      setDragging(true);
      rootRef.current?.setPointerCapture(event.pointerId);
    }
    setPosition(clampPosition(
      { left: drag.originLeft + deltaX, top: drag.originTop + deltaY },
      rootRef.current,
    ));
  };

  const endDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    dragRef.current = null;
    setDragging(false);
    if (!drag.moved) return;
    suppressClickRef.current = true;
    setPosition((current) => {
      if (current) writeStoredPosition(accessProfile, current);
      return current;
    });
  };

  const openFromPanel = (event: { target: EventTarget | null }) => {
    if (suppressClickRef.current) {
      suppressClickRef.current = false;
      return;
    }
    if (event.target instanceof Element && event.target.closest("button")) return;
    onOpenHome();
  };

  const muted = session.phase === "muted";
  const state = voicePresentation(session.phase);
  const elapsedMs = session.startedAtMs > 0 ? Math.max(0, nowMs - session.startedAtMs) : 0;

  return (
    <div
      aria-label={`Home call: ${state.title}`}
      className={`voice-call-puck${dragging ? " is-dragging" : ""}${muted ? " is-muted" : ""}`}
      onClick={openFromPanel}
      onPointerCancel={endDrag}
      onPointerDown={beginDrag}
      onPointerMove={moveDrag}
      onPointerUp={endDrag}
      ref={rootRef}
      role="group"
      style={position ? { left: position.left, top: position.top } : undefined}
      title="Open Home"
    >
      <VoiceCallWaveform
        accessProfile={accessProfile}
        muted={muted}
        session={session}
      />
      <button aria-label="Open Home call" className="voice-call-puck-copy" onClick={onOpenHome} type="button">
        <span className="voice-call-puck-timer">{session.startedAtMs > 0 ? formatCallDuration(elapsedMs) : "–:––"}</span>
        <span className="voice-call-puck-status" role="status">{state.title}</span>
      </button>
      <button
        aria-label={muted ? "Unmute" : "Mute"}
        aria-pressed={muted}
        className="voice-call-puck-action"
        disabled={!state.canMute}
        onClick={session.toggleMute}
        title={muted ? "Unmute" : "Mute"}
        type="button"
      >
        <Glyph name={muted ? "microphoneOff" : "microphone"} size={18} />
      </button>
      <button
        aria-label="End call"
        className="voice-call-puck-action is-end"
        disabled={!state.canEnd}
        onClick={() => { void session.end(); }}
        title="End call; tasks keep running"
        type="button"
      >
        <Glyph name="phoneEnd" size={18} />
      </button>
    </div>
  );
}

function VoiceCallWaveform({
  accessProfile,
  muted,
  session,
}: {
  accessProfile: OctopusClientAccessProfile;
  muted: boolean;
  session: VoiceSession;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const inputMeter = useSyncExternalStore(session.subscribeMeter, session.getMeterSnapshot, session.getMeterSnapshot);
  const outputMeter = useSyncExternalStore(
    session.subscribeOutputMeter,
    session.getOutputMeterSnapshot,
    session.getOutputMeterSnapshot,
  );
  const levelsRef = useRef({ input: 0, output: 0 });
  levelsRef.current = accessProfile === "lab"
    ? simulatedLevels(performance.now())
    : {
      input: muted ? 0 : clampUnit(inputMeter.level),
      output: clampUnit(outputMeter.level),
    };

  useEffect(() => {
    let frame = 0;
    const heights = new Array<number>(VU_BAR_COUNT).fill(0);

    const draw = (now: number) => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      if (!width || !height) return;
      const ratio = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
      const drawing = canvas.getContext("2d");
      if (!drawing) return;
      const style = getComputedStyle(canvas);
      const userColor = style.color || "#fff";
      const assistantColor = style.getPropertyValue("--voice-call-puck-assistant").trim()
        || "rgb(255 255 255 / 62%)";
      const level = levelsRef.current;
      const assistant = level.output > level.input;
      const active = Math.max(level.input, level.output);
      const seconds = now / 1000;

      drawing.clearRect(0, 0, canvas.width, canvas.height);
      const center = canvas.height * 0.5;
      const pitch = canvas.width / VU_BAR_COUNT;
      const barWidth = Math.max(2 * ratio, pitch * 0.52);
      const dotHeight = Math.max(2.6 * ratio, barWidth);
      const middle = (VU_BAR_COUNT - 1) / 2;
      drawing.fillStyle = active > LEVEL_FLOOR && assistant ? assistantColor : userColor;
      for (let index = 0; index < VU_BAR_COUNT; index += 1) {
        const centerWeight = 0.42 + 0.58 * Math.cos(((index - middle) / (middle + 1)) * Math.PI * 0.5);
        const jitter = 0.55
          + Math.sin(seconds * (6.4 + index * 0.83) + index * 1.7) * 0.25
          + Math.sin(seconds * (11.2 - index * 0.41) + index * 0.6) * 0.2;
        const target = Math.min(1, active * 3.2) * centerWeight * Math.max(0.18, jitter);
        const current = heights[index] ?? 0;
        const eased = current + (target - current) * (target > current ? 0.5 : 0.22);
        heights[index] = eased;
        const amplitude = Math.max(dotHeight * 0.5, eased * center * 0.94);
        const x = index * pitch + (pitch - barWidth) * 0.5;
        drawing.globalAlpha = active <= LEVEL_FLOOR ? 0.45 : 1;
        roundedBar(drawing, x, center - amplitude, barWidth, amplitude * 2);
      }
      drawing.globalAlpha = 1;
    };

    const tick = (now: number) => {
      draw(now);
      frame = window.requestAnimationFrame(tick);
    };

    frame = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(frame);
  }, [accessProfile]);

  return <canvas aria-hidden className="voice-call-puck-waveform" ref={canvasRef} />;
}

function roundedBar(
  drawing: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
): void {
  const radius = Math.min(width * 0.5, height * 0.5);
  drawing.beginPath();
  if (typeof drawing.roundRect === "function") {
    drawing.roundRect(x, y, width, height, radius);
  } else {
    drawing.rect(x, y, width, height);
  }
  drawing.fill();
}

function simulatedLevels(nowMs: number): { input: number; output: number } {
  const t = nowMs / 1000;
  const cycle = t % 9;
  const syllables = (phase: number) =>
    Math.max(0, 0.3 + Math.sin(t * 7.3 + phase) * 0.24 + Math.sin(t * 13.1 + phase * 1.7) * 0.16)
    * (Math.sin(t * 3.1 + phase) > -0.35 ? 1 : 0.12);
  const userWindow = cycle > 0.4 && cycle < 3.1 ? 1 : 0;
  const assistantWindow = cycle > 3.9 && cycle < 7.9 ? 1 : 0;
  return {
    input: clampUnit(syllables(0.4) * userWindow),
    output: clampUnit(syllables(2.1) * assistantWindow),
  };
}

function clampUnit(value: number): number {
  return Math.max(0, Math.min(1, value));
}

function clampPosition(position: PuckPosition, root: HTMLElement | null): PuckPosition {
  const width = root?.offsetWidth ?? 232;
  const height = root?.offsetHeight ?? 56;
  return {
    left: Math.min(Math.max(position.left, PUCK_MARGIN), Math.max(PUCK_MARGIN, window.innerWidth - width - PUCK_MARGIN)),
    top: Math.min(Math.max(position.top, PUCK_MARGIN), Math.max(PUCK_MARGIN, window.innerHeight - height - PUCK_MARGIN)),
  };
}

function storageKey(profile: OctopusClientAccessProfile): string {
  return profile === "live" ? PUCK_STORAGE_KEY : `${PUCK_STORAGE_KEY}.test`;
}

function readStoredPosition(profile: OctopusClientAccessProfile): PuckPosition | null {
  try {
    const raw = window.localStorage.getItem(storageKey(profile));
    if (!raw) return null;
    const value = JSON.parse(raw) as Partial<PuckPosition>;
    if (typeof value.left !== "number" || typeof value.top !== "number") return null;
    return clampPosition({ left: value.left, top: value.top }, null);
  } catch {
    return null;
  }
}

function writeStoredPosition(profile: OctopusClientAccessProfile, position: PuckPosition): void {
  try {
    window.localStorage.setItem(storageKey(profile), JSON.stringify(position));
  } catch {
    void 0;
  }
}
