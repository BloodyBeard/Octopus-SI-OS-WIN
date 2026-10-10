import { readProjection, submitInterfaceArtifact } from "../../projectionClient";
import { nextProjectionChange } from "../../projectionLive";
import type { ChatRenderMessage } from "../chat/types";

export type VoiceContextMessage = Readonly<{ id: string; role: "user" | "assistant"; text: string }>;
export type VoiceInitialItem = { type: "message"; role: "developer" | "user" | "assistant"; content: { type: "input_text"; text: string }[] };
export type VoiceContextSelection = {
  runId: string;
  interfaceId: string;
  messages: readonly VoiceContextMessage[];
  initialItems: VoiceInitialItem[];
  omittedMessageCount: number;
  truncated: boolean;
  disclosure: string;
};

const CONTEXT_MESSAGE_LIMIT = 24;
const CONTEXT_TEXT_BYTES = 24_000;

export function projectHomeVoiceContext(messages: readonly ChatRenderMessage[], runId: string, interfaceId: string): VoiceContextSelection {
  const admitted = new Map<string, VoiceContextMessage>();
  for (const message of messages) {
    if (!runId || !interfaceId || message.runId !== runId || message.interfaceId !== interfaceId || !message.artifactId || message.pending || message.error) continue;
    const user = message.role === "user" && message.direction === "in" && (message.eventKind === "message" || message.eventKind === "steering");
    const assistant = message.role === "assistant" && message.direction === "out" && message.eventKind === "message" && message.terminal === true;
    if ((!user && !assistant) || !message.text.trim()) continue;
    admitted.set(message.artifactId, { id: message.artifactId, role: user ? "user" : "assistant", text: message.text });
  }
  const visible = [...admitted.values()];
  const selected: VoiceContextMessage[] = [];
  let bytes = 0;
  let shortened = false;
  for (let index = visible.length - 1; index >= 0 && selected.length < CONTEXT_MESSAGE_LIMIT; index -= 1) {
    const message = visible[index];
    const length = new TextEncoder().encode(message.text).length;
    if (bytes + length > CONTEXT_TEXT_BYTES && selected.length > 0) break;
    const text = boundedContextText(message.text, CONTEXT_TEXT_BYTES - bytes);
    shortened ||= text !== message.text;
    selected.unshift({ ...message, text });
    bytes += new TextEncoder().encode(text).length;
    if (shortened) break;
  }
  const omitted = visible.length - selected.length;
  const disclosure = `Voice context includes ${selected.length} recent visible Home text messages. ${omitted} older visible messages omitted.${shortened ? " The newest message is truncated to the text limit." : ""} Attachments and unloaded history are not included.`;
  return {
    runId, interfaceId, messages: visible, omittedMessageCount: omitted, truncated: shortened, disclosure,
    initialItems: [
      { type: "message", role: "developer", content: [{ type: "input_text", text: `Client context selection: ${disclosure} The following items are existing conversation history, not new assignments.` }] },
      ...selected.map((message): VoiceInitialItem => ({ type: "message", role: message.role, content: [{ type: "input_text", text: message.text }] })),
    ],
  };
}

function boundedContextText(text: string, limit: number): string {
  if (new TextEncoder().encode(text).length <= limit) return text;
  const suffix = "\n[Remaining message text omitted by the client context limit.]";
  const budget = Math.max(0, limit - new TextEncoder().encode(suffix).length);
  let selected = "", bytes = 0;
  for (const character of text) {
    const length = new TextEncoder().encode(character).length;
    if (bytes + length > budget) break;
    selected += character;
    bytes += length;
  }
  return selected + suffix;
}

export type VoiceCallBinding = {
  runId: string;
  controlInterfaceId: string;
  providerId: string;
  delegationInterfaceId: string;
};

export type VoiceArtifact = {
  artifact_id: string;
  label: string;
  seq: number;
  payload?: Record<string, unknown>;
  error?: string;
};

