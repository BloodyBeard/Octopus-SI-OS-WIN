const { contentHashHex, requiredString } = require("./values.cjs");

function messageRevision(value) {
  if (value === undefined || value === null) return null;
  if (Object.keys(value).length !== 2) throw new Error("Message revision requires one exact artifact reference");
  return { id: requiredString(value.id, "revised message id"), content: [...Buffer.from(contentHashHex(value.content, "revised message content"), "hex")] };
}

async function readMessageRevision(options, request, label) {
  const revision = messageRevision(request.replacesMessage);
  if (!revision) return null;
  if (!request.runId || request.attachments?.length || request.continuation) {
    throw new Error("Editing requires the existing task and preserves its original attachments");
  }
  const { messages } = await options.readProjection("projection.operator.chat.messages", { run_id: request.runId });
  const latest = messages.filter(message => message.role === "user").at(-1);
  const retained = latest?.id === request.messageId && latest.event_data?.replaces_message?.id === revision.id
    && contentHashHex(latest.event_data.replaces_message.content) === contentHashHex(revision.content);
  if (!latest || latest.event_kind !== "message" || latest.run_id !== request.runId
      || !retained && (latest.artifact_id !== revision.id || latest.content_hash !== contentHashHex(revision.content))) {
    throw new Error("Only the most recent message can be edited. The task changed; your text has been kept.");
  }
  if (!retained) {
    const { chats } = await options.readProjection("projection.operator.chats.status", { run_ids: [request.runId] });
    const state = chats.find(chat => chat.run_id === request.runId);
    if (!state || state.archived || state.turn_open) throw new Error("Wait for this reply to finish before editing its message");
  }
  const { payload } = await options.readArtifactPayload({ artifactId: revision.id, manifestHash: contentHashHex(revision.content), label });
  return { revision, payload };
}

module.exports = { messageRevision, readMessageRevision };
