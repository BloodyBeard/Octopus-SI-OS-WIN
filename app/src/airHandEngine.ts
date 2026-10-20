import type { NormalizedLandmark } from "@mediapipe/tasks-vision";
import type { AirHandGestureId, GestureSensitivity } from "./gestureShortcuts";

export type AirHandPose = "none" | "aim" | "open_palm" | "left_click" | "fist" | "mission_control" | "pinch";

export type AirHandPoint = Readonly<{ x: number; y: number }>;

export type FingerCurls = { thumb: number; index: number; middle: number; ring: number; little: number };

export type AirHandEngineResult = {
  event: AirHandGestureId | null;
  point: AirHandPoint | null;
  palmPoint: AirHandPoint | null;
  pinchRatio: number | null;
  curls: FingerCurls | null;
  pose: AirHandPose;
};

type Point = { x: number; y: number };
type Classification = { curls: FingerCurls; pose: AirHandPose };

export class AirHandGestureEngine {
  private candidate: AirHandPose = "none";
  private candidateSince = 0;
  private fired = false;
  private swipe = new SwipeDetector();
  private pointerGraceUntil = 0;
  private pinchStartSpread: number | null = null;
  private clickIntent = new ClickIntentDetector();
  private palmStabilizer = new PalmPointStabilizer();
  private holdAnchor: Point | null = null;

  reset(): void {
    this.resetPose();
    this.swipe.reset();
  }

  lost(now: number, options: { mirrorX: boolean; sensitivity: GestureSensitivity }): AirHandGestureId | null {
    this.resetPose();
    return swipeEvent(this.swipe.lost(now, swipeThreshold(options.sensitivity), swipeSpeed(options.sensitivity)), options.mirrorX);
  }

  private resetPose(): void {
    this.candidate = "none";
    this.candidateSince = 0;
    this.fired = false;
    this.pointerGraceUntil = 0;
    this.pinchStartSpread = null;
    this.clickIntent.reset();
    this.palmStabilizer.reset();
    this.holdAnchor = null;
  }

  process(
    landmarks: NormalizedLandmark[],
    now: number,
    options: { mirrorX: boolean; sensitivity: GestureSensitivity },
  ): AirHandEngineResult {
    const classification = classifyLandmarks(landmarks, this.candidate);
    if (!classification) {
      this.reset();
      return { event: null, point: null, palmPoint: null, pinchRatio: null, curls: null, pose: "none" };
    }
    const curls = classification.curls;
    const controlPoint = handControlPoint(landmarks);
    const stabilizedPalm = controlPoint ? this.palmStabilizer.update(controlPoint, now) : null;
    const palmPoint = stabilizedPalm
      ? {
        x: clamp(options.mirrorX ? 1 - stabilizedPalm.x : stabilizedPalm.x),
        y: clamp(stabilizedPalm.y),
      }
      : null;

    const geometry = pinchSpread(landmarks);
    if (this.pinchStartSpread != null) {
      const supportHeld = classification.curls.middle >= 0.5
        && classification.curls.ring >= 0.5
        && classification.curls.little >= 0.5;
      if (supportHeld && geometry) {
        if (this.candidate !== "pinch") {
          this.candidate = "pinch";
          this.candidateSince = now;
          this.fired = false;
        }
        const ratio = Math.min(3, Math.max(0.35, geometry.spread / this.pinchStartSpread));
        this.swipe.reset();
        return { event: null, point: null, palmPoint, pinchRatio: ratio, curls, pose: "pinch" };
      }
      this.pinchStartSpread = null;
    } else if (
      geometry
      && classification.curls.middle >= 0.56
      && classification.curls.ring >= 0.56
      && classification.curls.little >= 0.56
      && classification.curls.index < 0.6
      && classification.curls.thumb < 0.66
      && geometry.spread < geometry.palmWidth * 0.45
    ) {
      this.pinchStartSpread = Math.max(geometry.spread, geometry.palmWidth * 0.14);
      this.candidate = "pinch";
      this.candidateSince = now;
      this.fired = false;
      this.swipe.reset();
      return { event: null, point: null, palmPoint, pinchRatio: 1, curls, pose: "pinch" };
    }

    let classified = classification.pose;
    let coasting = false;
    if (classified === "aim" || classified === "left_click") {
      this.pointerGraceUntil = now + 300;
    } else if (classified === "none" && now < this.pointerGraceUntil) {
      classified = "aim";
      coasting = true;
    }
    if (!coasting && (classified === "aim" || classified === "left_click")) {
      classified = this.clickIntent.update(classification.curls.index) ? "left_click" : "aim";
    } else if (!coasting) {
      this.clickIntent.reset();
    }
    const pointerPoint = stabilizedPalm && (classified === "aim" || classified === "left_click")
      ? palmPoint
      : null;
    if (classified !== this.candidate) {
      this.candidate = classified;
      this.candidateSince = now;
      this.fired = false;
      this.holdAnchor = controlPoint;
    }

    const swipeEligible = classified === "open_palm" || classified === "mission_control" || classified === "none";
    const palmWidth = geometry?.palmWidth ?? 0;
    const direction = controlPoint
      ? this.swipe.update(controlPoint, now, swipeEligible, Math.min(swipeThreshold(options.sensitivity), Math.max(SWIPE_MIN_TRAVEL, palmWidth * SWIPE_HAND_WIDTHS)), swipeSpeed(options.sensitivity))
      : 0;
    const swiped = swipeEvent(direction, options.mirrorX);
    if (swiped) {
      this.fired = true;
      return { event: swiped, point: null, palmPoint, pinchRatio: null, curls, pose: classified };
    }

    const event = shortcutEventForPose(classified);
    if (event && STILL_POSES.has(classified) && controlPoint && this.holdAnchor && distance(controlPoint, this.holdAnchor) > HOLD_STILL_TRAVEL) {
      this.holdAnchor = controlPoint;
      this.candidateSince = now;
    }
    if (!event || this.fired || now - this.candidateSince < holdDuration(options.sensitivity)) {
      return { event: null, point: pointerPoint, palmPoint, pinchRatio: null, curls, pose: classified };
    }
    this.fired = true;
    return { event, point: pointerPoint, palmPoint, pinchRatio: null, curls, pose: classified };
  }
}

