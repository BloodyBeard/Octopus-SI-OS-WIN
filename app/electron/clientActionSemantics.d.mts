export type WidgetAction = { schema: "octopus.client_action"; schema_version: 1; request_id: string } & (
  { action: "list_widgets" }
  | { action: "set_widget"; changes: Array<{ widget_id: string; source_digest: string; state: "visible" | "hidden" | "removed" }> }
);
export type InspectClientAction = {
  schema: "octopus.client_action"; schema_version: 1; request_id: string; action: "inspect_client"; extension_id?: string;
  source?: { method: string; params?: Record<string, unknown>; path?: string };
};
export type RelaunchClientAction = { schema: "octopus.client_action"; schema_version: 1; request_id: string; action: "relaunch_client" };
export type ClientResultAction = WidgetAction | InspectClientAction | RelaunchClientAction;
export type ClientAction = ClientResultAction | {
  schema: "octopus.client_action"; schema_version: 1; action: "open_surface"; surface_id: string;
};
export function parseClientAction(value: unknown): ClientAction;
