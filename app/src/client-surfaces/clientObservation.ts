import type { OctopusClientContext } from "../electron";
import type { ClientSurfaceDescriptor } from "../shell/surfaceTypes";
import { readHomeWidgetInventory, type HomeWidgetControls } from "./homeWidgetControls";

export const CLIENT_OBSERVATION_BYTES = 4 * 1024;

export function clientObservationContext(options: HomeWidgetControls & {
  availableSurfaces: readonly ClientSurfaceDescriptor[];
  ready: boolean;
}): OctopusClientContext {
  const surfaces = options.availableSurfaces.map(surface => ({
    surface_id: surface.id,
    title: surface.label,
    source_digest: surface.document?.sourceDigest ?? options.extensions.extensions.find(extension =>
      extension.manifest.id === surface.extension?.extensionId)?.sourceDigest ?? null,
    available: true,
  }));
  let widgets: ReturnType<typeof readHomeWidgetInventory> = [];
  let widgetsStatus = "ready";
  if (!options.ready || !options.extensions.hydrated || !options.runId || !options.interfaceId) {
    widgetsStatus = "unavailable";
  } else {
    try { widgets = readHomeWidgetInventory(options); }
    catch { widgetsStatus = "unavailable"; }
  }
  const snapshot = {
    schema: "octopus.client_observation", schema_version: 1, authority: "observation_only",
    surfaces: [] as typeof surfaces, widgets: [] as typeof widgets,
    widgets_status: widgetsStatus, omitted_surfaces: surfaces.length, omitted_widgets: widgets.length,
  };
  const size = () => new TextEncoder().encode(JSON.stringify(snapshot)).length;
  for (let index = 0; index < Math.max(surfaces.length, widgets.length); index += 1) {
    if (index < widgets.length) {
      snapshot.widgets.push(widgets[index]); snapshot.omitted_widgets -= 1;
      if (size() > CLIENT_OBSERVATION_BYTES) { snapshot.widgets.pop(); snapshot.omitted_widgets += 1; }
    }
    if (index < surfaces.length) {
      snapshot.surfaces.push(surfaces[index]); snapshot.omitted_surfaces -= 1;
      if (size() > CLIENT_OBSERVATION_BYTES) { snapshot.surfaces.pop(); snapshot.omitted_surfaces += 1; }
    }
  }
  return [{ name: "client_observation", text: JSON.stringify(snapshot) }];
}