function classifyLandmarks(landmarks: NormalizedLandmark[], previous: AirHandPose): Classification | null {
  if (landmarks.length < 21) return null;
  const wrist = point(landmarks, 0);
  const indexMcp = point(landmarks, 5);
  const middleMcp = point(landmarks, 9);
  const littleMcp = point(landmarks, 17);
  const center = palmCenter(landmarks);
  if (!wrist || !indexMcp || !middleMcp || !littleMcp || !center) return null;
  const palmAxis = normalized(subtract(middleMcp, wrist));
  const palmWidth = Math.max(distance(indexMcp, littleMcp), 0.04);
  const curls: FingerCurls = {
    thumb: thumbClosedScore(landmarks, center, palmWidth),
    index: curlScore(landmarks, 5, 6, 8, palmAxis),
    middle: curlScore(landmarks, 9, 10, 12, palmAxis),
    ring: curlScore(landmarks, 13, 14, 16, palmAxis),
    little: curlScore(landmarks, 17, 18, 20, palmAxis),
  };
  const topCurls = [curls.index, curls.middle, curls.ring, curls.little];
  const topOpen = topCurls.filter((curl) => curl < 0.44).length;
  const inPointerPose = previous === "aim" || previous === "left_click";
  const thumbOpenLimit = inPointerPose ? 0.75 : 0.62;
  const supportThreshold = inPointerPose ? 0.45 : 0.56;
  const thumbOpen = curls.thumb < thumbOpenLimit;
  if (topOpen === 4 && curls.thumb < 0.62) return { curls, pose: "open_palm" };
  if (curls.index < 0.44 && curls.middle < 0.44 && curls.ring >= 0.56 && curls.little >= 0.56) {
    return { curls, pose: "mission_control" };
  }
  const supportClosed = curls.middle >= supportThreshold
    && curls.ring >= supportThreshold
    && curls.little >= supportThreshold;
  if (!supportClosed) return { curls, pose: "none" };
  if (curls.index >= 0.62) return { curls, pose: thumbOpen ? "left_click" : "fist" };
  return { curls, pose: thumbOpen ? "aim" : "none" };
}

const STILL_POSES = new Set<AirHandPose>(["open_palm", "fist"]);
const HOLD_STILL_TRAVEL = 0.035;
const SWIPE_WINDOW_MS = 480;
const SWIPE_STALE_MS = 250;
const SWIPE_POSE_GRACE_MS = 200;
const SWIPE_LOST_GRACE_MS = 220;
const SWIPE_EXIT_FRACTION = 0.6;
const SWIPE_MIN_TRAVEL = 0.05;
const SWIPE_HAND_WIDTHS = 1.8;
const SWIPE_DOMINANCE = 1.4;
const SWIPE_STRAIGHTNESS = 0.7;
const SWIPE_SAME_DIRECTION_MS = 380;
const SWIPE_RETURN_SUPPRESS_MS = 750;

