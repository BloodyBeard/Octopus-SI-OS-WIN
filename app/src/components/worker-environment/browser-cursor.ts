type Point = { x: number; y: number };
type Size = { width: number; height: number };
type SpringConfig = { response: number; dampingFraction: number };
type Spring = SpringConfig & { force: number; simulationTime: number; scriptTime: number; target: number; value: number; velocity: number };
type Segment = { control1: Point; control2: Point; end: Point };
type Path = { start: Point; end: Point; arc: Point | null; segments: Segment[] };
type PathMeasures = { length: number; angleEnergy: number; maxAngle: number; totalTurn: number; inBounds: boolean };
type Motion = { mode: "bezier"; path: Path; progress: Spring } | { mode: "scoot"; start: Point; end: Point; axis: number; rotation: number; progress: Spring };
type MotionState = {
  point: Point; motion: Motion | null; x: Spring; y: Spring; rotation: number; rotationSpring: Spring;
  axis: number; axisSpring: Spring; scootRotation: Spring; scootStretch: Spring; stretch: Spring;
  visibility: Spring; thinkStartedAt: number | null;
};
export type BrowserCursorPoint = { x: number; y: number; moveSequence: number };
export type BrowserCursorState = { cursor: BrowserCursorPoint | null; visible: boolean; turnKey: string; viewportSize: Size };

