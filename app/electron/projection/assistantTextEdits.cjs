const path = require("node:path");
const { readOptionalRegularJsonDocument } = require("./jsonFiles.cjs");
const { exactObjectKeys, requiredSafeRegistryId } = require("./values.cjs");

function assistantTextEditsPath(home, runId) {
  return path.join(home, "client-state", "workspace", "assistant-text-edits", `${requiredSafeRegistryId(runId, "edited task")}.json`);
}

function applyAssistantTextEdits(messages, runId, document) {
  if (!document) return messages;
  exactObjectKeys(document, ["schema_version", "run_id", "edits"], ["schema_version", "run_id", "edits"], "Assistant text edits");
  if (document.schema_version !== 1 || document.run_id !== runId || !Array.isArray(document.edits)) {
    throw new Error("Assistant text edits do not match this task");
  }
  const edits = new Map();
  for (const edit of document.edits) {
    const keys = ["provider_interface", "turn_id", "item_id", "original_text", "text", "edited_at_ms"];
    exactObjectKeys(edit, keys, keys, "Assistant text edit");
    if (keys.slice(0, 5).some(key => typeof edit[key] !== "string" || !edit[key].trim())
      || !Number.isSafeInteger(edit.edited_at_ms) || edit.edited_at_ms <= 0) {
      throw new Error("Assistant text edit has invalid content or identity");
    }
    const key = JSON.stringify([edit.provider_interface, edit.turn_id, edit.item_id]);
    if (edits.has(key)) throw new Error("Assistant text edits repeat a message");
    edits.set(key, edit);
  }
  return messages.map(message => {
    if (message.role !== "assistant" || message.event_kind !== "message" || message.run_id !== runId) return message;
    const data = message.event_data ?? {}, event = data._event ?? {};
    const edit = edits.get(JSON.stringify([data._activity_origin?.interface_id,
      data.turn_id ?? data.turnId ?? event.turn_id, data.item_id ?? data.itemId ?? event.item_id]));
    if (!edit) return message;
    if (message.text !== edit.original_text && message.text !== edit.text) {
      throw new Error("The assistant answer changed; its saved text edit needs review");
    }
    return { ...message, text: edit.text, event_data: { ...data, _client_text_edit: edit } };
  });
}

async function readAssistantTextEdits(home, runId, messages) {
  const stored = await readOptionalRegularJsonDocument(assistantTextEditsPath(home, runId), "Assistant text edits");
  return applyAssistantTextEdits(messages, runId, stored?.document);
}

module.exports = { assistantTextEditsPath, applyAssistantTextEdits, readAssistantTextEdits };