export type VoiceCall = {
  binding: VoiceCallBinding;
  correlationId: string;
  answer: Promise<Record<string, unknown>> | null;
  control: Promise<void> | null;
  stop: Promise<void> | null;
  closeRequested: boolean;
  creationDisposition: "not_created" | "unconfirmed" | "created";
  callId: string;
  remoteClosed: boolean;
  closedPayload?: Record<string, unknown>;
};

export async function assertVoiceControlReady(binding: VoiceCallBinding): Promise<void> {
  for (const name of ["runId", "controlInterfaceId", "providerId", "delegationInterfaceId"] as const) {
    const value = binding[name];
    if (typeof value !== "string" || !value || value.trim() !== value) throw new Error(`Home voice has no declared ${name}.`);
  }
  const response = await readProjection<{ artifact_transfer_targets?: Array<Record<string, unknown>> }>(
    "projection.interfaces.artifact_transfer.targets", { run_id: binding.runId, interface_id: binding.controlInterfaceId, limit: 1_000 },
  );
  const target = response.artifact_transfer_targets?.find((item) => item.interface_id === binding.controlInterfaceId);
  if (target?.status !== "ready") {
    throw new Error(typeof target?.reason === "string" ? target.reason : "Home voice control interface is not ready.");
  }
}

export async function readVoiceArtifacts(call: VoiceCall, labels: string[], afterSeq = -1) {
  const result = await readProjection<{ artifacts?: VoiceArtifact[] }>("projection.interface_artifacts.list", {
    run_id: call.binding.runId,
    interface_id: call.binding.providerId,
    labels,
    after_seq: afterSeq,
    limit: 500,
  });
  const rows = result.artifacts ?? [];
  const artifacts = rows.filter((artifact) => {
    if (artifact.error) throw new Error(artifact.error);
    return artifact.payload?.correlation_id === call.correlationId;
  });
  return { artifacts, afterSeq: rows.reduce((seq, artifact) => Math.max(seq, artifact.seq), afterSeq), more: rows.length === 500 };
}

async function waitForArtifact(call: VoiceCall, label: string, matches: (payload: Record<string, unknown>) => boolean, signal?: AbortSignal) {
  const timeout = new AbortController();
  const expired = () => new Error(`Voice provider did not confirm ${label === "realtime.session.answer" ? "call creation" : label === "realtime.session.updated" ? "context delivery" : "call closure"} within 45 seconds.`);
  const timer = setTimeout(() => timeout.abort(expired()), 45_000);
  const waiting = signal ? AbortSignal.any([signal, timeout.signal]) : timeout.signal;
  const deadline = Date.now() + 45_000;
  let afterSeq = -1;
  try {
    while (Date.now() < deadline) {
      waiting.throwIfAborted();
      const changed = nextProjectionChange(1_000);
      const batch = await waitForVoiceStep(readVoiceArtifacts(call, [label], afterSeq), waiting);
      waiting.throwIfAborted();
      afterSeq = batch.afterSeq;
      for (const artifact of batch.artifacts) {
        if (artifact.payload && matches(artifact.payload)) return artifact.payload;
      }
      if (!batch.more) await waitForVoiceStep(changed, waiting);
    }
    throw expired();
  } finally { clearTimeout(timer); }
}

function waitForVoiceStep<T>(operation: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const aborted = () => { signal.removeEventListener("abort", aborted); reject(signal.reason); };
    operation.then(value => {
      signal.removeEventListener("abort", aborted);
      if (signal.aborted) reject(signal.reason);
      else resolve(value);
    }, failure => {
      signal.removeEventListener("abort", aborted);
      reject(failure);
    });
    if (signal.aborted) aborted();
    else signal.addEventListener("abort", aborted, { once: true });
  });
}