const STEP = 1 / 240, FRAME = 1 / 60;
const POSITION = { response: .19, dampingFraction: .9 };
const ROTATION = { response: .12, dampingFraction: .9 };
const CURVE = { arcFlow: .5783555327868779, arcSize: .2765523188064277, margin: 20, count: 20, angle: -44, endpoint: .15, start: .41960295031576633 };
const clamp = (value: number, low: number, high: number) => Math.max(low, Math.min(high, value));
const lerp = (a: number, b: number, amount: number) => a + (b - a) * amount;
const distance = (a: Point, b: Point) => Math.hypot(b.x - a.x, b.y - a.y);
const degrees = (angle: number) => (angle % 360 + 360) % 360;
const rounded = (value: number) => Math.round(value * 1000) / 1000;
function unit(point: Point): Point { const length = Math.hypot(point.x, point.y); return length < .001 ? { x: 1, y: 0 } : { x: point.x / length, y: point.y / length }; }
function angleDifference(from: number, to: number, circle = 360) { let delta = to - from; while (delta > circle / 2) delta -= circle; while (delta < -circle / 2) delta += circle; return delta; }
function clickTangent() { const angle = CURVE.angle * Math.PI / 180; return { x: Math.sin(angle), y: -Math.cos(angle) }; }
function tangentRotation(point: Point) { if (distance({ x: 0, y: 0 }, point) < .001) return degrees(-44); return degrees(Math.atan2(point.y, point.x) * 180 / Math.PI + 90); }
function control(bounds: Size, point: Point, direction: Point, length: number): Point {
  let amount = length;
  if (direction.x < 0) amount = Math.min(amount, point.x / -direction.x);
  if (direction.x > 0) amount = Math.min(amount, (bounds.width - point.x) / direction.x);
  if (direction.y < 0) amount = Math.min(amount, point.y / -direction.y);
  if (direction.y > 0) amount = Math.min(amount, (bounds.height - point.y) / direction.y);
  return { x: point.x + direction.x * Math.max(0, amount), y: point.y + direction.y * Math.max(0, amount) };
}
function cubic(start: Point, segment: Segment, t: number): Point {
  const u = 1 - t, a = u ** 3, b = 3 * u * u * t, c = 3 * u * t * t, d = t ** 3;
  return { x: start.x * a + segment.control1.x * b + segment.control2.x * c + segment.end.x * d,
    y: start.y * a + segment.control1.y * b + segment.control2.y * c + segment.end.y * d };
}
function sample(path: Path, progress: number) {
  const t = clamp(progress, 0, 1), offset = t === 1 ? path.segments.length - 1 : t * path.segments.length;
  const index = Math.floor(offset), segment = path.segments[index], start = index === 0 ? path.start : path.segments[index - 1].end;
  const local = t === 1 ? 1 : offset - index, u = 1 - local;
  return { point: cubic(start, segment, local), tangent: {
    x: 3 * u * u * (segment.control1.x - start.x) + 6 * u * local * (segment.control2.x - segment.control1.x) + 3 * local * local * (segment.end.x - segment.control2.x),
    y: 3 * u * u * (segment.control1.y - start.y) + 6 * u * local * (segment.control2.y - segment.control1.y) + 3 * local * local * (segment.end.y - segment.control2.y),
  } };
}
function measure(path: Path, bounds?: Size): PathMeasures {
  let length = 0, angleEnergy = 0, maxAngle = 0, totalTurn = 0, angle: number | null = null;
  const within = (p: Point) => !bounds || p.x >= CURVE.margin && p.x <= bounds.width - CURVE.margin && p.y >= CURVE.margin && p.y <= bounds.height - CURVE.margin;
  let inBounds = within(path.start), previous = path.start, start = path.start;
  for (const segment of path.segments) {
    for (let index = 1; index <= 24; index++) {
      const point = cubic(start, segment, index / 24); length += distance(previous, point); inBounds &&= within(point);
      const delta = { x: point.x - previous.x, y: point.y - previous.y };
      if (distance({ x: 0, y: 0 }, delta) > .01) {
        const nextAngle = Math.atan2(delta.y, delta.x);
        if (angle !== null) { const change = angleDifference(angle, nextAngle, Math.PI * 2); angleEnergy += change * change; maxAngle = Math.max(maxAngle, Math.abs(change)); totalTurn += Math.abs(change); }
        angle = nextAngle;
      }
      previous = point;
    }
    start = segment.end;
  }
  return { length, angleEnergy, maxAngle, totalTurn, inBounds };
}
function opposition(path: Path) {
  const tangent = clickTangent(), direction = unit({ x: path.end.x - path.start.x, y: path.end.y - path.start.y });
  return clamp((-(direction.x * tangent.x + direction.y * tangent.y) - .08) / .92, 0, 1);
}
function pathScore(path: Path, m: PathMeasures) {
  const detour = Math.max(0, m.length / Math.max(1, distance(path.start, path.end)) - 1);
  return m.length + detour * 320 + m.angleEnergy * 140 + m.maxAngle * 180 + m.totalTurn * 18 + opposition(path) * 90 + (path.arc ? 45 : 0);
}
function pathSpring(path: Path): SpringConfig {
  const m = measure(path), detour = Math.max(0, m.length / Math.max(1, distance(path.start, path.end)) - 1);
  const length = clamp((m.length - 180) / 760, 0, 1), turn = clamp(m.totalTurn / (Math.PI * 1.4), 0, 1);
  const complexity = clamp(clamp(detour / .55, 0, 1) * .42 + turn * .38 + clamp(m.angleEnergy / 1.25, 0, 1) * .2, 0, 1);
  return { dampingFraction: .9, response: clamp((.42 + length * .22 + complexity * .12 + opposition(path) * .28 + (path.arc ? .04 : 0)) * .7 * (path.arc ? .9 : 1), .12, 2.2) };
}
function cursorPath(start: Point, end: Point, bounds: Size): Path {
  const length = distance(start, end), tangent = clickTangent(), reverse = { x: -tangent.x, y: -tangent.y };
  const direction = unit({ x: end.x - start.x, y: end.y - start.y });
  const startLength = Math.max(48, Math.min(640, length * CURVE.start, length * .9));
  const endLength = Math.max(48, Math.min(640, length * CURVE.endpoint, length * .9));
  const first = control(bounds, start, tangent, startLength), last = control(bounds, end, reverse, endLength);
  const normal = { x: -direction.y, y: direction.x }, sign = normal.x * tangent.x + normal.y * tangent.y >= 0 ? 1 : -1;
  const midpoint = { x: (start.x + end.x) / 2, y: (start.y + end.y) / 2 };
  const candidates: Path[] = [
    { start, end, arc: null, segments: [{ control1: first, control2: last, end }] },
    { start, end, arc: null, segments: [{ control1: control(bounds, start, tangent, startLength * .65), control2: control(bounds, end, reverse, endLength * .65), end }] },
  ];
  for (const arcScale of [.55, .8, 1.05]) for (const handleScale of [.65, 1, 1.35]) for (const side of [sign, -sign]) {
    const arcDistance = Math.max(50, Math.min(520, length * CURVE.arcSize)) * arcScale;
    const handleDistance = Math.max(38, Math.min(440, length * CURVE.arcFlow)) * handleScale;
    const arc = { x: midpoint.x + normal.x * side * arcDistance + tangent.x * startLength * .16, y: midpoint.y + normal.y * side * arcDistance + tangent.y * startLength * .16 };
    candidates.push({ start, end, arc, segments: [
      { control1: first, control2: { x: arc.x - direction.x * handleDistance, y: arc.y - direction.y * handleDistance }, end: arc },
      { control1: { x: arc.x + direction.x * handleDistance, y: arc.y + direction.y * handleDistance }, control2: last, end },
    ] });
  }
  let best = candidates[0], any = best, score = Infinity, anyScore = Infinity;
  for (const candidate of candidates.slice(0, CURVE.count)) {
    const measurements = measure(candidate, bounds), value = pathScore(candidate, measurements);
    if (value < anyScore) { any = candidate; anyScore = value; }
    if (measurements.inBounds && value < score) { best = candidate; score = value; }
  }
  return score === Infinity ? any : best;
}
function spring(value: number, target: number, config: SpringConfig): Spring { return { ...config, force: 0, simulationTime: 0, scriptTime: 0, value, target, velocity: 0 }; }
function reset(s: Spring, value: number) { s.force = 0; s.simulationTime = 0; s.scriptTime = 0; s.target = value; s.value = value; s.velocity = 0; }
function settled(s: Spring) { if (Math.max(s.velocity ** 2, s.force ** 2) > .06 ** 2) return false; const tolerance = s.target * .01; return tolerance === 0 || (s.target - s.value) ** 2 <= tolerance ** 2; }
function step(s: Spring, time: number) {
  const response = Math.max(.001, s.response), stiffness = Math.min((Math.PI * 2) ** 2 / response ** 2, 1 / (2 * STEP ** 2));
  const damping = Math.sqrt(stiffness) * 2 * s.dampingFraction;
  s.scriptTime += Math.max(0, time);
  if (s.scriptTime - s.simulationTime > 1) s.simulationTime = s.scriptTime - FRAME;
  while (s.simulationTime < s.scriptTime) {
    const velocity = s.velocity + s.force * STEP / 2; s.value += velocity * STEP;
    s.force = velocity * -damping + (s.target - s.value) * stiffness; s.velocity = velocity + s.force * STEP / 2; s.simulationTime += STEP;
  }
  if (settled(s)) s.value = s.target;
}
function rotationTarget(s: Spring, target: number) { s.target = s.value + angleDifference(s.value, target); }
function createState(point: Point, visible: boolean): MotionState {
  const rotation = degrees(-44);
  return { point, motion: null, x: spring(point.x, point.x, POSITION), y: spring(point.y, point.y, POSITION), rotation, rotationSpring: spring(rotation, rotation, ROTATION),
    axis: 0, axisSpring: spring(0, 0, ROTATION), scootRotation: spring(0, 0, { response: .055, dampingFraction: .82 }), scootStretch: spring(1, 1, { response: .12, dampingFraction: .86 }),
    stretch: spring(1, 1, { response: .2, dampingFraction: .85 }), visibility: spring(+visible, +visible, { response: .42, dampingFraction: .86 }), thinkStartedAt: null };
}
function resetScoot(state: MotionState) { reset(state.axisSpring, 0); reset(state.scootRotation, 0); reset(state.scootStretch, 1); state.axis = 0; }
function setPoint(state: MotionState, point: Point) { state.point = point; reset(state.x, point.x); reset(state.y, point.y); }
function snap(state: MotionState, point: Point) { state.motion = null; setPoint(state, point); reset(state.rotationSpring, degrees(-44)); state.rotation = state.rotationSpring.value; resetScoot(state); reset(state.stretch, 1); }
function positionConfig(state: MotionState, config: SpringConfig) { Object.assign(state.x, config); Object.assign(state.y, config); }
function move(state: MotionState, point: Point, size: Size) {
  state.thinkStartedAt = null;
  const start = { ...state.point };
  if (distance(start, point) <= 196) {
    const direction = unit({ x: point.x - start.x, y: point.y - start.y });
    const axis = distance({ x: 0, y: 0 }, direction) < .001 ? 0 : Math.atan2(direction.y, direction.x) * 180 / Math.PI;
    positionConfig(state, POSITION); state.x.target = point.x; state.y.target = point.y;
    rotationTarget(state.rotationSpring, degrees(-44)); rotationTarget(state.axisSpring, axis);
    state.motion = { mode: "scoot", start, end: point, axis, rotation: clamp(direction.x * .75 - direction.y * .62, -1, 1) * 70, progress: spring(0, 1, { response: .19, dampingFraction: .94 }) };
  } else {
    const path = cursorPath(start, point, size), config = pathSpring(path);
    positionConfig(state, { response: clamp(config.response * .18, .035, .12), dampingFraction: config.dampingFraction });
    state.motion = { mode: "bezier", path, progress: spring(0, 1, config) };
  }
}
function updatePosition(state: MotionState, time: number) {
  const previous = state.point;
  for (const s of [state.x, state.y, state.rotationSpring, state.axisSpring]) step(s, time);
  state.point = { x: state.x.value, y: state.y.value }; state.rotation = state.rotationSpring.value; state.axis = state.axisSpring.value;
  return distance(previous, state.point) / Math.max(time, STEP);
}
function near(state: MotionState, point: Point) { return distance(state.point, point) <= .85 && Math.abs(state.x.velocity) <= 12 && Math.abs(state.y.velocity) <= 12; }
function advance(state: MotionState, time: number, now: number) {
  const motion = state.motion; let arrived = false;
  if (!motion) { state.stretch.target = 1; state.scootStretch.target = 1; state.scootRotation.target = 0; }
  else {
    state.thinkStartedAt = null; step(motion.progress, Math.max(0, time));
    if (motion.mode === "bezier") {
      state.scootStretch.target = 1; state.scootRotation.target = 0;
      const progress = clamp(motion.progress.value, 0, 1), next = sample(motion.path, progress);
      state.x.target = next.point.x; state.y.target = next.point.y; rotationTarget(state.rotationSpring, tangentRotation(next.tangent)); rotationTarget(state.axisSpring, 0);
      state.stretch.target = clamp(1 - updatePosition(state, time) / 5500, .65, 1);
      if (progress >= .999 && Math.abs(motion.progress.velocity) < .01 && near(state, next.point)) {
        const end = sample(motion.path, 1); setPoint(state, end.point); reset(state.rotationSpring, tangentRotation(end.tangent)); state.rotation = state.rotationSpring.value;
        reset(state.axisSpring, 0); state.axis = 0; reset(state.stretch, 1); arrived = true;
      }
    } else {
      state.x.target = motion.end.x; state.y.target = motion.end.y; rotationTarget(state.axisSpring, motion.axis); rotationTarget(state.rotationSpring, degrees(-44)); updatePosition(state, time);
      const dx = motion.end.x - motion.start.x, dy = motion.end.y - motion.start.y, length = dx * dx + dy * dy;
      const progress = length < .001 ? 1 : clamp(((state.point.x - motion.start.x) * dx + (state.point.y - motion.start.y) * dy) / length, 0, 1);
      state.stretch.target = 1; state.scootStretch.target = lerp(1, lerp(1, 0, Math.sin(progress * Math.PI)), .15); state.scootRotation.target = motion.rotation * Math.sin(Math.min(1, progress) * Math.PI);
      if (progress >= .999 && Math.abs(motion.progress.velocity) < .01 && near(state, motion.end)) {
        setPoint(state, motion.end); reset(state.rotationSpring, degrees(-44)); state.rotation = state.rotationSpring.value; resetScoot(state); reset(state.stretch, 1); arrived = true;
      }
    }
    if (arrived) { state.motion = null; state.thinkStartedAt = now; }
  }
  for (const s of [state.visibility, state.stretch, state.scootStretch, state.scootRotation]) step(s, time);
  return arrived;
}
function animating(state: MotionState) {
  return state.motion !== null || state.thinkStartedAt !== null || [state.x, state.y, state.rotationSpring, state.axisSpring, state.scootRotation, state.scootStretch, state.stretch, state.visibility].some(s => s.value !== s.target || !settled(s));
}
function transform(state: MotionState, now: number) {
  let rotation = state.rotation;
  if (state.thinkStartedAt !== null) {
    const elapsed = (now - state.thinkStartedAt) / 1000, progress = Math.min(1, elapsed / 1.41);
    if (progress >= 1) state.thinkStartedAt = null;
    else rotation += Math.sin(elapsed / .66 * Math.PI * 2) * Math.sin(progress * Math.PI) * 12.5;
  }
  const visibility = clamp(state.visibility.value, 0, 1), scale = lerp(.4, 1, visibility), stretch = clamp(state.scootStretch.value, 0, 1);
  const values = [`translate3d(${rounded(state.point.x - 12)}px, ${rounded(state.point.y - 12)}px, 0)`];
  if (Math.abs(angleDifference(0, state.axis)) > .001 || Math.abs(stretch - 1) > .001) values.push(`rotate(${rounded(state.axis)}deg)`, `scale(1, ${rounded(stretch)})`, `rotate(${rounded(-state.axis)}deg)`);
  values.push(`rotate(${rounded(degrees(rotation + state.scootRotation.value))}deg)`, `scale(${rounded(state.stretch.value * scale)}, ${rounded(scale)})`);
  return { transform: values.join(" "), opacity: String(rounded(visibility)), filter: `blur(${rounded(lerp(5, 0, visibility))}px)` };
}

