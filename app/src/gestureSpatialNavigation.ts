export type SpatialDirection = "left" | "right" | "up" | "down";

export type SpatialRect = Readonly<{ left: number; top: number; right: number; bottom: number }>;

const BEAM_PENALTY = 2.2;
const CENTER_PENALTY = 0.12;
const OVERLAP_SCALE = 0.5;
const CONE_SLOPE = 3.5;

type Axis = {
  leading: (rect: SpatialRect) => number;
  trailing: (rect: SpatialRect) => number;
  center: (rect: SpatialRect) => number;
  crossStart: (rect: SpatialRect) => number;
  crossEnd: (rect: SpatialRect) => number;
  crossCenter: (rect: SpatialRect) => number;
  sign: 1 | -1;
};

const horizontal = {
  center: (rect: SpatialRect) => (rect.left + rect.right) / 2,
  crossStart: (rect: SpatialRect) => rect.top,
  crossEnd: (rect: SpatialRect) => rect.bottom,
  crossCenter: (rect: SpatialRect) => (rect.top + rect.bottom) / 2,
};

const vertical = {
  center: (rect: SpatialRect) => (rect.top + rect.bottom) / 2,
  crossStart: (rect: SpatialRect) => rect.left,
  crossEnd: (rect: SpatialRect) => rect.right,
  crossCenter: (rect: SpatialRect) => (rect.left + rect.right) / 2,
};

const AXES: Record<SpatialDirection, Axis> = {
  right: { ...horizontal, leading: (rect) => rect.right, trailing: (rect) => rect.left, sign: 1 },
  left: { ...horizontal, leading: (rect) => rect.left, trailing: (rect) => rect.right, sign: -1 },
  down: { ...vertical, leading: (rect) => rect.bottom, trailing: (rect) => rect.top, sign: 1 },
  up: { ...vertical, leading: (rect) => rect.top, trailing: (rect) => rect.bottom, sign: -1 },
};

export function rankSpatialCandidates(
  origin: SpatialRect,
  direction: SpatialDirection,
  candidates: readonly SpatialRect[],
): number[] {
  const axis = AXES[direction];
  const originCenter = axis.center(origin);
  const originCross = axis.crossCenter(origin);
  const originEdge = axis.leading(origin);
  const scored: { index: number; score: number }[] = [];
  candidates.forEach((candidate, index) => {
    const ahead = (axis.center(candidate) - originCenter) * axis.sign;
    if (ahead <= 1) return;
    const gap = (axis.trailing(candidate) - originEdge) * axis.sign;
    const primary = gap >= 0 ? gap : ahead * OVERLAP_SCALE;
    const beamGap = Math.max(0, axis.crossStart(candidate) - axis.crossEnd(origin), axis.crossStart(origin) - axis.crossEnd(candidate));
    if (beamGap > 0 && beamGap > ahead * CONE_SLOPE) return;
    const centerOffset = Math.abs(axis.crossCenter(candidate) - originCross);
    scored.push({ index, score: primary + beamGap * BEAM_PENALTY + centerOffset * CENTER_PENALTY });
  });
  return scored.sort((left, right) => left.score - right.score || left.index - right.index).map((entry) => entry.index);
}

export function nearestSpatialCandidate(point: Readonly<{ x: number; y: number }>, candidates: readonly SpatialRect[]): number {
  let best = -1;
  let bestDistance = Number.POSITIVE_INFINITY;
  let bestArea = Number.POSITIVE_INFINITY;
  candidates.forEach((candidate, index) => {
    const distance = rectDistance(point, candidate);
    const area = rectArea(candidate);
    if (distance < bestDistance || (distance === bestDistance && area < bestArea)) {
      best = index;
      bestDistance = distance;
      bestArea = area;
    }
  });
  return best;
}

export function readingOrder(candidates: readonly SpatialRect[]): number[] {
  return candidates
    .map((rect, index) => ({ index, rect }))
    .sort((left, right) => {
      const rowDelta = left.rect.top - right.rect.top;
      if (Math.abs(rowDelta) > Math.min(left.rect.bottom - left.rect.top, right.rect.bottom - right.rect.top) / 2) return rowDelta;
      return left.rect.left - right.rect.left || left.index - right.index;
    })
    .map((entry) => entry.index);
}

export type AimState = Readonly<{ current: number; candidate: number; since: number }>;

export type AimOptions = Readonly<{ dwellMs: number; marginMin: number; marginMax: number; marginFraction: number }>;

export const IDLE_AIM: AimState = Object.freeze({ current: -1, candidate: -1, since: 0 });

export function trackAim(
  state: AimState,
  point: Readonly<{ x: number; y: number }>,
  rects: readonly SpatialRect[],
  now: number,
  options: AimOptions,
): AimState {
  const best = nearestSpatialCandidate(point, rects);
  const current = state.current >= 0 && state.current < rects.length ? state.current : -1;
  if (best < 0) return { current, candidate: -1, since: now };
  if (current < 0) return { current: best, candidate: -1, since: now };
  if (best === current) return state.candidate < 0 ? state : { current, candidate: -1, since: now };
  const bestRect = rects[best] as SpatialRect;
  const currentRect = rects[current] as SpatialRect;
  const bestDistance = rectDistance(point, bestRect);
  const currentDistance = rectDistance(point, currentRect);
  const margin = Math.min(options.marginMax, Math.max(options.marginMin, Math.min(bestRect.right - bestRect.left, bestRect.bottom - bestRect.top) * options.marginFraction));
  const nested = bestDistance === 0 && currentDistance === 0 && rectArea(bestRect) < rectArea(currentRect) * 0.85;
  if (!nested && bestDistance + margin > currentDistance) return state.candidate < 0 ? state : { current, candidate: -1, since: now };
  if (state.candidate !== best) return { current, candidate: best, since: now };
  if (now - state.since < options.dwellMs) return state;
  return { current: best, candidate: -1, since: now };
}

function rectDistance(point: Readonly<{ x: number; y: number }>, rect: SpatialRect): number {
  return Math.hypot(Math.max(rect.left - point.x, 0, point.x - rect.right), Math.max(rect.top - point.y, 0, point.y - rect.bottom));
}

function rectArea(rect: SpatialRect): number {
  return (rect.right - rect.left) * (rect.bottom - rect.top);
}