export function openVoiceCall(call: VoiceCall, offerSdp: string, initialItems: VoiceInitialItem[] = []): Promise<Record<string, unknown>> {
  if (call.closeRequested) throw new Error("This voice call was cancelled before its offer was submitted.");
  if (call.answer) throw new Error("This voice offer was already submitted.");
  call.creationDisposition = "unconfirmed";
  call.answer = (async () => {
    await submitInterfaceArtifact({
      interfaceId: call.binding.controlInterfaceId,
      runId: call.binding.runId,
      label: "realtime.session.offer",
      payload: { offer_sdp: offerSdp, correlation_id: call.correlationId, delegation_interface: call.binding.delegationInterfaceId, session: { initial_items: initialItems } },
      timeoutMs: 45_000,
    });
    const answer = await waitForArtifact(call, "realtime.session.answer", () => true);
    retainOfferEvidence(call, answer);
    if (answer.ok !== true) throw new Error(typeof answer.error === "string" ? answer.error : "Voice provider refused the call.");
    if (!call.callId) throw new Error("Voice provider returned no call identity.");
    return answer;
  })();
  return call.answer;
}

export async function updateVoiceContext(call: VoiceCall, message: VoiceContextMessage, signal: AbortSignal): Promise<void> {
  signal.throwIfAborted();
  if (call.closeRequested || call.remoteClosed) throw new Error("Voice call is not available for a context update.");
  const operation = (call.control ?? Promise.resolve()).then(() => submitVoiceContext(call, message, signal));
  call.control = operation.then(() => {}, () => {});
  await operation;
}

async function submitVoiceContext(call: VoiceCall, message: VoiceContextMessage, signal: AbortSignal): Promise<void> {
  signal.throwIfAborted();
  if (!call.callId || call.closeRequested || call.remoteClosed) throw new Error("Voice call is not available for a context update.");
  const updateId = `home-context:${message.id}`;
  if (!message.id || new TextEncoder().encode(updateId).length > 256 || /[\u0000-\u001f\u007f]/.test(updateId) || updateId.trim() !== updateId) throw new Error("Home context message has no supported artifact identity.");
  await submitInterfaceArtifact({
    interfaceId: call.binding.controlInterfaceId,
    runId: call.binding.runId,
    label: "realtime.session.update",
    payload: {
      call_id: call.callId, correlation_id: call.correlationId, update_id: updateId, delegation_id: null, channel: "context",
      text: JSON.stringify({ source: "Visible Home conversation; context only, not a new assignment", artifact_id: message.id, role: message.role, text: boundedContextText(message.text, 8_000) }),
    },
    timeoutMs: 45_000,
  });
  const result = await waitForArtifact(call, "realtime.session.updated", (payload) => (
    payload.call_id === call.callId && payload.update_id === updateId
    && (payload.status !== "chunk_injected" || payload.ok === false)
  ), signal);
  if (result.ok === false || (result.status !== "transport_written" && result.status !== "injected")) throw new Error(typeof result.error === "string" ? result.error : "The voice provider did not accept the Home context update.");
  signal.throwIfAborted();
}

