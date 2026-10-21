const attentionKinds = new Set(["approval", "user_input", "elicitation"]);
const titles = { message: "Message", approval: "Approval needed", user_input: "Input needed", elicitation: "Information needed" };
const EXCERPT_LIMIT = 320;

function activityExcerpt(text) {
  if (typeof text !== "string") return null;
  const plain = text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}(?:#{1,6}|>|[-*+]|\d+\.)\s+/gm, "")
    .replace(/\*\*|__|`/g, "")
    .replace(/\s+/g, " ")
    .trim();
  if (!plain) return null;
  return plain.length > EXCERPT_LIMIT ? `${plain.slice(0, EXCERPT_LIMIT - 1).trimEnd()}…` : plain;
}

function operatorActivityRows({ chat, messages, deliveries, responses, agent }) {
  const rows = [], pending = new Map();
  const delivered = new Map(deliveries.map((row) => [JSON.stringify([row.artifact_id, row.seq]), row]));
  const ordered = [...messages].sort((a, b) => a.movement_seq - b.movement_seq
    || (a.event_data?._stream?.stream_seq ?? 0) - (b.event_data?._stream?.stream_seq ?? 0));
  const make = (message, kind, requestId, movementId) => ({
    id: JSON.stringify([kind, chat.run_id, message.artifact_id, message.movement_seq, requestId, message.event_data?._stream?.stream_seq ?? null]),
    kind, title: `${titles[kind]} — ${chat.title}`, name: kind,
    run_id: chat.run_id, client_surface_role: chat.client_surface_role,
    worker: chat.worker_interface_id, agent_id: agent?.id ?? null,
    agent_name: agent?.name ?? null, agent_icon: agent?.icon ?? null,
    agent_color: agent ? chat.agent_color ?? null : null,
    artifact_id: message.artifact_id, movement_id: movementId, seq: message.movement_seq,
    request_id: requestId, created_at_ms: message.created_at_ms || null,
    excerpt: activityExcerpt(message.text),
  });
  for (const message of ordered) {
    if (message.direction !== "out" || message.interface_id !== chat.chat_interface_id) continue;
    const kind = message.event_kind, data = message.event_data ?? {};
    if (kind === "operator_request_resolved") {
      const id = data.requestId;
      if (typeof id !== "string" && !Number.isSafeInteger(id)) continue;
      for (const [key, row] of pending) if (row.request_id === String(id)) pending.delete(key);
      continue;
    }
    if (kind === "message") {
      const delivery = delivered.get(JSON.stringify([message.artifact_id, message.movement_seq]));
      if (delivery && message.terminal === true && message.error !== true && data.ok !== false && !data.error && !data.data?.error && typeof message.text === "string" && message.text.trim()) {
        rows.push(make(message, kind, null, delivery.movement_id));
      }
      continue;
    }
    if (!attentionKinds.has(kind) || message.error === true) continue;
    const requestId = data.request_id;
    if (typeof requestId !== "string" || !requestId || !message.artifact_id || !Number.isSafeInteger(message.movement_seq)) {
      throw new Error(`Activity request has incomplete receipt identity in ${chat.run_id}`);
    }
    const key = `${kind}:${requestId}`;
    if (responses.some((response) => response.key === key && response.seq > message.movement_seq)) continue;
    pending.set(key, make(message, kind, requestId, null));
  }
  return [...rows, ...pending.values()];
}

module.exports = { operatorActivityRows };
