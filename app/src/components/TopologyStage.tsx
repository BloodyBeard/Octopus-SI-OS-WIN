import { memo, useCallback, useEffect, useId, useMemo, useRef, useState, type CSSProperties, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import type { CheckpointRow, EnvironmentNode, RunProjection, SelectionRef, TopologySnapshot } from "../data/topology";
import type { DashboardFilters, DashboardScope, DashboardTopologyMode } from "../uiState";
import { Glyph } from "./IconGlyphs";
import { WorldProjection } from "./WorldProjection";
import { SystemArtifactTransfers, useArtifactTransferFeed } from "./ArtifactTransferAnimation";
import { DashboardLabel, DashboardTooltip } from "./DashboardLabel";
import { useAppErrorToast } from "../toasts";
import { OctopusLoadingState } from "./OctopusLoadingLogo";

// Two scopes, two honest pictures (owner):
//   System — zone cards, one level at a time (drill into child environments instead of
//   drawing nests into each other). The card repeats the reference: Goal, runs with their
//   checkpoints and binding chips, child environments. NOTHING world-related is drawn in
//   System scope.
//   World — the two-layer 3D projection: system surface with nested zones and runs below,
//   world interfaces as named blue dots on the plane above, interface routes going up.
// One dot language everywhere: blue = interface, green = alive/completed, yellow = in
// progress, red = broken, purple = checkpoint identity, gray = not materialized.

const CARD_WIDTH = 330;
const RUN_NODE_WIDTH = CARD_WIDTH;
const RUN_NODE_HEIGHT = 286;
const ZONE_MIN_WIDTH = 390;
const ZONE_HEADER = 38;
const ZONE_PAD = 22;
const ZONE_GAP = 26;
const SURFACE_GRID = 52; // world-units per grid square at scale 1
const MIN_SCALE = 0.35;
const STEP_UP_SCALE = 0.5; // zoom out past this at a child level → the parent frame takes over
const MAX_SCALE = 2.2;
const RUN_POSITIONS_KEY = "octopus.runPos.v6.zones";
const ZONE_PALETTE_KEY = "octopus.eeZonePalette.v1.zones";
const ZONES_INITIAL_CHECKPOINTS = 2;
const ZONE_MATERIALS: ZoneMaterial[] = [
  { id: "aqua", rgb: [54, 169, 184], edgeRgb: [83, 202, 213], labelRgb: [164, 228, 234] },
  { id: "lapis", rgb: [70, 130, 218], edgeRgb: [105, 164, 238], labelRgb: [181, 209, 250] },
  { id: "moss", rgb: [104, 164, 92], edgeRgb: [138, 199, 123], labelRgb: [196, 232, 188] },
  { id: "violet", rgb: [145, 112, 224], edgeRgb: [179, 149, 246], labelRgb: [219, 205, 255] },
  { id: "coral", rgb: [207, 102, 92], edgeRgb: [235, 136, 124], labelRgb: [250, 205, 198] },
  { id: "ochre", rgb: [197, 145, 67], edgeRgb: [230, 178, 95], labelRgb: [244, 221, 178] },
  { id: "rose", rgb: [203, 91, 139], edgeRgb: [235, 126, 169], labelRgb: [248, 199, 220] },
  { id: "pine", rgb: [58, 151, 122], edgeRgb: [88, 191, 157], labelRgb: [176, 226, 210] },
  { id: "steel", rgb: [106, 132, 171], edgeRgb: [140, 167, 207], labelRgb: [204, 218, 239] },
];
const ZONE_MATERIALS_BY_ID = new Map(ZONE_MATERIALS.map((material) => [material.id, material]));
// The camera looks at an infinite plane: (x, y) is where the plane's origin sits on
// screen, scale is the zoom. World point (wx, wy) draws at (x + wx*scale, y + wy*scale).
type Camera = { x: number; y: number; scale: number };
const HOME_CAMERA: Camera = { x: 32, y: 28, scale: 1 };
function clampScale(scale: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}

type ZoneSize = { width: number; height: number };
type Point = { x: number; y: number };
type Rgb = [number, number, number];
type ZoneMaterial = {
  id: string;
  rgb: Rgb;
  edgeRgb: Rgb;
  labelRgb: Rgb;
};
type ZonePaletteResult = {
  assignments: Record<string, string>;
  materialsById: Map<string, ZoneMaterial>;
  signature: string;
};
type ZonePaint = {
  zone: ZoneRect;
  order: number;
  maskId: string;
  material: ZoneMaterial;
};
type ZoneLayout = {
  zones: Array<ZoneRect>;
  zonesById: Map<string, ZoneRect>;
  runNodes: Array<RunNodeRect>;
  width: number;
  height: number;
};
type ZoneRect = {
  id: string;
  name: string;
  parentId?: string;
  depth: number;
  x: number;
  y: number;
  width: number;
  height: number;
  accent: string;
  statusLabel: string;
  runCount: number;
  childCount: number;
};
type RunNodeRect = {
  run: RunProjection;
  env: EnvironmentNode;
  x: number;
  y: number;
};

type Props = {
  snapshot: TopologySnapshot;
  query: string;
  scope: DashboardScope;
  viewMode: DashboardTopologyMode;
  filters: DashboardFilters;
  selected: SelectionRef | null;
  onSelect: (selection: SelectionRef) => void;
};


export const TopologyStage = memo(function TopologyStage({ snapshot, query, scope, viewMode, filters, selected, onSelect }: Props) {
  const zoneMaskPrefix = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const artifactTransfers = useArtifactTransferFeed(snapshot.projection);
  useAppErrorToast(artifactTransfers.error);
  // Callback-state ref: the grid wrap appears only once the projection has zones, so the
  // wheel listener must attach when the ELEMENT arrives, not when the component mounts.
  const [wrapEl, setWrapEl] = useState<HTMLDivElement | null>(null);
  const [levelId, setLevelId] = useState("");
  const normalizedQuery = query.trim().toLowerCase();
  const environmentsById = useMemo(
    () => new Map(snapshot.environments.map((env) => [env.id, env])),
    [snapshot.environments],
  );
  const childCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const env of snapshot.environments) {
      if (!env.parentId) continue;
      counts.set(env.parentId, (counts.get(env.parentId) ?? 0) + 1);
    }
    return counts;
  }, [snapshot.environments]);

  // A vanished level (its EE left the projection) falls back to the roots.
  useEffect(() => {
    if (levelId && !environmentsById.has(levelId)) setLevelId("");
  }, [levelId, environmentsById]);

  // Selecting a zone on another level (e.g. a child from the inspector) moves the stage to
  // the level that contains it, so the selection is always visible.
  const selectedEnvironment = useMemo(() => {
    if (!selected) return null;
    if (selected.kind === "environment") return environmentsById.get(selected.id) ?? null;
    return snapshot.environments.find((env) => env.runs.some((run) => run.id === selected.id)) ?? null;
  }, [selected, environmentsById, snapshot.environments]);
  const lastSelectionKeyRef = useRef("");
  useEffect(() => {
    if (!selectedEnvironment || scope === "World" || viewMode !== "drilldown") return;
    const selectionKey = `${selected?.kind}:${selected && "id" in selected ? selected.id : ""}`;
    // Travel only when the SELECTION changed — never in response to level navigation,
    // otherwise "go to root" with a child selected snaps straight back down.
    if (selectionKey === lastSelectionKeyRef.current) return;
    lastSelectionKeyRef.current = selectionKey;
    const homeLevel = selectedEnvironment.parentId ?? "";
    if (homeLevel !== levelId && selectedEnvironment.id !== levelId) setLevelId(homeLevel);
  }, [selectedEnvironment, selected, levelId, scope, viewMode]);

  // The surface is a camera over an infinite plane (owner: "real projection, real
  // surface — not widgets"). ONE transform is the ground: translate = pan, scale =
  // zoom. Zones live at world coordinates on the plane; the camera is only how you look
  // at it. Nothing here steps levels — drilling is explicit (a zone's door / the
  // breadcrumb), so zoom stays a pure continuous scale with no fragile thresholds to
  // fight when real scale lands later.
  const [camera, setCamera] = useState<Camera>(HOME_CAMERA);
  const cameraRef = useRef(camera);
  cameraRef.current = camera;
  const [isPanning, setIsPanning] = useState(false);
  const panRef = useRef<{ pointerId: number; startX: number; startY: number; baseX: number; baseY: number } | null>(null);

  // A new level opens at the home framing; the operator pans/zooms from there.
  useEffect(() => { setCamera(HOME_CAMERA); }, [levelId, viewMode]);

  // Zoom-out-to-return (owner): keep pushing the zoom out past the floor at a child level
  // and the parent frame takes over — the way back UP a level, layered on top of the
  // continuous zoom (not instead of it). At root there is nowhere to return, so it just
  // bottoms out at MIN_SCALE.
  useEffect(() => {
    if (viewMode !== "drilldown") return;
    if (!levelId || camera.scale > STEP_UP_SCALE) return;
    const level = environmentsById.get(levelId);
    setLevelId(level?.parentId ?? "");
  }, [camera.scale, levelId, environmentsById, viewMode]);

  // Wheel = continuous zoom, anchored under the cursor: the world point beneath the
  // pointer stays fixed, so it reads as leaning in, not rescaling a box. Non-passive so
  // the surface owns the gesture instead of scrolling the page.
  useEffect(() => {
    const el = wrapEl;
    if (!el || scope === "World") return;
    function onWheel(event: WheelEvent) {
      event.preventDefault();
      const rect = el!.getBoundingClientRect();
      const px = event.clientX - rect.left;
      const py = event.clientY - rect.top;
      setCamera((cam) => {
        const scale = clampScale(cam.scale * Math.exp(-event.deltaY * 0.0015));
        const k = scale / cam.scale;
        return { x: px - (px - cam.x) * k, y: py - (py - cam.y) * k, scale };
      });
    }
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [wrapEl, scope]);

  function onSurfacePointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    if ((event.target as HTMLElement).closest("[data-topology-interactive='true'], .env-card-holder, .run-node-holder")) return;
    panRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      baseX: cameraRef.current.x,
      baseY: cameraRef.current.y,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
    setIsPanning(true);
  }
  function onSurfacePointerMove(event: ReactPointerEvent<HTMLDivElement>) {
    const pan = panRef.current;
    if (!pan || pan.pointerId !== event.pointerId) return;
    const dx = event.clientX - pan.startX;
    const dy = event.clientY - pan.startY;
    setCamera((cam) => ({ ...cam, x: pan.baseX + dx, y: pan.baseY + dy }));
  }
  function endPan() {
    panRef.current = null;
    setIsPanning(false);
  }
  function onSurfaceDoubleClick(event: ReactMouseEvent<HTMLDivElement>) {
    if ((event.target as HTMLElement).closest("[data-topology-interactive='true'], .env-card-holder, .run-node-holder")) return;
    setCamera(HOME_CAMERA);
  }

  const levelEnvironments = useMemo(
    () => snapshot.environments.filter((env) => (env.parentId ?? "") === levelId),
    [snapshot.environments, levelId],
  );

  const visibleEnvironments = useMemo(() => levelEnvironments.filter((env) => {
    if (!filters.statuses[env.status]) return false;
    if (normalizedQuery && !environmentMatches(env, normalizedQuery)) return false;
    return true;
  }), [filters, normalizedQuery, levelEnvironments]);

  const zonedEnvironments = useMemo(() => {
    if (scope === "World" || viewMode !== "zones") return [];
    const matching = snapshot.environments.filter((env) => {
      if (!filters.statuses[env.status]) return false;
      if (normalizedQuery && !environmentMatches(env, normalizedQuery)) return false;
      return true;
    });
    const byId = new Map(snapshot.environments.map((env) => [env.id, env]));
    const withAncestors = new Map<string, EnvironmentNode>();
    for (const env of matching) {
      let cursor: EnvironmentNode | undefined = env;
      while (cursor) {
        withAncestors.set(cursor.id, cursor);
        cursor = cursor.parentId ? byId.get(cursor.parentId) : undefined;
      }
    }
    return Array.from(withAncestors.values());
  }, [filters, normalizedQuery, snapshot.environments, scope, viewMode]);

  // THIS IS TOPOLOGY, not a list (owner): zones live at REAL coordinates on the surface.
  // Drag a card anywhere; the position pins and persists per level. A card without a pin
  // gets a seeded flow position once, then it is the operator's to move.
  const positionsKey = `octopus.eePos.v1.${levelId || "root"}`;
  const [positions, setPositions] = useState<Record<string, { x: number; y: number }>>({});
  useEffect(() => {
    try {
      const raw = window.localStorage.getItem(positionsKey);
      setPositions(raw ? (JSON.parse(raw) as Record<string, { x: number; y: number }>) : {});
    } catch {
      setPositions({});
    }
  }, [positionsKey]);
  const layoutPositions = useMemo(() => {
    const out: Record<string, { x: number; y: number }> = {};
    visibleEnvironments.forEach((env, index) => {
      out[env.id] = positions[env.id] ?? {
        x: (index % 3) * (CARD_WIDTH + 20) + 4,
        y: Math.floor(index / 3) * 300 + 4,
      };
    });
    return out;
  }, [visibleEnvironments, positions]);
  const cardDragRef = useRef<{
    id: string;
    pointerId: number;
    startX: number;
    startY: number;
    baseX: number;
    baseY: number;
    moved: boolean;
  } | null>(null);

  function onCardPointerDown(env: EnvironmentNode, event: ReactPointerEvent<HTMLDivElement>) {
    if ((event.target as HTMLElement).closest("button, a, input, textarea, select")) return;
    const base = layoutPositions[env.id] ?? { x: 0, y: 0 };
    cardDragRef.current = {
      id: env.id,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      baseX: base.x,
      baseY: base.y,
      moved: false,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function onCardPointerMove(event: ReactPointerEvent<HTMLDivElement>) {
    const drag = cardDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    // Screen deltas divide by the live camera scale → world-space movement.
    const dx = (event.clientX - drag.startX) / cameraRef.current.scale;
    const dy = (event.clientY - drag.startY) / cameraRef.current.scale;
    if (!drag.moved && Math.abs(dx) + Math.abs(dy) < 5) return;
    drag.moved = true;
    setPositions((current) => ({
      ...current,
      [drag.id]: { x: drag.baseX + dx, y: drag.baseY + dy },
    }));
  }

  function onCardPointerUp(env: EnvironmentNode, event: ReactPointerEvent<HTMLDivElement>) {
    const drag = cardDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    cardDragRef.current = null;
    if (drag.moved) {
      setPositions((current) => {
        window.localStorage.setItem(positionsKey, JSON.stringify(current));
        return current;
      });
    } else {
      onSelect({ kind: "environment", id: env.id });
    }
  }

  // Stable identity so a pan (which only moves the camera) never re-renders the cards.
  const enterChildren = useCallback((id: string) => {
    onSelect({ kind: "environment", id });
    setLevelId(id);
  }, [onSelect]);

  const [runPositions, setRunPositions] = useState<Record<string, Point>>({});
  const [zonePaletteAssignments, setZonePaletteAssignments] = useState<Record<string, string>>({});
  useEffect(() => {
    try {
      const rawRuns = window.localStorage.getItem(RUN_POSITIONS_KEY);
      const rawPalette = window.localStorage.getItem(ZONE_PALETTE_KEY);
      setRunPositions(rawRuns ? (JSON.parse(rawRuns) as Record<string, Point>) : {});
      setZonePaletteAssignments(readZonePaletteAssignments(rawPalette));
    } catch {
      setRunPositions({});
      setZonePaletteAssignments({});
    }
  }, []);
  const zoneLayout = useMemo(
    () => buildZoneLayout(zonedEnvironments, runPositions),
    [zonedEnvironments, runPositions],
  );
  const zonePalette = useMemo(
    () => resolveZonePalette(zoneLayout.zones, zonePaletteAssignments),
    [zoneLayout.zones, zonePaletteAssignments],
  );
  const zonePaletteAssignmentsSignature = useMemo(
    () => assignmentSignature(zonePaletteAssignments),
    [zonePaletteAssignments],
  );
  useEffect(() => {
    if (zonePalette.signature === zonePaletteAssignmentsSignature) return;
    setZonePaletteAssignments(zonePalette.assignments);
    window.localStorage.setItem(ZONE_PALETTE_KEY, JSON.stringify({ version: 1, assignments: zonePalette.assignments }));
  }, [zonePalette, zonePaletteAssignmentsSignature]);
  const zoneMaterialOrder: ZonePaint[] = useMemo(() => zoneLayout.zones
    .map((zone, order) => ({ zone, order }))
    .sort((left, right) => left.zone.depth - right.zone.depth || left.order - right.order)
    .map((item, paintIndex) => ({
      ...item,
      maskId: `${zoneMaskPrefix}-zone-mask-${paintIndex}`,
      material: zonePalette.materialsById.get(item.zone.id) ?? ZONE_MATERIALS[0],
    })),
  [zoneLayout.zones, zoneMaskPrefix, zonePalette.materialsById]);
  const materialHoles = useMemo(() => new Map(zoneMaterialOrder.map(({ zone }, index) => [
    zone.id,
    zoneMaterialHoles(zone, zoneMaterialOrder.slice(index + 1).map((item) => item.zone)),
  ])), [zoneMaterialOrder]);
  const zoneLayoutRef = useRef(zoneLayout);
  zoneLayoutRef.current = zoneLayout;
  const runDragRef = useRef<{
    id: string;
    envId: string;
    pointerId: number;
    startX: number;
    startY: number;
    baseX: number;
    baseY: number;
    moved: boolean;
  } | null>(null);

  function onRunPointerDown(node: RunNodeRect, event: ReactPointerEvent<HTMLDivElement>) {
    if ((event.target as HTMLElement).closest("button, a, input, textarea, select")) return;
    runDragRef.current = {
      id: node.run.id,
      envId: node.env.id,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      baseX: node.x,
      baseY: node.y,
      moved: false,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function onRunPointerMove(event: ReactPointerEvent<HTMLDivElement>) {
    const drag = runDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    const zone = zoneLayoutRef.current.zonesById.get(drag.envId);
    if (!zone) return;
    const dx = (event.clientX - drag.startX) / cameraRef.current.scale;
    const dy = (event.clientY - drag.startY) / cameraRef.current.scale;
    if (!drag.moved && Math.abs(dx) + Math.abs(dy) < 5) return;
    drag.moved = true;
    const x = Math.max(zone.x + ZONE_PAD, drag.baseX + dx);
    const y = Math.max(zone.y + ZONE_HEADER + 8, drag.baseY + dy);
    setRunPositions((current) => ({ ...current, [drag.id]: { x: x - zone.x, y: y - zone.y } }));
  }

  function onRunPointerUp(node: RunNodeRect, event: ReactPointerEvent<HTMLDivElement>) {
    const drag = runDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    runDragRef.current = null;
    if (drag.moved) {
      setRunPositions((current) => {
        window.localStorage.setItem(RUN_POSITIONS_KEY, JSON.stringify(current));
        return current;
      });
    } else {
      onSelect({ kind: "run", id: node.run.id });
    }
  }

  const hasRuntimeRows = snapshot.environments.length > 0;
  const registeredWorldInterfaces = useMemo(
    () => Object.values(snapshot.interfaceIndex)
      .filter((iface) => iface.type === "world"),
    [snapshot.interfaceIndex],
  );
  const stats = useMemo(() => {
    if (scope === "World") return snapshot.summaryStats;
    const visibleCount = viewMode === "zones" ? zonedEnvironments.length : visibleEnvironments.length;
    return hasRuntimeRows && visibleCount !== snapshot.environments.length
      ? [`${visibleCount} / ${snapshot.environments.length} EEs`]
      : snapshot.summaryStats;
  }, [hasRuntimeRows, scope, snapshot.environments.length, snapshot.summaryStats, viewMode, visibleEnvironments.length, zonedEnvironments.length]);
  const breadcrumb = useMemo(() => breadcrumbChain(levelId, environmentsById), [levelId, environmentsById]);

  if (snapshot.projection.mode === "connecting") {
    return <OctopusLoadingState className="surface-content-loading-state" label="Loading topology" />;
  }

  if (scope === "World") {
    return (
      <section className="topology-shell is-world-scope" aria-label="World projection">
        <div className="stage-title">
          <Glyph name="topology" size={14} />
          <span>World Projection</span>
          <em>environments below · world surface above</em>
        </div>
        <div className="topology-stage is-world">
          {snapshot.environments.length === 0 && registeredWorldInterfaces.length === 0 ? (
            <EmptyProjection projectionLabel={snapshot.projection.label} projectionDetail={snapshot.projection.detail} />
          ) : (
            <WorldProjection
              environments={snapshot.environments}
              registeredWorldInterfaces={registeredWorldInterfaces}
              interfaceIndex={snapshot.interfaceIndex}
              transferFeed={artifactTransfers.feed}
              mode={viewMode === "zones" ? "routeSets" : "instances"}
              selected={selected}
              onSelect={onSelect}
            />
          )}
        </div>
        <div className="legend-bar">
          <div className="legend-items">
            <span><i className="dot red" />Run</span>
            <span><i className="dot blue" />{viewMode === "zones" ? "Interface route set" : "Interface"}</span>
            <span><i className="line solid" />Interface binding</span>
            <TransferBurstLegend />
          </div>
          <div className="legend-stats">
            {stats.map((item) => <span key={item}>{item}</span>)}
          </div>
        </div>
      </section>
    );
  }

  return (
    <section className="topology-shell" aria-label="Executional Environments">
      <div className="stage-title">
        <Glyph name="ee" size={14} />
        <span>{viewMode === "zones" ? "Runs in EE Zones" : "Run Drilldown"}</span>
        {viewMode === "drilldown" && breadcrumb.length > 0 ? (
          <nav className="stage-breadcrumb" aria-label="Environment breadcrumb">
            <button type="button" onClick={() => setLevelId("")}>root</button>
            {breadcrumb.map((crumb, index) => (
              <span className="stage-breadcrumb-item" key={crumb.id}>
                <i className="crumb-sep" aria-hidden="true">/</i>
                {index === breadcrumb.length - 1 ? (
                  <em className="crumb-current" aria-current="location"><DashboardLabel text={crumb.name} /></em>
                ) : (
                  <button type="button" onClick={() => setLevelId(crumb.id)}><DashboardLabel text={crumb.name} /></button>
                )}
              </span>
            ))}
          </nav>
        ) : null}
      </div>

      <div className={`topology-stage${snapshot.environments.length === 0 || (viewMode === "zones" ? zonedEnvironments.length === 0 : visibleEnvironments.length === 0) ? " is-empty" : ""}`}>
        {snapshot.environments.length === 0 ? (
          <EmptyProjection projectionLabel={snapshot.projection.label} projectionDetail={snapshot.projection.detail} />
        ) : (viewMode === "zones" ? zonedEnvironments.length === 0 : visibleEnvironments.length === 0) ? (
          <EmptyProjection projectionLabel="No EEs in current view" projectionDetail="Adjust search, status filters, or step up a level in the breadcrumb." />
        ) : (
          <>
            <div
              className={`environment-grid-wrap${isPanning ? " is-panning" : ""}`}
              ref={setWrapEl}
              onPointerDown={onSurfacePointerDown}
              onPointerMove={onSurfacePointerMove}
              onPointerUp={endPan}
              onPointerCancel={endPan}
              onDoubleClick={onSurfaceDoubleClick}
            >
              <div
                className="environment-surface-grid"
                style={{
                  backgroundPosition: `${camera.x}px ${camera.y}px`,
                  backgroundSize: `${SURFACE_GRID * camera.scale}px ${SURFACE_GRID * camera.scale}px`,
                }}
              />
              <div
                className="environment-canvas"
                key={levelId}
                style={{
                  transform: `translate(${camera.x}px, ${camera.y}px) scale(${camera.scale})`,
                  width: viewMode === "zones" ? zoneLayout.width : undefined,
                  height: viewMode === "zones" ? zoneLayout.height : undefined,
                }}
              >
                {viewMode === "zones" ? (
                  <>
                    <svg
                      className="ee-zone-materials"
                      width={zoneLayout.width}
                      height={zoneLayout.height}
                      viewBox={`0 0 ${zoneLayout.width} ${zoneLayout.height}`}
                      aria-hidden="true"
                    >
                      <defs>
                        {zoneMaterialOrder.map(({ zone, maskId }) => {
                          const holes = materialHoles.get(zone.id) ?? [];
                          return (
                            <g key={`defs-${zone.id}`}>
                              <mask id={maskId} maskUnits="userSpaceOnUse" x={0} y={0} width={zoneLayout.width} height={zoneLayout.height}>
                                <rect x={zone.x} y={zone.y} width={zone.width} height={zone.height} rx={8} fill="white" />
                                {holes.map((hole) => (
                                  <rect x={hole.x} y={hole.y} width={hole.width} height={hole.height} rx={8} fill="black" key={hole.id} />
                                ))}
                              </mask>
                            </g>
                          );
                        })}
                      </defs>
                      {zoneMaterialOrder.map(({ zone, maskId, material }) => (
                        <g className="ee-zone-material" style={zoneMaterialStyle(material)} mask={`url(#${maskId})`} data-zone-material={material.id} key={`material-${zone.id}`}>
                          <rect className="ee-zone-material-fill" x={zone.x} y={zone.y} width={zone.width} height={zone.height} rx={8} />
                        </g>
                      ))}
                    </svg>
                    {zoneLayout.zones.map((zone) => (
                      <div
                        className="ee-zone-panel"
                        style={{
                          ...zoneMaterialStyle(zonePalette.materialsById.get(zone.id) ?? ZONE_MATERIALS[0]),
                          transform: `translate(${zone.x}px, ${zone.y}px)`,
                          width: zone.width,
                          height: zone.height,
                          zIndex: zone.depth + 1,
                        }}
                        key={zone.id}
                      >
                        <button
                          aria-label={`Open ${zone.name}`}
                          className="ee-zone-head"
                          data-topology-interactive="true"
                          onClick={() => onSelect({ kind: "environment", id: zone.id })}
                          type="button"
                        ><DashboardLabel text={zone.name} /></button>
                      </div>
                    ))}
                    {zoneLayout.runNodes.map((node) => {
                      const isDragged = runDragRef.current?.id === node.run.id && runDragRef.current?.moved;
                      return (
                        <div
                          className={`run-node-holder ${isDragged ? "is-dragging" : ""}`}
                          style={{ transform: `translate(${node.x}px, ${node.y}px)`, width: RUN_NODE_WIDTH }}
                          onPointerDown={(event) => onRunPointerDown(node, event)}
                          onPointerMove={onRunPointerMove}
                          onPointerUp={(event) => onRunPointerUp(node, event)}
                          onPointerCancel={() => { runDragRef.current = null; }}
                          key={node.run.id}
                        >
                          <RunNodeCard
                            run={node.run}
                            env={node.env}
                            selected={selected?.kind === "run" && selected.id === node.run.id}
                            onSelect={onSelect}
                          />
                        </div>
                      );
                    })}
                  </>
                ) : visibleEnvironments.map((env) => {
                  const pos = layoutPositions[env.id];
                  const isDragged = cardDragRef.current?.id === env.id && cardDragRef.current?.moved;
                  return (
                    <div
                      className={`env-card-holder ${isDragged ? "is-dragging" : ""}`}
                      style={{ transform: `translate(${pos.x}px, ${pos.y}px)`, width: CARD_WIDTH }}
                      onPointerDown={(event) => onCardPointerDown(env, event)}
                      onPointerMove={onCardPointerMove}
                      onPointerUp={(event) => onCardPointerUp(env, event)}
                      onPointerCancel={() => { cardDragRef.current = null; }}
                      key={env.id}
                    >
                      <EnvironmentCard
                        env={env}
                        selected={selectionTouchesEnvironment(selected, env)}
                        activeRunId={selected?.kind === "run" ? selected.id : ""}
                        childCount={childCounts.get(env.id) ?? 0}
                        onEnterChildren={enterChildren}
                        onSelect={onSelect}
                      />
                    </div>
                  );
                })}
              </div>
              <SystemArtifactTransfers container={wrapEl} feed={artifactTransfers.feed} interfaces={snapshot.interfaceIndex} />
            </div>
          </>
        )}
      </div>

      <div className="legend-bar">
        <div className="legend-items">
          <span><i className="dot green" />Made checkpoint</span>
          <span><i className="dot gray" />Ephemeral checkpoint</span>
          <span><i className="diamond amber" />Artifact</span>
          <span><i className="diamond purple" />Binding</span>
          <span><i className="dot blue" />Interface</span>
          <span><i className="dot yellow" />In progress</span>
          <span><i className="dot red" />Blocked</span>
          <TransferBurstLegend />
        </div>
        <div className="legend-stats">
          {stats.map((item) => <span key={item}>{item}</span>)}
        </div>
      </div>
    </section>
  );
});

function TransferBurstLegend() {
  return <DashboardTooltip text="Repeated traffic is grouped by run and interface pair. The first movement in each direction anchors its burst; newer repeats join that burst. Related steps keep recorded order with a 300 ms pause. Independent paths animate together. Only recent paths animate; full movement history stays in the ledger.">
    <span>Latest transfer bursts</span>
  </DashboardTooltip>;
}

// The reference card (owner's screenshot): identity, goal, runs with their checkpoint
// trees and binding chips, and the door into child environments.
const EnvironmentCard = memo(function EnvironmentCard({
  env,
  selected,
  activeRunId,
  childCount,
  onEnterChildren,
  onSelect,
}: {
  env: EnvironmentNode;
  selected: boolean;
  activeRunId: string;
  childCount: number;
  onEnterChildren: (id: string) => void;
  onSelect: (selection: SelectionRef) => void;
}) {
  // Expansion is EXPLICIT state per run id — never derived from the selection, so
  // expanding a run can never move anything in a neighbouring zone.
  const [expandedRuns, setExpandedRuns] = useState<Record<string, boolean>>({});
  const activeRuns = env.runs.filter((run) => run.status === "active").length;

  function selectEnvironment() {
    onSelect({ kind: "environment", id: env.id });
  }

  function toggleRun(runId: string) {
    setExpandedRuns((current) => ({ ...current, [runId]: !current[runId] }));
  }

  return (
    <article
      data-env-id={env.id}
      className={`env-card ${selected ? "is-selected" : ""}`}
      tabIndex={0}
      aria-pressed={selected}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          selectEnvironment();
        }
      }}
    >
      <div className="env-head">
        <span className="env-icon"><Glyph name="ee" size={20} /></span>
        <strong><DashboardLabel text={env.name} /></strong>
        {childCount > 0 ? (
          <button
            className="env-child-chip"
            type="button"
            onClick={(event) => {
              event.stopPropagation();
              onEnterChildren(env.id);
            }}
          >
            {childCount} child env{childCount === 1 ? "" : "s"}
          </button>
        ) : null}
      </div>

      <div className="env-goal">
        <span>Goal</span>
        <p>{env.goal}</p>
      </div>

      <div className="env-runs">
        <div className="env-runs-head">
          <span>Runs</span>
          <em>{env.runs.length ? `${activeRuns} active / ${env.runs.length} total` : "none projected"}</em>
        </div>
        {env.runs.map((run) => (
          <RunBlock
            run={run}
            active={run.id === activeRunId}
            expanded={Boolean(expandedRuns[run.id])}
            onToggle={() => toggleRun(run.id)}
            onSelect={onSelect}
            key={run.id}
          />
        ))}
      </div>

      {childCount > 0 ? (
        <button
          className="env-children-row"
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onEnterChildren(env.id);
          }}
        >
          <span>Child environments</span>
          <em>{childCount}</em>
          <i>›</i>
        </button>
      ) : null}
    </article>
  );
});

