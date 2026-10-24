import type { ChatRun } from "../../chatRuns";
import type { ChatRenderMessage } from "./types";

export function withRunOperationTargets(messages: ChatRenderMessage[], runs: readonly ChatRun[]): ChatRenderMessage[] {
  const byId = new Map(runs.map((run) => [run.runId, run]));
  const byInterface = new Map<string, ChatRun[]>();
  for (const run of runs) for (const id of run.inputInterfaceIds ?? []) {
    const peers = byInterface.get(id) ?? [];
    peers.push(run);
    byInterface.set(id, peers);
  }
  let declaredPeers: string[] = [];
  return messages.map((message) => {
    const data = message.eventData;
    if (data?.namespace !== "formed_interfaces") return message;
    const args = record(data.arguments), body = record(data.body);
    if (data.tool === "list_runs" && Array.isArray(body.declared_peer_interfaces)) {
      declaredPeers = body.declared_peer_interfaces.filter((id): id is string => typeof id === "string");
    }
    const targets = new Map<string, ChatRun>();
    const add = (run: ChatRun | undefined) => { if (run) targets.set(run.runId, run); };
    const runId = record(body.created).run_id;
    if (typeof runId === "string") add(byId.get(runId));
    const addressed = args.interface ?? body.interface;
    const interfaces = typeof addressed === "string" ? [addressed]
      : data.tool === "wait_for_runs" || data.tool === "list_runs" ? declaredPeers : [];
    for (const id of interfaces) for (const run of byInterface.get(id) ?? []) add(run);
    if (!targets.size) return message;
    return { ...message, relatedRuns: [...targets.values()].map((run) => ({ runId: run.runId, title: run.title })) };
  });
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