export function createBrowserCursor(parent: HTMLElement, assetUrl: string, onArrived: (sequence: number) => void) {
  const layer = document.createElement("div"); layer.setAttribute("aria-hidden", "true");
  Object.assign(layer.style, { inset: "0", overflow: "hidden", pointerEvents: "none", position: "absolute", zIndex: "20" });
  const cursor = document.createElement("div"); cursor.dataset.testid = "browser-model-cursor";
  Object.assign(cursor.style, { height: "24px", left: "0", top: "0", position: "absolute", transformOrigin: "12px 12px", willChange: "transform", width: "24px" });
  const imageOffset = document.createElement("div"); imageOffset.style.transform = "translate3d(12px, -2.5px, 0)";
  const image = document.createElement("img"); image.alt = ""; image.draggable = false; image.height = 24; image.width = 23; image.src = assetUrl;
  Object.assign(image.style, { display: "block", filter: "drop-shadow(0 0 6px color-mix(in srgb, var(--color-accent-blue) 90%, transparent)) drop-shadow(0 0 15px color-mix(in srgb, var(--color-accent-blue) 48%, transparent))", transform: "rotate(44deg) scale(1)", transformOrigin: "0 0" });
  imageOffset.appendChild(image); cursor.appendChild(imageOffset); layer.appendChild(cursor); parent.appendChild(layer);
  let state: MotionState | null = null, animation: number | null = null, previousTime = performance.now(), firstFrame = false, destroyed = false;
  let sequence: number | null = null, arrivalKey: string | null = null, arrivedKey: string | null = null, thoughtKey: string | null = null, revealedKey: string | null = null;
  const render = () => { if (state) Object.assign(cursor.style, transform(state, performance.now())); };
  const arrive = () => { if (sequence !== null && arrivalKey !== null && arrivalKey !== arrivedKey) { arrivedKey = arrivalKey; onArrived(sequence); } };
  const schedule = () => {
    if (animation !== null || !state || destroyed) return;
    animation = window.requestAnimationFrame(now => {
      animation = null; if (!state || destroyed) return;
      const time = firstFrame ? FRAME : Math.max(FRAME, (now - previousTime) / 1000); firstFrame = false; previousTime = now;
      const arrived = advance(state, time, now); render(); if (arrived) arrive(); if (animating(state)) schedule();
    });
  };
  return {
    setState(next: BrowserCursorState) {
      if (destroyed) return;
      const point = { x: clamp(next.cursor?.x ?? Math.round(next.viewportSize.width * .58), 0, next.viewportSize.width), y: clamp(next.cursor?.y ?? Math.round(next.viewportSize.height * .55), 0, next.viewportSize.height) };
      sequence = next.cursor?.moveSequence ?? null; arrivalKey = sequence === null ? null : `${next.turnKey}:${sequence}`;
      state ??= createState(point, next.visible); state.visibility.target = +next.visible;
      if (next.visible && !next.cursor && thoughtKey !== next.turnKey) { thoughtKey = next.turnKey; reset(state.visibility, 1); state.thinkStartedAt = performance.now(); }
      if (!next.cursor) { snap(state, point); render(); schedule(); return; }
      const reveal = next.visible && state.visibility.value <= .001 && revealedKey !== next.turnKey;
      state.thinkStartedAt = null;
      if (reveal || distance(state.point, point) < .5) {
        if (reveal) { revealedKey = next.turnKey; reset(state.visibility, 1); }
        snap(state, point); render(); arrive(); schedule(); return;
      }
      move(state, point, next.viewportSize); firstFrame = true; render(); schedule();
    },
    destroy() { destroyed = true; if (animation !== null) window.cancelAnimationFrame(animation); animation = null; layer.remove(); },
  };
}