const RunNodeCard = memo(function RunNodeCard({
  run,
  env,
  selected,
  onSelect,
}: {
  run: RunProjection;
  env: EnvironmentNode;
  selected: boolean;
  onSelect: (selection: SelectionRef) => void;
}) {
  const [showAllCheckpoints, setShowAllCheckpoints] = useState(false);
  const checkpointStrip = run.checkpoints.slice(0, 7);
  const hasMoreCheckpoints = run.checkpoints.length > ZONES_INITIAL_CHECKPOINTS;
  const shownCheckpoints = showAllCheckpoints || !hasMoreCheckpoints
    ? run.checkpoints
    : run.checkpoints.slice(0, ZONES_INITIAL_CHECKPOINTS);
  const checkpointSummary = checkpointListSummary(shownCheckpoints.length, run.checkpoints.length, hasMoreCheckpoints && !showAllCheckpoints);

  return (
    <article
      data-run-id={run.id}
      className={`env-card run-card ${selected ? "is-selected" : ""}`}
      tabIndex={0}
      aria-pressed={selected}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onSelect({ kind: "run", id: run.id });
        }
      }}
    >
      <div className="env-head">
        <span className="env-icon" data-projection-interface={run.worker} data-projection-anchor="worker"><Glyph name="dashboard" size={20} /></span>
        <span className="run-card-title">
          <strong><DashboardLabel text={`${run.worker}.run`} /></strong>
          <small><i className={`status-dot ${runTone(run)}`} />{run.statusLabel} · <DashboardLabel text={env.name} /></small>
        </span>
      </div>

      <div className="run-card-checkpoint-strip" aria-hidden="true">
        {checkpointStrip.map((checkpoint) => (
          <DashboardTooltip text={`${checkpoint.label} · ${titleCase(checkpoint.status)}`} key={checkpoint.id}><span
            className={`checkpoint-dot ${checkpointTone(checkpoint.status) === "green" ? "is-made" : "is-ephemeral"}`}
          /></DashboardTooltip>
        ))}
      </div>

      <div className="env-goal">
        <span>Goal</span>
        <p>{run.goal}</p>
      </div>

      <div className="env-runs">
        <div className="env-runs-head">
          <span>Checkpoints</span>
          <div className="env-runs-head-meta">
            <em>{checkpointSummary}</em>
            {hasMoreCheckpoints ? (
              <button
                className={`env-run-chevron ${showAllCheckpoints ? "is-open" : ""}`}
                type="button"
                aria-expanded={showAllCheckpoints}
                aria-label={`${showAllCheckpoints ? "Collapse" : "Show all"} checkpoints`}
                onClick={(event) => {
                  event.stopPropagation();
                  setShowAllCheckpoints((current) => !current);
                }}
              >
                <Glyph name="chevronDown" size={11} />
              </button>
            ) : null}
          </div>
        </div>
        {shownCheckpoints.map((checkpoint, index) => (
          <CheckpointAsRunBlock
            checkpoint={checkpoint}
            index={index}
            onSelect={onSelect}
            key={checkpoint.id}
          />
        ))}
        {shownCheckpoints.length === 0 ? (
          <div className="env-run">
            <div className="env-run-row">
              <div className="env-run-main is-static">
                <i className="status-dot gray" />
                <strong>No checkpoints yet</strong>
              </div>
            </div>
          </div>
        ) : null}
      </div>

    </article>
  );
});

