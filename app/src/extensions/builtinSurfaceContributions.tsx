import { lazy, type ComponentType } from "react";
import type { ClientSurfaceDescriptor, SurfaceChrome } from "../shell/surfaceTypes";

const MetricsView = lazy(() => import("../components/MetricsView").then((module) => ({ default: module.MetricsView })));
const UsageView = lazy(() => import("../components/UsageView").then((module) => ({ default: module.UsageView })));

type BuiltinSurfaceContext = {
  query: string;
  searchFocusTick: number;
};

function MetricsExtensionSurface({ query }: BuiltinSurfaceContext) {
  return <MetricsView query={query} />;
}

function UsageExtensionSurface({ query }: BuiltinSurfaceContext) {
  return <UsageView query={query} />;
}

type BuiltinSurfaceContribution = {
  surfaceId: string;
  label: string;
  description: string;
  glyph: ClientSurfaceDescriptor["glyph"];
  order: number;
  placements: ClientSurfaceDescriptor["placements"];
  chrome: SurfaceChrome;
  hostClassName?: string;
  component: ComponentType<BuiltinSurfaceContext>;
};

const CONTRIBUTIONS = [
  {
    surfaceId: "metrics",
    label: "Metrics",
    description: "Read-only activity and distribution lenses.",
    glyph: "metrics",
    order: 45,
    placements: ["shell"],
    chrome: {
      topbar: "wide",
      workspace: "panel",
      titlebarGlyph: "metrics",
      searchPlaceholder: "Search metric rows...",
    },
    hostClassName: "is-metrics",
    component: MetricsExtensionSurface,
  },
  {
    surfaceId: "usage",
    label: "Usage",
    description: "Local provider token activity and API-equivalent cost.",
    glyph: "dollar",
    order: 46,
    placements: ["shell"],
    chrome: {
      topbar: "wide",
      workspace: "panel",
      searchPlaceholder: "Search usage rows...",
    },
    hostClassName: "is-usage",
    component: UsageExtensionSurface,
  },
] satisfies BuiltinSurfaceContribution[];

const BY_SURFACE_ID = new Map(CONTRIBUTIONS.map((contribution) => [contribution.surfaceId, contribution]));

export const builtinClientExtensionSurfaceIds = CONTRIBUTIONS.map((contribution) => contribution.surfaceId);

export function hasBuiltinClientExtensionSurface(surfaceId: string): boolean {
  return BY_SURFACE_ID.has(surfaceId);
}

export function materializeBuiltinClientExtensionSurface(
  extensionId: string,
  surfaceId: string,
): ClientSurfaceDescriptor | null {
  const contribution = BY_SURFACE_ID.get(surfaceId);
  if (!contribution) return null;
  return {
    id: contribution.surfaceId,
    label: contribution.label,
    description: contribution.description,
    glyph: contribution.glyph,
    rendererKind: "extension.builtin",
    order: contribution.order,
    source: "extension",
    placements: contribution.placements,
    chrome: contribution.chrome,
    hostClassName: contribution.hostClassName,
    extension: { extensionId, surfaceId },
  };
}

export function BuiltinClientExtensionSurface({
  query,
  searchFocusTick,
  surfaceId,
}: BuiltinSurfaceContext & { surfaceId: string }) {
  const contribution = BY_SURFACE_ID.get(surfaceId);
  if (!contribution) throw new Error(`built-in client extension surface "${surfaceId}" is not registered`);
  const Component = contribution.component;
  return <Component query={query} searchFocusTick={searchFocusTick} />;
}
