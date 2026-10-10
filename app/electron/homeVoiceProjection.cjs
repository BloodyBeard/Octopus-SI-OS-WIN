const VOICE_KEYS = ["control_interface_id", "provider_id", "delegation_interface_id", "run_id"];

function parseHomeVoiceContext(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== VOICE_KEYS.length
    || VOICE_KEYS.some((key) => typeof value[key] !== "string" || !value[key]
      || value[key].trim() !== value[key] || /[\x00-\x1f\x7f/\\]/.test(value[key]) || value[key] === "." || value[key] === "..")) {
    throw new Error("Home voice context must declare exact control, provider, delegation and run identities.");
  }
  if (new Set(VOICE_KEYS.slice(0, 3).map((key) => value[key])).size !== 3) {
    throw new Error("Home voice control, media provider and delegation receiver must have separate interface identities.");
  }
  return Object.fromEntries(VOICE_KEYS.map((key) => [key, value[key]]));
}

function projectHomeVoiceContext(record, sidecar, clientState, runsById) {
  const configured = sidecar?.voice_context;
  const recorded = clientState?.voice_context;
  if (configured == null && recorded == null) return null;
  if (clientState?.client_surface_role !== "home" || (sidecar && sidecar.surface_role !== "home")) {
    throw new Error("Voice context must belong to the exact Home client.");
  }
  const context = parseHomeVoiceContext(recorded);
  if (sidecar) {
    const declaration = parseHomeVoiceContext(configured);
    if (VOICE_KEYS.some((key) => context[key] !== declaration[key])) {
      throw new Error("Home voice context conflicts with its authored run record.");
    }
  }
  if (context.run_id === record?.run_id) throw new Error("Home voice requires its own execution Run.");
  const voice = runsById?.get(context.run_id);
  const origin = voice?.interface_state?.[context.control_interface_id];
  const homeClient = Object.keys(record?.interface_state ?? {}).find((id) => record.interface_state[id] === clientState);
  if (!voice || !homeClient || voice.ee_id !== record?.ee_id || voice.worker !== record?.worker
    || origin?.home_run !== record?.run_id || origin?.home_interface !== homeClient
    || (sidecar && sidecar.interface !== homeClient)) {
    throw new Error("Home voice context has no matching independent Run binding.");
  }
  const interfaces = new Set((voice.checkpoints ?? []).filter((cp) => cp.status !== "made")
    .flatMap((checkpoint) => [checkpoint.interface, ...(checkpoint.out_interfaces ?? [])]));
  const receives = (record.checkpoints ?? []).some((cp) => cp.status !== "made" && cp.interface === context.delegation_interface_id);
  if (!receives || VOICE_KEYS.slice(0, 3).some((key) => !interfaces.has(context[key]))) {
    throw new Error("Home voice context references an interface absent from the authored run bindings.");
  }
  return context;
}

module.exports = { parseHomeVoiceContext, projectHomeVoiceContext };
