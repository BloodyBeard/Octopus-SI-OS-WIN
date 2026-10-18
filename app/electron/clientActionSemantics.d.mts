export type WidgetAction = { schema: "octopus.client_action"; schema_version: 1; request_id: string } & (
  { action: "list_widgets" }
  | { action: "set_widget"; widget_id: string; source_digest: string; state: "visible" | "hidden" | "removed" }
);
export type ClientAction = WidgetAction | {
  schema: "octopus.client_action"; schema_version: 1; action: "open_surface"; surface_id: string;
};
export function parseClientAction(value: unknown): ClientAction;