type SwipeSample = { at: number; x: number; y: number };

class SwipeDetector {
  private samples: SwipeSample[] = [];
  private ineligibleSince: number | null = null;
  private lostAt: number | null = null;
  private lastFired: { at: number; direction: -1 | 1 } | null = null;
  private threshold = 0;

  reset(): void {
    this.samples = [];
    this.ineligibleSince = null;
    this.lostAt = null;
  }

  update(position: Point, now: number, eligible: boolean, threshold: number, speed: number): -1 | 0 | 1 {
    this.lostAt = null;
    this.threshold = threshold;
    if (!eligible) {
      this.ineligibleSince ??= now;
      if (now - this.ineligibleSince > SWIPE_POSE_GRACE_MS) this.samples = [];
      return 0;
    }
    this.ineligibleSince = null;
    const latest = this.samples[this.samples.length - 1];
    if (latest && now - latest.at > SWIPE_STALE_MS) this.samples = [];
    this.samples.push({ at: now, x: position.x, y: position.y });
    while (this.samples.length > 2 && now - this.samples[0].at > SWIPE_WINDOW_MS) this.samples.shift();
    return this.evaluate(now, threshold, speed);
  }

  lost(now: number, threshold: number, speed: number): -1 | 0 | 1 {
    if (this.samples.length === 0) return 0;
    if (this.lostAt == null) {
      this.lostAt = now;
      return this.evaluate(now, Math.min(threshold, this.threshold || threshold) * SWIPE_EXIT_FRACTION, speed);
    }
    if (now - this.lostAt > SWIPE_LOST_GRACE_MS) this.samples = [];
    return 0;
  }

  private evaluate(now: number, threshold: number, speed: number): -1 | 0 | 1 {
    const last = this.samples[this.samples.length - 1];
    if (!last || this.samples.length < 2) return 0;
    for (let index = 0; index < this.samples.length - 1; index += 1) {
      const start = this.samples[index];
      const dx = last.x - start.x;
      const dy = last.y - start.y;
      if (Math.abs(dx) < threshold || Math.abs(dx) < Math.abs(dy) * SWIPE_DOMINANCE) continue;
      const elapsed = Math.max((last.at - start.at) / 1000, 1 / 60);
      if (Math.abs(dx) / elapsed < speed) continue;
      let path = 0;
      for (let step = index + 1; step < this.samples.length; step += 1) path += Math.abs(this.samples[step].x - this.samples[step - 1].x);
      if (Math.abs(dx) < path * SWIPE_STRAIGHTNESS) continue;
      const direction: -1 | 1 = dx > 0 ? 1 : -1;
      this.samples = [last];
      const previous = this.lastFired;
      if (previous) {
        const since = now - previous.at;
        if (since < (previous.direction === direction ? SWIPE_SAME_DIRECTION_MS : SWIPE_RETURN_SUPPRESS_MS)) return 0;
      }
      this.lastFired = { at: now, direction };
      return direction;
    }
    return 0;
  }
}

function swipeEvent(direction: -1 | 0 | 1, mirrorX: boolean): AirHandGestureId | null {
  if (direction === 0) return null;
  return (mirrorX ? direction < 0 : direction > 0) ? "swipe_right" : "swipe_left";
}

class ClickIntentDetector {
  private baseline = 0.25;
  private clicking = false;
  private primed = false;

  reset(): void {
    this.baseline = 0.25;
    this.clicking = false;
    this.primed = false;
  }

  update(indexCurl: number): boolean {
    if (!this.primed) {
      this.baseline = indexCurl;
      this.primed = true;
    }
    const above = indexCurl - this.baseline;
    if (this.clicking) {
      if (above < 0.14) this.clicking = false;
    } else if (above >= 0.24 && indexCurl >= 0.55) {
      this.clicking = true;
    }
    if (!this.clicking) {
      this.baseline += (indexCurl - this.baseline) * 0.22;
      this.baseline = Math.min(this.baseline, 0.55);
    }
    return this.clicking;
  }
}

class PalmPointStabilizer {
  private lastRaw: Point | null = null;
  private lastUpdateAt: number | null = null;
  private jumpCandidate: Point | null = null;
  private jumpCandidateCount = 0;
  private output: Point | null = null;
  private xFilter = new OneEuroFilter(2.2, 1.2);
  private yFilter = new OneEuroFilter(2.2, 1.2);