function CheckpointAsRunBlock({
  checkpoint,
  index,
  onSelect,
}: {
  checkpoint: CheckpointRow;
  index: number;
  onSelect: (selection: SelectionRef) => void;
}) {
  return (
    <div className="env-run">
      <div className="env-run-row">
        <div className="env-run-main is-static">
          <i className={`status-dot ${checkpointTone(checkpoint.status)}`} />
          <strong><DashboardLabel text={checkpoint.label || `Checkpoint ${index + 1}`} /></strong>
          <em className={`env-status-word is-${checkpointTone(checkpoint.status)}`}>{titleCase(checkpoint.status)}</em>
        </div>
        <CheckpointInterface checkpoint={checkpoint} onSelect={onSelect} />
      </div>
      <RequiredVerifiedArtifactList checkpoint={checkpoint} />
    </div>
  );
}

// The checkpoint contract, shown honestly: an authored requirement is "Declared" until the
// ledger proves otherwise — "Closed" only from a recorded mark tick (MarkOutcomeAdmitted),
// "Failed" only from a recorded VerificationFailed movement. The mark is the success tick;
// a failed check is a durable movement, so the row names the requirement whose tick did not
// appear instead of pretending nothing happened.
function RequiredVerifiedArtifactList({ checkpoint }: { checkpoint: CheckpointRow }) {
  const rows = checkpoint.requiredVerifiedArtifacts;
  if (!rows || rows.length === 0) return null;
  return (
    <div className="env-checkpoint-required">
      {rows.map((row) => (
        <div className="env-checkpoint-required-row" key={`${checkpoint.id}:${row.mark}`}>
          <i className={`status-dot ${requiredVerifiedArtifactTone(row.outcome)}`} />
          <strong><DashboardLabel text={row.mark} /></strong>
          {row.verifier ? <small>via <DashboardLabel text={row.verifier} /></small> : null}
          <em className={`env-status-word is-${requiredVerifiedArtifactTone(row.outcome)}`}>{titleCase(row.outcome)}</em>
        </div>
      ))}
    </div>
  );
}