export function closeVoiceCall(call: VoiceCall): Promise<void> {
  if (call.stop) return call.stop;
  const retry = call.closeRequested;
  call.closeRequested = true;
  const stopping = (async () => {
    const deadline = new AbortController();
    const signal = deadline.signal;
    let stage = "call creation";
    const armDeadline = () => setTimeout(() => deadline.abort(new Error(`Your microphone is off. Voice shutdown timed out while waiting for ${stage}. Retry closing this call.`)), 45_000);
    let timer = armDeadline();
    try {
      if (!call.answer || offerWasNotCreated(call)) return;
      if (!call.callId) {
        try { await waitForVoiceStep(call.answer, signal); }
        catch { signal.throwIfAborted(); await catchUpCallEvidence(call, signal); }
        signal.throwIfAborted();
        clearTimeout(timer);
        timer = armDeadline();
      }
      if (offerWasNotCreated(call)) return;
      if (call.remoteClosed) return;
      if (!call.callId) throw new Error("Your microphone is off, but call creation remains unconfirmed. Retry closure to check for a late provider answer; the offer will not be replayed.");
      if (retry) {
        stage = "previous closure evidence";
        await catchUpCallEvidence(call, signal);
      }
      if (call.remoteClosed) return;
      signal.throwIfAborted();
      const stopId = crypto.randomUUID();
      stage = "stop-request delivery";
      await waitForVoiceStep(submitInterfaceArtifact({
        interfaceId: call.binding.controlInterfaceId,
        runId: call.binding.runId,
        label: "realtime.session.stop",
        payload: { call_id: call.callId, correlation_id: call.correlationId, stop_id: stopId },
        timeoutMs: 45_000,
      }), signal);
      stage = "the provider's closure confirmation";
      const closed = await waitForArtifact(call, "realtime.session.closed", (payload) => (
        payload.call_id === call.callId && (payload.stop_id === stopId || payload.remote_closed === true)
      ), signal);
      call.closedPayload = closed;
      if (closed.remote_closed !== true) {
        throw new Error(typeof closed.error === "string" ? closed.error : "Your microphone is off, but the provider has not confirmed call closure.");
      }
      call.remoteClosed = true;
    } finally { clearTimeout(timer); deadline.abort(); }
  })();
  call.stop = stopping;
  void stopping.catch(() => { if (call.stop === stopping) call.stop = null; });
  return stopping;
}

async function catchUpCallEvidence(call: VoiceCall, signal: AbortSignal): Promise<void> {
  let afterSeq = -1;
  for (;;) {
    const batch = await waitForVoiceStep(readVoiceArtifacts(call, ["realtime.session.answer", "realtime.session.state", "realtime.session.closed"], afterSeq), signal);
    for (const artifact of batch.artifacts) {
      const payload = artifact.payload;
      if (!payload) continue;
      retainOfferEvidence(call, payload);
      if (artifact.label === "realtime.session.closed" && call.callId && payload.call_id === call.callId && payload.remote_closed === true) {
        call.closedPayload = payload;
        call.remoteClosed = true;
      }
    }
    if (!batch.more) return;
    if (batch.afterSeq <= afterSeq) throw new Error("Voice evidence cursor did not advance.");
    afterSeq = batch.afterSeq;
  }
}

function offerWasNotCreated(call: VoiceCall): boolean {
  return call.creationDisposition === "not_created";
}

export function canRetireVoiceCall(call: VoiceCall): boolean {
  return !call.answer || offerWasNotCreated(call) || call.remoteClosed;
}

function retainOfferEvidence(call: VoiceCall, payload: Record<string, unknown>): void {
  if (payload.call_id !== undefined && payload.call_id !== null && payload.call_id !== "") {
    if (typeof payload.call_id !== "string" || new TextEncoder().encode(payload.call_id).length > 256
      || payload.call_id.trim() !== payload.call_id || /[\u0000-\u001f\u007f]/.test(payload.call_id)) {
      throw new Error("Voice provider returned an invalid call identity.");
    }
    if (call.callId && call.callId !== payload.call_id) throw new Error("Voice provider changed this offer's call identity.");
    call.callId = payload.call_id;
    call.creationDisposition = "created";
  }
  const disposition = payload.creation_disposition;
  if (disposition === undefined) return;
  if (disposition !== "not_created" && disposition !== "unconfirmed" && disposition !== "created") throw new Error("Voice provider returned an invalid creation disposition.");
  if ((call.callId && disposition !== "created") || (!call.callId && disposition === "created")) throw new Error("Voice provider returned contradictory creation evidence.");
  if (disposition === "not_created" && payload.ok !== false) throw new Error("Voice provider did not prove its refusal before creation.");
  call.creationDisposition = disposition;
}