  reset(): void {
    this.lastRaw = null;
    this.lastUpdateAt = null;
    this.jumpCandidate = null;
    this.jumpCandidateCount = 0;
    this.output = null;
    this.xFilter.reset();
    this.yFilter.reset();
  }

  update(raw: Point, now: number): Point {
    if (!this.output || !this.lastRaw) {
      this.output = raw;
      this.lastRaw = raw;
      this.lastUpdateAt = now;
      this.xFilter.prime(raw.x);
      this.yFilter.prime(raw.y);
      return raw;
    }
    const previous = this.output;
    const dt = Math.min(Math.max((now - (this.lastUpdateAt ?? now)) / 1000, 1 / 120), 1 / 15);
    this.lastUpdateAt = now;
    const rawDelta = distance(raw, this.lastRaw);
    const distanceFromOutput = distance(raw, previous);
    const rawJumpThreshold = Math.max(dt * 2.8, 0.14);
    if (this.jumpCandidate && distance(this.jumpCandidate, raw) < 0.045 && distanceFromOutput > 0.11) {
      this.jumpCandidateCount += 1;
      this.lastRaw = raw;
      if (this.jumpCandidateCount < 4) return previous;
      this.jumpCandidate = null;
      this.jumpCandidateCount = 0;
      this.xFilter.prime(raw.x);
      this.yFilter.prime(raw.y);
      this.output = raw;
      return raw;
    }
    if (rawDelta > rawJumpThreshold && distanceFromOutput > 0.11) {
      this.jumpCandidate = raw;
      this.jumpCandidateCount = 1;
      this.lastRaw = raw;
      return previous;
    }
    this.jumpCandidate = null;
    this.jumpCandidateCount = 0;
    this.lastRaw = raw;
    this.output = {
      x: this.xFilter.filter(raw.x, dt),
      y: this.yFilter.filter(raw.y, dt),
    };
    return this.output;
  }
}

class OneEuroFilter {
  private initialized = false;
  private filteredValue = 0;
  private filteredDerivative = 0;

  constructor(private readonly minCutoff: number, private readonly beta: number, private readonly derivativeCutoff = 1) {}

  reset(): void {
    this.initialized = false;
    this.filteredValue = 0;
    this.filteredDerivative = 0;
  }

  prime(value: number): void {
    this.initialized = true;
    this.filteredValue = value;
    this.filteredDerivative = 0;
  }

  filter(value: number, dt: number): number {
    const elapsed = Math.max(dt, 1 / 1000);
    if (!this.initialized) {
      this.prime(value);
      return value;
    }
    const derivative = (value - this.filteredValue) / elapsed;
    this.filteredDerivative = lowPass(derivative, this.filteredDerivative, this.derivativeCutoff, elapsed);
    const cutoff = this.minCutoff + this.beta * Math.abs(this.filteredDerivative);
    this.filteredValue = lowPass(value, this.filteredValue, cutoff, elapsed);
    return this.filteredValue;
  }
}

function lowPass(value: number, previous: number, cutoff: number, dt: number): number {
  const tau = 1 / (2 * Math.PI * Math.max(cutoff, 0.0001));
  const alpha = 1 / (1 + tau / dt);
  return previous + alpha * (value - previous);
}

function handControlPoint(landmarks: NormalizedLandmark[]): Point | null {
  const wrist = point(landmarks, 0);
  const indexMcp = point(landmarks, 5);
  const middleMcp = point(landmarks, 9);
  const ringMcp = point(landmarks, 13);
  const littleMcp = point(landmarks, 17);
  if (!wrist || !indexMcp || !middleMcp || !ringMcp || !littleMcp) return null;
  const knuckleCenter = {
    x: (indexMcp.x + middleMcp.x + ringMcp.x + littleMcp.x) / 4,
    y: (indexMcp.y + middleMcp.y + ringMcp.y + littleMcp.y) / 4,
  };
  const axisPoint = {
    x: wrist.x + (middleMcp.x - wrist.x) * 0.62,
    y: wrist.y + (middleMcp.y - wrist.y) * 0.62,
  };
  const palmControlPoint = {
    x: axisPoint.x * 0.72 + knuckleCenter.x * 0.28,
    y: axisPoint.y * 0.72 + knuckleCenter.y * 0.28,
  };
  return {
    x: indexMcp.x * 0.45 + palmControlPoint.x * 0.55,
    y: indexMcp.y * 0.45 + palmControlPoint.y * 0.55,
  };
}

