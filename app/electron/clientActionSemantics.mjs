export function parseClientAction(value) {
  const object = value && typeof value === "object" && !Array.isArray(value);
  const text = value => typeof value === "string" && value.length > 0 && value.length <= 256 && value.trim() === value;
  if (!object || value.schema !== "octopus.client_action" || value.schema_version !== 1) {
    throw new Error("Client action requires exact octopus.client_action v1 data.");
  }
  if (value.action === "open_surface" && Object.keys(value).length === 4 && text(value.surface_id)) return value;
  if (value.action === "list_widgets" && Object.keys(value).length === 4 && text(value.request_id)) return value;
  if (value.action === "set_widget" && Object.keys(value).length === 7 && text(value.request_id)
    && text(value.widget_id) && text(value.source_digest) && ["visible", "hidden", "removed"].includes(value.state)) return value;
  if (value.action === "inspect_client" && text(value.request_id) && inspectionScope(value)) return value;
  if (value.action === "relaunch_client" && Object.keys(value).length === 4 && text(value.request_id)) return value;
  throw new Error("Client action must be an exact open_surface, list_widgets, set_widget, inspect_client or relaunch_client request.");
}

function inspectionScope(value) {
  const record = candidate => candidate && typeof candidate === "object" && !Array.isArray(candidate);
  if (Object.keys(value).some(key => !["schema", "schema_version", "action", "request_id", "extension_id", "source"].includes(key))) return false;
  if (Object.hasOwn(value, "extension_id") && !/^[a-z0-9][a-z0-9._-]{1,63}$/.test(String(value.extension_id))) return false;
  if (!Object.hasOwn(value, "source")) return true;
  const source = value.source;
  if (!record(source) || Object.keys(source).some(key => !["method", "params", "path"].includes(key))) return false;
  if (typeof source.method !== "string" || !/^projection\.[a-z0-9_.]{1,96}$/.test(source.method)) return false;
  if (Object.hasOwn(source, "params") && (!record(source.params) || JSON.stringify(source.params).length > 16_384)) return false;
  return !Object.hasOwn(source, "path") || typeof source.path === "string" && source.path.length > 0 && source.path.length <= 256;
}
