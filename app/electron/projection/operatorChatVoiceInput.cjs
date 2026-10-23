const { movementBoundaryInterface, movementRunId } = require("./movementRows.cjs");
const { ledgerArtifactLabel } = require("./values.cjs");

function isHomeVoiceInput(entry, row) {
  const state = entry.record?.interface_state?.[entry.chatInterfaceId];
  return state?.client_surface_role === "home"
    && typeof state.voice_context?.delegation_interface_id === "string"
    && row.kind === "InterfaceArrival" && movementRunId(row) === entry.runId
    && ledgerArtifactLabel(row) === "realtime.delegation"
    && movementBoundaryInterface(row, "from") === state.voice_context.delegation_interface_id;
}

module.exports = { isHomeVoiceInput };