function curlScore(
  landmarks: NormalizedLandmark[],
  mcpIndex: number,
  pipIndex: number,
  tipIndex: number,
  palmAxis: Point,
): number {
  const mcp = point(landmarks, mcpIndex);
  const pip = point(landmarks, pipIndex);
  const tip = point(landmarks, tipIndex);
  if (!mcp || !pip || !tip) return 0;
  const angle = angleDegrees(subtract(mcp, pip), subtract(tip, pip));
  const angleCurl = 1 - clamp((angle - 78) / 92);
  const projection = dot(subtract(tip, mcp), palmAxis);
  const projectionCurl = 1 - clamp((projection - 0.05) / 0.22);
  return clamp(angleCurl * 0.62 + projectionCurl * 0.38);
}

function thumbClosedScore(landmarks: NormalizedLandmark[], center: Point, palmWidth: number): number {
  const tip = point(landmarks, 4);
  const mp = point(landmarks, 2);
  const ip = point(landmarks, 3);
  const indexMcp = point(landmarks, 5);
  if (!tip || !mp || !ip || !indexMcp) return 0;
  const closeToPalm = 1 - clamp((distance(tip, center) / palmWidth - 0.55) / 0.8);
  const closeToIndex = 1 - clamp((distance(tip, indexMcp) / palmWidth - 0.45) / 0.9);
  const angleCurl = 1 - clamp((angleDegrees(subtract(mp, ip), subtract(tip, ip)) - 76) / 94);
  return clamp(closeToPalm * 0.45 + closeToIndex * 0.3 + angleCurl * 0.25);
}

function pinchSpread(landmarks: NormalizedLandmark[]): { spread: number; palmWidth: number } | null {
  const thumbTip = point(landmarks, 4);
  const indexTip = point(landmarks, 8);
  const indexMcp = point(landmarks, 5);
  const littleMcp = point(landmarks, 17);
  if (!thumbTip || !indexTip || !indexMcp || !littleMcp) return null;
  return {
    spread: distance(thumbTip, indexTip),
    palmWidth: Math.max(distance(indexMcp, littleMcp), 0.04),
  };
}

function palmCenter(landmarks: NormalizedLandmark[]): Point | null {
  const values = [0, 5, 9, 13, 17].map((index) => point(landmarks, index)).filter((value): value is Point => value != null);
  if (values.length < 4) return null;
  const sum = values.reduce((total, value) => ({ x: total.x + value.x, y: total.y + value.y }), { x: 0, y: 0 });
  return { x: sum.x / values.length, y: sum.y / values.length };
}

function shortcutEventForPose(pose: AirHandPose): AirHandGestureId | null {
  if (pose === "open_palm" || pose === "left_click" || pose === "fist") return pose;
  return null;
}

function holdDuration(sensitivity: GestureSensitivity): number {
  if (sensitivity === "responsive") return 120;
  if (sensitivity === "deliberate") return 280;
  return 180;
}

function swipeThreshold(sensitivity: GestureSensitivity): number {
  if (sensitivity === "responsive") return 0.08;
  if (sensitivity === "deliberate") return 0.15;
  return 0.11;
}

function swipeSpeed(sensitivity: GestureSensitivity): number {
  if (sensitivity === "responsive") return 0.45;
  if (sensitivity === "deliberate") return 0.7;
  return 0.55;
}

function point(landmarks: NormalizedLandmark[], index: number): Point | null {
  const value = landmarks[index];
  return value && Number.isFinite(value.x) && Number.isFinite(value.y) ? { x: value.x, y: value.y } : null;
}

function subtract(left: Point, right: Point): Point {
  return { x: left.x - right.x, y: left.y - right.y };
}

function distance(left: Point, right: Point): number {
  return Math.hypot(left.x - right.x, left.y - right.y);
}

function dot(left: Point, right: Point): number {
  return left.x * right.x + left.y * right.y;
}

function normalized(value: Point): Point {
  const length = Math.max(Math.hypot(value.x, value.y), 0.000001);
  return { x: value.x / length, y: value.y / length };
}

function angleDegrees(left: Point, right: Point): number {
  const denominator = Math.max(Math.hypot(left.x, left.y) * Math.hypot(right.x, right.y), 0.000001);
  return Math.acos(Math.min(1, Math.max(-1, dot(left, right) / denominator))) * 180 / Math.PI;
}

function clamp(value: number): number {
  return Math.min(1, Math.max(0, value));
}
