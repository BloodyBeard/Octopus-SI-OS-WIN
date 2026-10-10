import { useMemo, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactElement, type WheelEvent as ReactWheelEvent } from "react";
import type { EnvironmentNode, InterfaceSummary, SelectionRef } from "../data/topology";

// The two-layer projection (owner's reference): the SYSTEM surface below carries the zones
// — Execution Environments as nested AREAS (zones inside zones), each holding its runs as
// dots. A run's interface bindings are blue lines rising from the run to the WORLD surface
// above, where each world interface is a named blue dot. Past that boundary nothing is
// visible by design; only artifacts come back. Drag rotates (vertical inverted, the way the
// owner expects), wheel zooms, corner controls do the same.
//
// Everything drawn is projected truth: zones/runs from the EE projection, world dots from
// registered world interfaces, lines only where a run actually binds that world interface.
// No decorative movement — that lives in the Event Log.

type Props = {
  environments: EnvironmentNode[];
  registeredWorldInterfaces: InterfaceSummary[];
  mode: "instances" | "routeSets";
  selected: SelectionRef | null;
  onSelect: (selection: SelectionRef) => void;
};

type WorldNode = {
  id: string;
  label: string;
  members: string[];
  bound: boolean;
};

type RunNode = { run: EnvironmentNode["runs"][number]; lx: number; ly: number; world: string[] };
type ZoneNode = {
  env: EnvironmentNode;
  depth: number;
  w: number;
  h: number;
  lx: number; // offset within the parent zone
  ly: number;
  runs: RunNode[];
  children: ZoneNode[];
};
type RunAbs = { run: EnvironmentNode["runs"][number]; x: number; y: number; world: string[] };
type ZoneAbs = { env: EnvironmentNode; depth: number; x: number; y: number; width: number; height: number; runs: RunAbs[] };

const RUN_CELL = 100;
const ZONE_PAD = 38;
const ZONE_GAP = 30;
const ZONE_HEAD = 36;
const MIN_INNER = 150;
const WORLD_ELEVATION = 330;
const DEFAULT_YAW = -14;
const DEFAULT_PITCH = 50;
const DEFAULT_ZOOM = 1.08;
const MIN_ZOOM = 0.35;
const MAX_ZOOM = 5.2;
const WORLD_GRID_GAP_X = 150;
const WORLD_GRID_GAP_Y = 104;
const WORLD_SURFACE_PAD = 86;
const WORLD_SURFACE_MIN_HALF = 280;