function requiredVerifiedArtifactTone(outcome: "declared" | "closed" | "failed"): string {
  if (outcome === "closed") return "green";
  if (outcome === "failed") return "red";
  return "gray";
}

function checkpointListSummary(visibleCount: number, madeCount: number, isTruncated: boolean): string {
  if (madeCount === 0) return "none yet";
  return isTruncated ? `${visibleCount} / ${madeCount} checkpoints` : `${madeCount} checkpoints`;
}

function RunBlock({
  run,
  active,
  expanded,
  onToggle,
  onSelect,
}: {
  run: RunProjection;
  active: boolean;
  expanded: boolean;
  onToggle: () => void;
  onSelect: (selection: SelectionRef) => void;
}) {
  return (
    <div className={`env-run ${active ? "is-active" : ""}`} data-run-id={run.id}>
      <div className="env-run-row">
        <button
          className="env-run-main"
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onSelect({ kind: "run", id: run.id });
          }}
        >
          <i className={`status-dot ${runTone(run)}`} />
          <strong data-projection-interface={run.worker} data-projection-anchor="worker"><DashboardLabel text={`${run.worker}.run`} /></strong>
          <em className={`env-status-word is-${runTone(run)}`}>{run.statusLabel}</em>
        </button>
        {run.checkpoints.length ? (
          <button
            className={`env-run-chevron ${expanded ? "is-open" : ""}`}
            type="button"
            aria-expanded={expanded}
            aria-label={`${expanded ? "Collapse" : "Expand"} checkpoints`}
            onClick={(event) => {
              event.stopPropagation();
              onToggle();
            }}
          >
            <Glyph name="chevronDown" size={11} />
          </button>
        ) : null}
      </div>

      {run.checkpoints.length ? (
        <div className={`env-checkpoints-shell ${expanded ? "is-open" : ""}`}>
          <div className="env-checkpoints">
            {run.checkpoints.map((checkpoint, index) => (
              <CheckpointBlock checkpoint={checkpoint} index={index} projectionVisible={expanded} onSelect={onSelect} key={checkpoint.id} />
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function CheckpointBlock({ checkpoint, index, projectionVisible, onSelect }: { checkpoint: CheckpointRow; index: number; projectionVisible: boolean; onSelect: (selection: SelectionRef) => void }) {
  const artifactCount = checkpoint.artifactBindings ?? dedupeNonEmpty([checkpoint.artifactLabel, checkpoint.tool]).length;
  const interfaceCount = checkpoint.interfaceBindings ?? dedupeNonEmpty([checkpoint.interfaceName]).length;
  return (
    <div className="env-checkpoint">
      <div className="env-checkpoint-head">
        <i className="env-checkpoint-mark" aria-hidden="true" />
        <strong>{`Checkpoint ${index + 1}`}</strong>
        <em className={`env-status-word is-${checkpointTone(checkpoint.status)}`}>{titleCase(checkpoint.status)}</em>
        <CheckpointInterface checkpoint={checkpoint} onSelect={onSelect} visible={projectionVisible} />
      </div>
      <div className="env-checkpoint-chips">
        <span>Artifact bindings <b>{artifactCount}</b></span>
        <span>Interface bindings <b>{interfaceCount}</b></span>
      </div>
      <RequiredVerifiedArtifactList checkpoint={checkpoint} />
    </div>
  );
}

function CheckpointInterface({ checkpoint, onSelect, visible = true }: { checkpoint: CheckpointRow; onSelect: (selection: SelectionRef) => void; visible?: boolean }) {
  if (!checkpoint.interfaceName) return null;
  return (
    <DashboardTooltip text={`Interface: ${checkpoint.interfaceName}`}><button
      className="env-checkpoint-interface"
      data-projection-interface={visible ? checkpoint.interfaceName : undefined}
      data-projection-checkpoint={checkpoint.id}
      type="button"
      aria-label={`Open interface ${checkpoint.interfaceName}`}
      onClick={(event) => {
        event.stopPropagation();
        onSelect({ kind: "interface", id: checkpoint.interfaceName! });
      }}
    >
      <i className="status-dot blue" />
    </button></DashboardTooltip>
  );
}

function runTone(run: RunProjection): string {
  if (run.status === "active") return "green";
  if (run.status === "idle") return "gray";
  if (run.status === "done") return "green";
  if (run.status === "blocked") return "red";
  if (run.status === "stale") return "amber";
  return "gray";
}

function checkpointTone(status: string): string {
  const text = status.toLowerCase();
  if (text === "made") return "green";
  return "gray";
}

function titleCase(value: string): string {
  const text = value.trim();
  if (!text) return "Projected";
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function dedupeNonEmpty(values: Array<string | undefined>): string[] {
  return Array.from(new Set(values.map((value) => (value ?? "").trim()).filter(Boolean)));
}

function readZonePaletteAssignments(raw: string | null): Record<string, string> {
  if (!raw) return {};
  const parsed: unknown = JSON.parse(raw);
  if (!isRecord(parsed) || parsed.version !== 1 || !isRecord(parsed.assignments)) return {};
  const assignments: Record<string, string> = {};
  for (const [zoneId, materialId] of Object.entries(parsed.assignments)) {
    if (typeof materialId === "string" && ZONE_MATERIALS_BY_ID.has(materialId)) assignments[zoneId] = materialId;
  }
  return assignments;
}

function resolveZonePalette(zones: ZoneRect[], storedAssignments: Record<string, string>): ZonePaletteResult {
  const sanitizedStored: Record<string, string> = {};
  for (const [zoneId, materialId] of Object.entries(storedAssignments)) {
    if (ZONE_MATERIALS_BY_ID.has(materialId)) sanitizedStored[zoneId] = materialId;
  }
  const conflictsByZone = buildZoneConflictMap(zones);
  const assignedVisible: Record<string, string> = {};
  const usage = new Map<string, number>();
  const paintOrder = zones
    .map((zone, order) => ({ zone, order }))
    .sort((left, right) => {
      const depthDelta = left.zone.depth - right.zone.depth;
      if (depthDelta !== 0) return depthDelta;
      const conflictDelta = (conflictsByZone.get(right.zone.id)?.size ?? 0) - (conflictsByZone.get(left.zone.id)?.size ?? 0);
      if (conflictDelta !== 0) return conflictDelta;
      return left.order - right.order;
    });

  for (const { zone } of paintOrder) {
    const conflictIds = conflictsByZone.get(zone.id) ?? new Set<string>();
    const forbidden = new Set<string>();
    for (const conflictId of conflictIds) {
      const conflictMaterial = assignedVisible[conflictId];
      if (conflictMaterial) forbidden.add(conflictMaterial);
    }
    const stored = sanitizedStored[zone.id];
    const materialId = stored && !forbidden.has(stored)
      ? stored
      : chooseZoneMaterial(zone, conflictIds, assignedVisible, usage);
    assignedVisible[zone.id] = materialId;
    usage.set(materialId, (usage.get(materialId) ?? 0) + 1);
  }

  const assignments = { ...sanitizedStored, ...assignedVisible };
  const materialsById = new Map<string, ZoneMaterial>();
  for (const zone of zones) {
    materialsById.set(zone.id, ZONE_MATERIALS_BY_ID.get(assignments[zone.id]) ?? ZONE_MATERIALS[0]);
  }
  return { assignments, materialsById, signature: assignmentSignature(assignments) };
}

function chooseZoneMaterial(
  zone: ZoneRect,
  conflictIds: Set<string>,
  assignedVisible: Record<string, string>,
  usage: Map<string, number>,
): string {
  let best = ZONE_MATERIALS[0];
  let bestScore = Number.NEGATIVE_INFINITY;
  for (const candidate of ZONE_MATERIALS) {
    let score = seededUnit(`${zone.id}:${candidate.id}`) * 6 - (usage.get(candidate.id) ?? 0) * 14;
    for (const conflictId of conflictIds) {
      const neighborMaterialId = assignedVisible[conflictId];
      if (!neighborMaterialId) continue;
      const neighbor = ZONE_MATERIALS_BY_ID.get(neighborMaterialId);
      if (!neighbor) continue;
      score += rgbDistance(candidate.rgb, neighbor.rgb) / 18;
      if (neighbor.id === candidate.id) score -= 10000;
    }
    if (score > bestScore) {
      best = candidate;
      bestScore = score;
    }
  }
  return best.id;
}

function buildZoneConflictMap(zones: ZoneRect[]): Map<string, Set<string>> {
  const conflicts = new Map(zones.map((zone) => [zone.id, new Set<string>()]));
  for (let leftIndex = 0; leftIndex < zones.length; leftIndex += 1) {
    for (let rightIndex = leftIndex + 1; rightIndex < zones.length; rightIndex += 1) {
      const left = zones[leftIndex];
      const right = zones[rightIndex];
      if (!zonesConflict(left, right)) continue;
      conflicts.get(left.id)?.add(right.id);
      conflicts.get(right.id)?.add(left.id);
    }
  }
  return conflicts;
}

function zonesConflict(left: ZoneRect, right: ZoneRect): boolean {
  const leftParent = left.parentId ?? "";
  const rightParent = right.parentId ?? "";
  return left.parentId === right.id
    || right.parentId === left.id
    || leftParent === rightParent
    || rectsOverlap(left, right)
    || rectDistance(left, right) < ZONE_GAP * 2;
}

function rectDistance(left: ZoneRect, right: ZoneRect): number {
  const dx = Math.max(left.x - (right.x + right.width), right.x - (left.x + left.width), 0);
  const dy = Math.max(left.y - (right.y + right.height), right.y - (left.y + left.height), 0);
  return Math.hypot(dx, dy);
}

function assignmentSignature(assignments: Record<string, string>): string {
  return Object.entries(assignments)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([zoneId, materialId]) => `${zoneId}:${materialId}`)
    .join("|");
}

function zoneMaterialStyle(material: ZoneMaterial): CSSProperties & Record<"--zone-rgb" | "--zone-edge-rgb" | "--zone-label-rgb", string> {
  return {
    "--zone-rgb": rgbVar(material.rgb),
    "--zone-edge-rgb": rgbVar(material.edgeRgb),
    "--zone-label-rgb": rgbVar(material.labelRgb),
  };
}

function rgbVar(rgb: Rgb): string {
  return `${rgb[0]} ${rgb[1]} ${rgb[2]}`;
}

function rgbDistance(left: Rgb, right: Rgb): number {
  return Math.hypot(left[0] - right[0], left[1] - right[1], left[2] - right[2]);
}

function seededUnit(value: string): number {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0) / 4294967295;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function fallbackRunPosition(index: number): Point {
  return {
    x: ZONE_PAD + (index % 2) * (RUN_NODE_WIDTH + 18),
    y: ZONE_HEADER + 16 + Math.floor(index / 2) * (RUN_NODE_HEIGHT + 16),
  };
}

function runBoundsForEnvironment(env: EnvironmentNode, runPositions: Record<string, Point>): { right: number; bottom: number } {
  let right = 0;
  let bottom = 0;
  env.runs.forEach((run, index) => {
    const position = runPositions[run.id] ?? fallbackRunPosition(index);
    right = Math.max(right, position.x + RUN_NODE_WIDTH);
    bottom = Math.max(bottom, position.y + RUN_NODE_HEIGHT);
  });
  return { right, bottom };
}

function buildZoneLayout(environments: EnvironmentNode[], runPositions: Record<string, Point>): ZoneLayout {
  const byId = new Map(environments.map((env) => [env.id, env]));
  const childrenByParent = new Map<string, EnvironmentNode[]>();
  for (const env of environments) {
    const parent = env.parentId && byId.has(env.parentId) ? env.parentId : "";
    const children = childrenByParent.get(parent) ?? [];
    children.push(env);
    childrenByParent.set(parent, children);
  }

  const measureCache = new Map<string, ZoneSize>();
  function measure(env: EnvironmentNode): ZoneSize {
    const cached = measureCache.get(env.id);
    if (cached) return cached;
    const children = childrenByParent.get(env.id) ?? [];
    const runBounds = runBoundsForEnvironment(env, runPositions);
    const runWidth = env.runs.length === 0 ? 0 : runBounds.right + ZONE_PAD;
    const runHeight = env.runs.length === 0 ? 0 : runBounds.bottom;
    let childWidth = 0;
    let childHeight = 0;
    if (children.length > 0) {
      const childMeasures = children.map(measure);
      const childColumns = Math.min(2, children.length);
      const rowWidths: number[] = [];
      const rowHeights: number[] = [];
      childMeasures.forEach((child, index) => {
        const row = Math.floor(index / childColumns);
        rowWidths[row] = (rowWidths[row] ?? 0) + child.width + (index % childColumns === 0 ? 0 : ZONE_GAP);
        rowHeights[row] = Math.max(rowHeights[row] ?? 0, child.height);
      });
      childWidth = ZONE_PAD * 2 + Math.max(...rowWidths);
      childHeight = 30 + rowHeights.reduce((total, height) => total + height, 0) + Math.max(0, rowHeights.length - 1) * ZONE_GAP;
    }
    const isEmptyZone = env.runs.length === 0 && children.length === 0;
    const size = {
      width: Math.max(ZONE_MIN_WIDTH, runWidth, childWidth),
      height: Math.max(isEmptyZone ? 122 : 230, runHeight + childHeight + ZONE_PAD),
    };
    measureCache.set(env.id, size);
    return size;
  }

  const zones: ZoneRect[] = [];
  const runNodes: RunNodeRect[] = [];
  let maxRight = 0;
  let maxBottom = 0;

  function place(env: EnvironmentNode, x: number, y: number, depth: number) {
    const size = measure(env);
    const children = childrenByParent.get(env.id) ?? [];
    zones.push({
      id: env.id,
      name: env.name,
      parentId: env.parentId && byId.has(env.parentId) ? env.parentId : undefined,
      depth,
      x,
      y,
      width: size.width,
      height: size.height,
      accent: env.accent,
      statusLabel: env.statusLabel,
      runCount: env.runs.length,
      childCount: children.length,
    });
    maxRight = Math.max(maxRight, x + size.width);
    maxBottom = Math.max(maxBottom, y + size.height);

    env.runs.forEach((run, index) => {
      const relative = runPositions[run.id] ?? fallbackRunPosition(index);
      runNodes.push({ run, env, x: x + relative.x, y: y + relative.y });
    });

    if (children.length === 0) return;
    const childColumns = Math.min(2, children.length);
    const runBounds = runBoundsForEnvironment(env, runPositions);
    let childY = y + (env.runs.length ? runBounds.bottom + 28 : ZONE_HEADER + 18);
    for (let index = 0; index < children.length; index += childColumns) {
      const row = children.slice(index, index + childColumns);
      let childX = x + ZONE_PAD;
      let rowHeight = 0;
      for (const child of row) {
        const childSize = measure(child);
        place(child, childX, childY, depth + 1);
        childX += childSize.width + ZONE_GAP;
        rowHeight = Math.max(rowHeight, childSize.height);
      }
      childY += rowHeight + ZONE_GAP;
    }
  }

  const roots = [...(childrenByParent.get("") ?? [])].sort((left, right) => (
    ((right.runs.length + (childrenByParent.get(right.id)?.length ?? 0)) - (left.runs.length + (childrenByParent.get(left.id)?.length ?? 0)))
  ));
  let rootX = 36;
  let rootY = 52;
  const rootColumns = Math.min(2, Math.max(1, roots.length));
  for (let index = 0; index < roots.length; index += rootColumns) {
    const row = roots.slice(index, index + rootColumns);
    rootX = 36;
    let rowHeight = 0;
    for (const root of row) {
      const size = measure(root);
      place(root, rootX, rootY, 0);
      rootX += size.width + ZONE_GAP;
      rowHeight = Math.max(rowHeight, size.height);
    }
    rootY += rowHeight + ZONE_GAP;
  }

  return {
    zones,
    zonesById: new Map(zones.map((zone) => [zone.id, zone])),
    runNodes,
    width: maxRight + 160,
    height: maxBottom + 160,
  };
}

function zoneMaterialHoles(zone: ZoneRect, laterZones: ZoneRect[]): ZoneRect[] {
  const overlaps = laterZones.filter((item) => rectsOverlap(zone, item));
  return overlaps.filter((candidate) => !overlaps.some((other) => other.id !== candidate.id && rectContains(other, candidate)));
}

function rectsOverlap(left: ZoneRect, right: ZoneRect): boolean {
  return left.x < right.x + right.width
    && left.x + left.width > right.x
    && left.y < right.y + right.height
    && left.y + left.height > right.y;
}

function rectContains(outer: ZoneRect, inner: ZoneRect): boolean {
  return outer.x <= inner.x
    && outer.y <= inner.y
    && outer.x + outer.width >= inner.x + inner.width
    && outer.y + outer.height >= inner.y + inner.height;
}

function EmptyProjection({ projectionLabel, projectionDetail }: { projectionLabel: string; projectionDetail: string }) {
  return (
    <div className="empty-projection">
      <Glyph name="ee" size={24} />
      <strong>{projectionLabel}</strong>
      <span>{projectionDetail}</span>
    </div>
  );
}

function breadcrumbChain(levelId: string, environmentsById: Map<string, EnvironmentNode>): Array<{ id: string; name: string }> {
  const chain: Array<{ id: string; name: string }> = [];
  let cursor = levelId;
  while (cursor) {
    const env = environmentsById.get(cursor);
    if (!env) break;
    chain.unshift({ id: env.id, name: env.name });
    cursor = env.parentId ?? "";
  }
  return chain;
}

function environmentMatches(env: EnvironmentNode, query: string): boolean {
  const haystack = [
    env.name,
    env.statusLabel,
    env.goal,
    env.description,
    ...env.worldInterfaces,
    ...env.stats.flatMap((stat) => [stat.label, stat.value]),
    ...env.interfaces.flatMap((item) => [item.name, item.description, item.tag, item.status]),
    ...env.bindings.flatMap((item) => [item.name, item.description, item.tag, item.status]),
    ...env.runs.flatMap((run) => [
      run.id,
      run.worker,
      run.goal,
      ...run.workerEnvironments.flatMap((workerEnvironment) => [
        workerEnvironment.id,
        workerEnvironment.label,
        workerEnvironment.kind,
        workerEnvironment.root,
        ...workerEnvironment.surfaces.flatMap((surface) => [surface.id, surface.label, surface.protocol, surface.root]),
      ]),
    ]),
    ...env.eventLog.flatMap((row) => [row.label, row.detail ?? "", row.kind ?? ""]),
  ];
  return haystack.some((value) => value.toLowerCase().includes(query));
}

function selectionTouchesEnvironment(selection: SelectionRef | null, env: EnvironmentNode): boolean {
  if (!selection) return false;
  if (selection.kind === "environment") return selection.id === env.id;
  return env.runs.some((run) => run.id === selection.id);
}
