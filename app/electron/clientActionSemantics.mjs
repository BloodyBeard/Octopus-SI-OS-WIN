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
  throw new Error("Client action must be an exact open_surface, list_widgets or set_widget request.");
}