export function WorldProjection({ environments, registeredWorldInterfaces, mode, selected, onSelect }: Props) {
  const [yawDeg, setYawDeg] = useState(DEFAULT_YAW);
  const [pitchDeg, setPitchDeg] = useState(DEFAULT_PITCH);
  const [zoom, setZoom] = useState(DEFAULT_ZOOM);
  const dragRef = useRef<{ pointerId: number; x: number; y: number } | null>(null);

  const scene = useMemo(
    () => buildScene(environments, registeredWorldInterfaces, mode),
    [environments, registeredWorldInterfaces, mode],
  );
  const project = useMemo(() => makeProjector(yawDeg, pitchDeg), [yawDeg, pitchDeg]);

  function onPointerDown(event: ReactPointerEvent<SVGSVGElement>) {
    dragRef.current = { pointerId: event.pointerId, x: event.clientX, y: event.clientY };
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function onPointerMove(event: ReactPointerEvent<SVGSVGElement>) {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    const dx = event.clientX - drag.x;
    const dy = event.clientY - drag.y;
    dragRef.current = { pointerId: drag.pointerId, x: event.clientX, y: event.clientY };
    setYawDeg((yaw) => yaw - dx * 0.35);
    // Vertical is INVERTED (owner): drag up → tilt down toward the plane, drag down → rise.
    setPitchDeg((pitch) => clamp(pitch - dy * 0.25, 35, 80));
  }

  function onPointerUp(event: ReactPointerEvent<SVGSVGElement>) {
    if (dragRef.current?.pointerId === event.pointerId) dragRef.current = null;
  }

  function onWheel(event: ReactWheelEvent<SVGSVGElement>) {
    event.preventDefault();
    setZoom((current) => clamp(current * Math.exp(-event.deltaY * 0.0016), MIN_ZOOM, MAX_ZOOM));
  }

  const worldColumns = Math.max(1, Math.ceil(Math.sqrt(scene.worldNodes.length)));
  const worldRows = Math.max(1, Math.ceil(scene.worldNodes.length / worldColumns));
  const worldGridHalfWidth = ((worldColumns - 1) * WORLD_GRID_GAP_X) / 2;
  const worldGridHalfDepth = ((worldRows - 1) * WORLD_GRID_GAP_Y) / 2;
  const worldDots = scene.worldNodes.map((node, index) => {
    const row = Math.floor(index / worldColumns);
    const column = index % worldColumns;
    const itemsInRow = Math.min(worldColumns, scene.worldNodes.length - row * worldColumns);
    const x = (column - (itemsInRow - 1) / 2) * WORLD_GRID_GAP_X;
    const y = (row - (worldRows - 1) / 2) * WORLD_GRID_GAP_Y;
    return {
      ...node,
      point: project(x, y, WORLD_ELEVATION),
    };
  });
  const worldDotByName = new Map<string, (typeof worldDots)[number]>();
  for (const dot of worldDots) {
    for (const member of dot.members) worldDotByName.set(member, dot);
  }
  const boundNodeCount = worldDots.filter((dot) => dot.bound).length;
  const worldSurfaceHalfExtent = Math.max(
    WORLD_SURFACE_MIN_HALF,
    scene.systemHalfWidth * 0.82,
    worldGridHalfWidth + WORLD_SURFACE_PAD,
    worldGridHalfDepth + WORLD_SURFACE_PAD,
  );

  // The viewBox frames the content at the DEFAULT orientation and then stays FIXED — it is
  // NOT recomputed as you rotate, so rotating just turns the scene inside a stable frame
  // (no "strange enlarging" from the frame re-fitting on every drag). Zoom is separate.
  const viewBox = useMemo(() => {
    const fit = makeProjector(DEFAULT_YAW, DEFAULT_PITCH, 1);
    // Frame the ACTUAL visible content — the zones (there is no drawn system surface) plus
    // the world surface above — so the projection centers instead of hugging the top.
    const points = [
      ...scene.zones.flatMap((zone) => [
        fit(zone.x, zone.y, 0),
        fit(zone.x + zone.width, zone.y, 0),
        fit(zone.x + zone.width, zone.y + zone.height, 0),
        fit(zone.x, zone.y + zone.height, 0),
      ]),
      fit(-worldSurfaceHalfExtent, -worldSurfaceHalfExtent, WORLD_ELEVATION),
      fit(worldSurfaceHalfExtent, -worldSurfaceHalfExtent, WORLD_ELEVATION),
      fit(worldSurfaceHalfExtent, worldSurfaceHalfExtent, WORLD_ELEVATION),
      fit(-worldSurfaceHalfExtent, worldSurfaceHalfExtent, WORLD_ELEVATION),
    ];
    if (!points.length) return "-640 -430 1280 860";
    const xs = points.map((point) => point.x);
    const ys = points.map((point) => point.y);
    const minX = Math.min(...xs);
    const maxX = Math.max(...xs);
    const minY = Math.min(...ys);
    const maxY = Math.max(...ys);
    const padX = 52;
    const padTop = 30; // world-dot labels sit above the top surface
    const padBottom = 34; // run labels sit below their zone
    return `${minX - padX} ${minY - padTop} ${maxX - minX + padX * 2} ${maxY - minY + padTop + padBottom}`;
  }, [scene.zones, worldSurfaceHalfExtent]);

  return (
    <div className="world-projection">
      <svg
        viewBox={viewBox}
        role="application"
        aria-label="Two-layer world projection: drag to rotate, wheel to zoom"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onWheel={onWheel}
      >
        <g transform={`scale(${zoom})`}>
          {scene.zones.map((zone) => (
            <ZoneShape zone={zone} project={project} selected={selected} onSelect={onSelect} key={zone.env.id} />
          ))}

          {worldDots.length > 0 ? (
            <>
              <polygon className="wp-surface wp-surface-world" points={surfacePoints(project, worldSurfaceHalfExtent, worldSurfaceHalfExtent, WORLD_ELEVATION)} />
              <SurfaceLabel
                project={project}
                x={-worldSurfaceHalfExtent + 30}
                y={worldSurfaceHalfExtent - 26}
                z={WORLD_ELEVATION}
                text={mode === "routeSets"
                  ? `World Surface · ${worldDots.length} route sets · ${boundNodeCount} bound`
                  : `World Surface · ${worldDots.length} registered · ${scene.boundWorldInterfaces.size} bound`}
                className="wp-surface-caption is-world"
              />
            </>
          ) : null}

          {scene.zones.flatMap((zone) => zoneRoutes(zone, project, worldDotByName))}
          {scene.zones.flatMap((zone) => zoneRuns(zone, project, selected, onSelect))}

          {worldDots.map((dot) => (
            <g
              className={`wp-world-dot ${dot.bound ? "is-bound" : "is-unbound"} ${selected?.kind === "interface" && dot.members.includes(selected.id) ? "is-selected" : ""}`}
              onClick={(event) => {
                event.stopPropagation();
                onSelect({ kind: "interface", id: dot.id });
              }}
              key={dot.id}
            >
              <title>{mode === "routeSets" ? `${dot.id}\n${dot.members.join("\n")}` : dot.id}</title>
              <circle cx={dot.point.x} cy={dot.point.y} r={7} />
              <circle className="wp-world-dot-core" cx={dot.point.x} cy={dot.point.y} r={3.2} />
              <text x={dot.point.x} y={dot.point.y - 16} textAnchor="middle">{dot.label}</text>
            </g>
          ))}
        </g>
      </svg>

      <div className="world-controls" role="group" aria-label="Projection controls">
        <button type="button" title="Rotate left" aria-label="Rotate left" onClick={() => setYawDeg((yaw) => yaw - 15)}>⟲</button>
        <button type="button" title="Rotate right" aria-label="Rotate right" onClick={() => setYawDeg((yaw) => yaw + 15)}>⟳</button>
        <button type="button" title="Zoom out" aria-label="Zoom out" onClick={() => setZoom((current) => clamp(current / 1.2, MIN_ZOOM, MAX_ZOOM))}>−</button>
        <button type="button" title="Zoom in" aria-label="Zoom in" onClick={() => setZoom((current) => clamp(current * 1.2, MIN_ZOOM, MAX_ZOOM))}>+</button>
        <button type="button" title="Reset view" aria-label="Reset view" onClick={() => { setYawDeg(DEFAULT_YAW); setPitchDeg(DEFAULT_PITCH); setZoom(DEFAULT_ZOOM); }}>⌂</button>
      </div>

      {scene.worldNodes.length === 0 ? (
        <div className="world-empty-note">No world interfaces are registered.</div>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------------------
// Scene: nested zone layout in world units. A zone is sized to CONTAIN its runs and its
// child zones (zones inside zones); the whole cluster is centered on the system surface so
// the zones sit INSIDE it, never on its edge.
// ---------------------------------------------------------------------------------------

function buildScene(
  environments: EnvironmentNode[],
  registeredWorldInterfaces: InterfaceSummary[],
  mode: "instances" | "routeSets",
) {
  const byParent = new Map<string, EnvironmentNode[]>();
  for (const env of environments) {
    const key = env.parentId && environments.some((item) => item.id === env.parentId) ? env.parentId : "";
    const list = byParent.get(key) ?? [];
    list.push(env);
    byParent.set(key, list);
  }

  function layout(env: EnvironmentNode, depth: number): ZoneNode {
    const children = (byParent.get(env.id) ?? []).map((child) => layout(child, depth + 1));
    const runs = env.runs;
    const n = runs.length;
    const cols = n ? Math.max(1, Math.ceil(Math.sqrt(n))) : 0;
    const rows = cols ? Math.ceil(n / cols) : 0;
    const runsW = cols * RUN_CELL;
    const runsH = rows * RUN_CELL;
    const childrenW = children.length
      ? children.reduce((total, child) => total + child.w, 0) + ZONE_GAP * (children.length - 1)
      : 0;
    const childrenH = children.length ? Math.max(...children.map((child) => child.h)) : 0;
    const innerW = Math.max(runsW, childrenW, MIN_INNER);
    const gapRC = runs.length && children.length ? ZONE_GAP : 0;
    const innerH = runsH + gapRC + childrenH;
    const w = innerW + ZONE_PAD * 2;
    const h = ZONE_HEAD + innerH + ZONE_PAD;

    const runNodes: RunNode[] = runs.map((run, index) => {
      const col = index % cols;
      const row = Math.floor(index / cols);
      const itemsInRow = Math.min(n - row * cols, cols);
      const rowStart = ZONE_PAD + (innerW - itemsInRow * RUN_CELL) / 2;
      const boundInterfaces = dedupe(run.interfaces.map((item) => item.name));
      const world = boundInterfaces.filter((name) => env.worldInterfaces.includes(name));
      return {
        run,
        lx: rowStart + col * RUN_CELL + RUN_CELL / 2,
        ly: ZONE_HEAD + row * RUN_CELL + RUN_CELL / 2,
        world,
      };
    });

    let childX = ZONE_PAD + (innerW - childrenW) / 2;
    const childY = ZONE_HEAD + runsH + gapRC;
    for (const child of children) {
      child.lx = childX;
      child.ly = childY;
      childX += child.w + ZONE_GAP;
    }

    return { env, depth, w, h, lx: 0, ly: 0, runs: runNodes, children };
  }

  const roots = (byParent.get("") ?? []).map((env) => layout(env, 0));
  const rootsW = roots.reduce((total, zone) => total + zone.w, 0) + ZONE_GAP * Math.max(0, roots.length - 1);
  const rootsH = roots.length ? Math.max(...roots.map((zone) => zone.h)) : 0;
  let cursor = -rootsW / 2;
  for (const root of roots) {
    root.lx = cursor;
    root.ly = -rootsH / 2;
    cursor += root.w + ZONE_GAP;
  }

  const zones: ZoneAbs[] = [];
  function absolutize(node: ZoneNode, px: number, py: number) {
    const x = px + node.lx;
    const y = py + node.ly;
    zones.push({
      env: node.env,
      depth: node.depth,
      x,
      y,
      width: node.w,
      height: node.h,
      runs: node.runs.map((run) => ({ run: run.run, x: x + run.lx, y: y + run.ly, world: run.world })),
    });
    for (const child of node.children) absolutize(child, x, y);
  }
  for (const root of roots) absolutize(root, 0, 0);

  const boundWorldInterfaceNames = dedupe(environments.flatMap((env) => env.worldInterfaces));
  const boundWorldInterfaces = new Set(boundWorldInterfaceNames);
  const registrations = registeredWorldInterfaces.map((iface) => {
    const sidecar = descriptorObject(iface.descriptor, "octopus_os_sidecar");
    const ownerInterface = descriptorString(sidecar, "owner_interface");
    return {
      id: iface.id,
      routeSetId: descriptorString(sidecar, "workspace") === "octopus_os" && ownerInterface
        ? ownerInterface
        : iface.id,
    };
  });
  for (const id of boundWorldInterfaceNames) {
    if (!registrations.some((registration) => registration.id === id)) registrations.push({ id, routeSetId: id });
  }
  const worldNodes = mode === "routeSets"
    ? groupWorldInterfaceRouteSets(registrations, boundWorldInterfaces)
    : registrations
      .map(({ id }) => ({
        id,
        label: compactWorldInterfaceLabel(id),
        members: [id],
        bound: boundWorldInterfaces.has(id),
      }))
      .sort((left, right) => left.id.localeCompare(right.id));

  if (!zones.length) {
    return { zones, systemHalfWidth: 300, systemHalfDepth: 200, worldNodes, boundWorldInterfaces };
  }

  // Center the whole cluster on the surface: the zones sit INSIDE it, centered, not at the rim.
  const minX = Math.min(...zones.map((zone) => zone.x));
  const maxX = Math.max(...zones.map((zone) => zone.x + zone.width));
  const minY = Math.min(...zones.map((zone) => zone.y));
  const maxY = Math.max(...zones.map((zone) => zone.y + zone.height));
  const centerX = (minX + maxX) / 2;
  const centerY = (minY + maxY) / 2;
  for (const zone of zones) {
    zone.x -= centerX;
    zone.y -= centerY;
    for (const run of zone.runs) {
      run.x -= centerX;
      run.y -= centerY;
    }
  }

  const systemHalfWidth = Math.max((maxX - minX) / 2 + 90, 300);
  const systemHalfDepth = Math.max((maxY - minY) / 2 + 80, 200);
  return { zones, systemHalfWidth, systemHalfDepth, worldNodes, boundWorldInterfaces };
}

function groupWorldInterfaceRouteSets(
  registrations: Array<{ id: string; routeSetId: string }>,
  boundWorldInterfaces: Set<string>,
): WorldNode[] {
  const membersByRouteSet = new Map<string, string[]>();
  for (const registration of registrations) {
    const members = membersByRouteSet.get(registration.routeSetId) ?? [];
    members.push(registration.id);
    membersByRouteSet.set(registration.routeSetId, members);
  }
  return Array.from(membersByRouteSet, ([id, members]) => {
    const sortedMembers = dedupe(members).sort((left, right) => left.localeCompare(right));
    return {
      id,
      label: sortedMembers.length > 1
        ? `${compactWorldInterfaceLabel(id)} · ${sortedMembers.length}`
        : compactWorldInterfaceLabel(id),
      members: sortedMembers,
      bound: sortedMembers.some((member) => boundWorldInterfaces.has(member)),
    };
  }).sort((left, right) => left.id.localeCompare(right.id));
}

// ---------------------------------------------------------------------------------------
// Projection + drawing helpers.
// ---------------------------------------------------------------------------------------

type Projector = (x: number, y: number, z: number) => { x: number; y: number };

function makeProjector(yawDeg: number, pitchDeg: number, zoom = 1): Projector {
  const yaw = (yawDeg * Math.PI) / 180;
  const pitch = (pitchDeg * Math.PI) / 180;
  const cosYaw = Math.cos(yaw);
  const sinYaw = Math.sin(yaw);
  const cosPitch = Math.cos(pitch);
  const sinPitch = Math.sin(pitch);
  return (x, y, z) => {
    const u = x * cosYaw - y * sinYaw;
    const v = x * sinYaw + y * cosYaw;
    return {
      x: u * zoom,
      y: (v * cosPitch - (z - WORLD_ELEVATION / 2) * sinPitch) * zoom,
    };
  };
}

function surfacePoints(project: Projector, halfWidth: number, halfDepth: number, z: number): string {
  return [
    project(-halfWidth, -halfDepth, z),
    project(halfWidth, -halfDepth, z),
    project(halfWidth, halfDepth, z),
    project(-halfWidth, halfDepth, z),
  ].map((point) => `${point.x},${point.y}`).join(" ");
}

function SurfaceLabel({ project, x, y, z, text, className }: {
  project: Projector;
  x: number;
  y: number;
  z: number;
  text: string;
  className: string;
}) {
  const point = project(x, y, z);
  return <text className={className} x={point.x} y={point.y}>{text}</text>;
}

function ZoneShape({ zone, project, selected, onSelect }: {
  zone: ZoneAbs;
  project: Projector;
  selected: SelectionRef | null;
  onSelect: (selection: SelectionRef) => void;
}) {
  const left = zone.x;
  const right = zone.x + zone.width;
  const near = zone.y;
  const far = zone.y + zone.height;
  const points = [
    project(left, near, 0),
    project(right, near, 0),
    project(right, far, 0),
    project(left, far, 0),
  ];
  const labelPoint = project(left + 16, near + 24, 0);
  const isSelected = selected?.kind === "environment" && selected.id === zone.env.id;
  return (
    <g
      className={`wp-zone accent-${zone.env.accent} depth-${Math.min(zone.depth, 3)} ${isSelected ? "is-selected" : ""}`}
      onClick={(event) => {
        event.stopPropagation();
        onSelect({ kind: "environment", id: zone.env.id });
      }}
    >
      <polygon points={points.map((point) => `${point.x},${point.y}`).join(" ")} />
      <text x={labelPoint.x} y={labelPoint.y}>{zone.env.name}</text>
    </g>
  );
}

function zoneRoutes(
  zone: ZoneAbs,
  project: Projector,
  worldDotByName: Map<string, { id: string; point: { x: number; y: number } }>,
): ReactElement[] {
  const routes: ReactElement[] = [];
  // A binding line ALWAYS leaves a RUN (its checkpoint bindings), never the zone edge — the
  // interface binding lives on a checkpoint, and a checkpoint lives inside a run.
  for (const run of zone.runs) {
    const runPoint = project(run.x, run.y, 0);
    const routedNodes = new Set<string>();
    for (const name of run.world) {
      const dot = worldDotByName.get(name);
      if (!dot || routedNodes.has(dot.id)) continue;
      routedNodes.add(dot.id);
      // Rim to rim: leave the run dot's edge, meet the world dot's edge — attaches exactly
      // at any rotation/zoom instead of overshooting into the centers.
      const from = trimToCircle(dot.point, runPoint, 8);
      const to = trimToCircle(runPoint, dot.point, 7);
      routes.push(
        <line className="wp-route" x1={from.x} y1={from.y} x2={to.x} y2={to.y} key={`route:${zone.env.id}:${run.run.id}:${dot.id}`} />,
      );
    }
  }
  return routes;
}

function zoneRuns(
  zone: ZoneAbs,
  project: Projector,
  selected: SelectionRef | null,
  onSelect: (selection: SelectionRef) => void,
): ReactElement[] {
  return zone.runs.map((run) => {
    const point = project(run.x, run.y, 0);
    const isSelected = selected?.kind === "run" && selected.id === run.run.id;
    return (
      <g
        className={`wp-run ${isSelected ? "is-selected" : ""}`}
        onClick={(event) => {
          event.stopPropagation();
          onSelect({ kind: "run", id: run.run.id });
        }}
        key={`run:${zone.env.id}:${run.run.id}`}
      >
        <circle className="wp-run-dot" cx={point.x} cy={point.y} r={8} />
        <circle className="wp-run-core" cx={point.x} cy={point.y} r={3.4} />
        <text x={point.x} y={point.y + 20} textAnchor="middle">{run.run.worker}</text>
      </g>
    );
  });
}

// End a line ON the rim of the target circle so it reads as attached, not overshooting.
function trimToCircle(from: { x: number; y: number }, center: { x: number; y: number }, radius: number): { x: number; y: number } {
  const dx = center.x - from.x;
  const dy = center.y - from.y;
  const length = Math.hypot(dx, dy) || 1;
  return { x: center.x - (dx / length) * radius, y: center.y - (dy / length) * radius };
}

function dedupe(values: string[]): string[] {
  return Array.from(new Set(values.filter(Boolean)));
}

function compactWorldInterfaceLabel(name: string): string {
  const withoutAxis = name.replace(/^world_/, "");
  const instance = /^(.*)_([0-9a-f]{32})$/.exec(withoutAxis);
  const label = instance ? `${instance[1]} · ${instance[2].slice(0, 6)}` : withoutAxis;
  return label.length > 26 ? `${label.slice(0, 25)}…` : label;
}

function descriptorString(descriptor: Record<string, unknown>, field: string): string {
  const value = descriptor[field];
  return typeof value === "string" ? value.trim() : "";
}

function descriptorObject(descriptor: Record<string, unknown>, field: string): Record<string, unknown> {
  const value = descriptor[field];
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
