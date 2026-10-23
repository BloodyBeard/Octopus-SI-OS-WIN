import type { WidgetAction } from "../../electron/clientActionSemantics.mjs";
import type { ClientExtensionRegistryState } from "../extensions/types";
import { materializeClientExtensions } from "../extensions/useClientExtensions";
import { readClientExtensions } from "../extensions/transport";
import { homeSurfaceIdentity, selectHomeSurfaceDocuments } from "./homePresentation";
import type { AdmittedClientSurfaceDocument } from "./types";

export const homeWidgetHiddenKey = (profile: string) => `octopus.voice.hidden-surfaces.v1.${profile}`;
export const homeWidgetDismissalsKey = (profile: string, runId: string, interfaceId: string) =>
  `octopus.home.surface-dismissals.v1:${JSON.stringify([profile, runId, interfaceId])}`;
export const removedHomeWidgetIdentity = (identity: string) => `removed:${identity}`;

function strings(key: string): Set<string> | null {
  const raw = window.localStorage.getItem(key);
  if (raw === null) return null;
  const values: unknown = JSON.parse(raw);
  if (!Array.isArray(values) || values.length > 512 || values.some(value => typeof value !== "string" || value.length > 4096)) {
    throw new Error("Saved Home widget visibility is malformed.");
  }
  return new Set(values as string[]);
}

export function saveHomeWidgetStrings(key: string, values: ReadonlySet<string>): void {
  if (values.size > 512) throw new Error("Too many saved Home widget visibility entries.");
  const value = JSON.stringify([...values]);
  window.localStorage.setItem(key, value);
  if (window.localStorage.getItem(key) !== value) throw new Error("Home widget visibility was not saved.");
  window.dispatchEvent(new StorageEvent("storage", { key, newValue: value }));
}

export type HomeWidgetControls = {
  accessProfile: "live" | "lab";
  runId: string;
  interfaceId: string;
  surfaces: AdmittedClientSurfaceDocument[];
  extensions: ClientExtensionRegistryState;
};

function inventory(options: HomeWidgetControls) {
  const hidden = strings(homeWidgetHiddenKey(options.accessProfile));
  const dismissed = strings(homeWidgetDismissalsKey(options.accessProfile, options.runId, options.interfaceId)) ?? new Set<string>();
  const selected = selectHomeSurfaceDocuments(options.surfaces.filter(item => !item.sourceExtensionId), options, Date.now());
  if (selected.errors.length) throw new Error(selected.errors.join("; "));
  const rows = options.extensions.extensions.filter(extension => extension.manifest.category === "widget").map(extension => {
    const contribution = extension.manifest.contribution;
    const widgetId = contribution.kind === "builtin" ? contribution.surface_id : extension.surface?.document.id;
    if (!widgetId) throw new Error(`Widget ${extension.manifest.name} has no admitted document.`);
    return {
      widget_id: widgetId, title: extension.manifest.name, source_digest: extension.sourceDigest,
      origin: contribution.kind === "builtin" ? "builtin" : "extension",
      state: !extension.installed ? "removed" : hidden === null || hidden.has(widgetId) ? "hidden" : "visible",
      installed: extension.installed, available: extension.configured && (extension.admission === "builtin" || extension.admission === "reviewed"),
      configured: extension.configured, review: extension.admission,
      extensionId: extension.manifest.id, item: undefined as AdmittedClientSurfaceDocument | undefined,
    };
  });
  const published = selected.documents.map(item => {
    const identity = homeSurfaceIdentity(item);
    return {
      widget_id: item.document.id, title: item.document.title, source_digest: item.sourceDigest, origin: "published",
      state: dismissed.has(removedHomeWidgetIdentity(identity)) ? "removed" : dismissed.has(identity) ? "hidden" : "visible",
      installed: false, available: true, extensionId: "", item,
      configured: true, review: "admitted",
    };
  });
  return [...rows, ...published];
}

export function readHomeWidgetInventory(options: HomeWidgetControls) {
  if (!options.runId || !options.interfaceId || !options.extensions.hydrated) {
    throw new Error("The selected Home widget registry is not ready.");
  }
  return inventory(options).map(({ extensionId: _extensionId, item: _item, ...row }) => row);
}

export async function applyHomeWidgetAction(options: HomeWidgetControls, action: WidgetAction, operationId: string) {
  if (options.accessProfile !== "live" || !options.runId || !options.interfaceId || !options.extensions.hydrated) {
    throw new Error("The selected Home widget registry is not ready.");
  }
  let current = options;
  const changes = action.action === "set_widget" ? action.changes : [];
  const before = inventory(options);
  const widgets = changes.map((change) => {
    const matches = before.filter(item => item.widget_id === change.widget_id);
    if (matches.length !== 1) throw new Error(`Widget ${change.widget_id} is unavailable or ambiguous; list widgets again.`);
    if (matches[0].source_digest !== change.source_digest) throw new Error(`Widget ${change.widget_id} revision changed; list widgets again before changing it.`);
    if (change.state === "visible" && matches[0].extensionId && !matches[0].available) {
      throw new Error(`Widget ${change.widget_id} requires source configuration or extension review before it can be enabled.`);
    }
    return { change, widget: matches[0] };
  });
  for (const { change, widget } of widgets) {
    if (widget.extensionId) {
      if (change.state === "visible" && !widget.installed) await options.extensions.install(widget.extensionId, operationId);
      if (change.state === "removed" && widget.installed) await options.extensions.uninstall(widget.extensionId, operationId);
      const refreshed = materializeClientExtensions(await readClientExtensions(), "live", "");
      current = { ...options, extensions: { ...options.extensions, ...refreshed } };
      const key = homeWidgetHiddenKey(options.accessProfile);
      const hidden = strings(key) ?? new Set(inventory(current).filter(item => item.installed).map(item => item.widget_id));
      if (change.state === "visible") hidden.delete(widget.widget_id); else hidden.add(widget.widget_id);
      saveHomeWidgetStrings(key, hidden);
    } else {
      if (!widget.item) throw new Error("Published widget lost its delivery identity.");
      const identity = homeSurfaceIdentity(widget.item);
      const key = homeWidgetDismissalsKey(options.accessProfile, options.runId, options.interfaceId);
      const dismissed = strings(key) ?? new Set<string>();
      dismissed.delete(identity);
      dismissed.delete(removedHomeWidgetIdentity(identity));
      if (change.state === "hidden") dismissed.add(identity);
      if (change.state === "removed") dismissed.add(removedHomeWidgetIdentity(identity));
      saveHomeWidgetStrings(key, dismissed);
    }
  }
  const rows = inventory(current);
  for (const change of changes) {
    if (!rows.some(row => row.widget_id === change.widget_id && row.source_digest === change.source_digest && row.state === change.state)) {
      throw new Error(`Widget ${change.widget_id} changed before its requested presentation state could be confirmed.`);
    }
  }
  return rows.map(({ extensionId: _extensionId, item: _item, ...row }) => row);
}
