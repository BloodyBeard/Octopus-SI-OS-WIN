import type { AirHandFrame, GestureInteraction, HandLandmark, HandObservation, HandPose, HandSide } from "./airHandEngine";
import type { AirHandGestureId, GestureSensitivity } from "./gestureShortcuts";

export const GESTURE_TRACE_FORMAT = "octopus.gesture-trace";
export const GESTURE_TRACE_VERSION = 1;

export type TraceHand = Readonly<{ label: string; score: number; image: readonly (readonly number[])[]; world: readonly (readonly number[])[] }>;

export type TraceDecision = Readonly<{
  hands: readonly Readonly<{ id: number; side: HandSide; pose: HandPose; pinch: number; aim: readonly number[]; engaged: boolean }>[];
  interactions: readonly GestureInteraction[];
  commands: readonly AirHandGestureId[];
}>;

export type TraceFrame = Readonly<{ t: number; aspect: number; hands: readonly TraceHand[]; decision: TraceDecision }>;

export type GestureTrace = Readonly<{
  format: typeof GESTURE_TRACE_FORMAT;
  version: typeof GESTURE_TRACE_VERSION;
  recordedAt: string;
  label: string;
  options: Readonly<{ mirrorX: boolean; sensitivity: GestureSensitivity }>;
  camera: Readonly<{ width: number; height: number }>;
  frames: readonly TraceFrame[];
}>;

export function traceFrame(t: number, aspect: number, observations: readonly HandObservation[], frame: AirHandFrame): TraceFrame {
  return {
    t: round(t, 2),
    aspect: round(aspect, 5),
    hands: observations.map((observation) => ({
      label: observation.label,
      score: round(observation.score, 4),
      image: observation.image.map(landmarkRow),
      world: observation.world.map(landmarkRow),
    })),
    decision: {
      hands: frame.hands.map((hand) => ({
        id: hand.id,
        side: hand.side,
        pose: hand.pose,
        pinch: round(hand.pinch, 3),
        aim: [round(hand.aim.x, 4), round(hand.aim.y, 4)],
        engaged: hand.engaged,
      })),
      interactions: frame.interactions,
      commands: frame.commands,
    },
  };
}

export function traceObservations(frame: TraceFrame): HandObservation[] {
  return frame.hands.map((hand) => ({
    label: hand.label,
    score: hand.score,
    image: hand.image.map(landmarkPoint),
    world: hand.world.map(landmarkPoint),
  }));
}

export function parseGestureTrace(value: unknown): GestureTrace {
  const trace = value as Partial<GestureTrace> | null;
  if (!trace || typeof trace !== "object") throw new Error("Gesture trace is not an object.");
  if (trace.format !== GESTURE_TRACE_FORMAT || trace.version !== GESTURE_TRACE_VERSION) {
    throw new Error(`Gesture trace format ${String(trace.format)} v${String(trace.version)} is not ${GESTURE_TRACE_FORMAT} v${GESTURE_TRACE_VERSION}.`);
  }
  const options = trace.options;
  if (!options || typeof options.mirrorX !== "boolean" || !["responsive", "balanced", "deliberate"].includes(String(options.sensitivity))) {
    throw new Error("Gesture trace options are missing or invalid.");
  }
  if (!Array.isArray(trace.frames)) throw new Error("Gesture trace has no frames.");
  trace.frames.forEach((frame, index) => {
    if (!frame || !Number.isFinite(frame.t) || !Number.isFinite(frame.aspect) || !Array.isArray(frame.hands) || !frame.decision) {
      throw new Error(`Gesture trace frame ${index} is malformed.`);
    }
    for (const hand of frame.hands) {
      const rows = [...(hand?.image ?? []), ...(hand?.world ?? [])];
      if (hand?.image?.length !== 21 || hand?.world?.length !== 21 || !rows.every((row) => Array.isArray(row) && row.length === 3 && row.every(Number.isFinite))) {
        throw new Error(`Gesture trace frame ${index} has a hand without 21 image and 21 world landmarks.`);
      }
    }
  });
  return trace as GestureTrace;
}

function landmarkRow(landmark: HandLandmark): number[] {
  return [round(landmark.x, 6), round(landmark.y, 6), round(landmark.z, 6)];
}

function landmarkPoint(row: readonly number[]): HandLandmark {
  return { x: row[0] ?? Number.NaN, y: row[1] ?? Number.NaN, z: row[2] ?? Number.NaN };
}

function round(value: number, digits: number): number {
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}
