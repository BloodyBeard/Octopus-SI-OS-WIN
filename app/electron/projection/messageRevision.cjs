const { contentHashHex } = require("./values.cjs");
function applyMessageRevision(messages, receipt) {
  if (receipt?.phase !== "message_revised") return false;
  const matches = (message, ref) => message?.role === "user"
    && message.artifact_id === ref?.id && message.content_hash === contentHashHex(ref?.content);
  const replacement = messages.findIndex(message => matches(message, receipt.source_artifact));
  const original = messages.findIndex(message => matches(message, receipt.replaces_message));
  if (replacement >= 0 && original < 0) {
    const retained = messages[replacement].event_data?.message_revision_receipt;
    if (retained?.replaces_message?.id === receipt.replaces_message?.id
        && contentHashHex(retained.replaces_message.content) === contentHashHex(receipt.replaces_message.content)) return true;
  }
  if (replacement < 0 || original < 0 || original >= replacement
      || messages.slice(original + 1, replacement).some(message => message.role === "user")) {
    throw new Error("Admitted message revision does not identify the latest input and its replacement");
  }
  const from = messages[original].event_data?.message_revision_receipt?.from_movement_seq ?? messages[original].movement_seq;
  messages[replacement].event_data = { ...messages[replacement].event_data,
    message_revision_receipt: { ...receipt, from_movement_seq: from } };
  messages.splice(original, replacement - original);
  return true;
}

module.exports = { applyMessageRevision };
