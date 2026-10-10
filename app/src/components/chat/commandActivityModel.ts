import type { ChatRenderMessage } from "./types";

export type CommandPhase = "running" | "exited" | "timed_out" | "interrupted" | "failed" | "unknown";
export type CommandOutputSection = { stream: string; text: string };
export type CommandActivityModel = {
  phase: CommandPhase;
  invocationStatus: string;
  exitCode: number | null;
  running: boolean;
  yielded: boolean;
  continuationAvailable: boolean;
  processId: string;
  failure: string;
  durationMs: number | null;
  sections: CommandOutputSection[];
  notices: string[];
};

export function commandResultBody(data: Record<string, unknown>): Record<string, unknown> {
  return record(data.body ?? data.result ?? record(data.error).details);
}

export function commandHasProcessEvidence(data: Record<string, unknown>): boolean {
  const body = commandResultBody(data);
  const output = record(body.output ?? data.output);
  return Boolean(body.process_id) || ["running", "exited", "closed", "exit_code", "exitCode", "timed_out", "chunks", "stdout", "stderr", "failure"]
    .some((key) => Object.prototype.hasOwnProperty.call(output, key));
}

export function commandActivityModel(message: ChatRenderMessage): CommandActivityModel {
  const data = record(message.eventData);
  const body = commandResultBody(data);
  const output = record(body.output ?? data.output);
  const invocationStatus = text(data.status);
  const native = message.eventKind === "tool";
  const status = text(output.status || (native ? data.status : "")).toLowerCase();
  const rawExit = output.exit_code ?? output.exitCode ?? data.exit_code ?? data.exitCode;
  const exitCode = typeof rawExit === "number" && Number.isInteger(rawExit) && rawExit >= 0 ? rawExit : null;
  const running = output.running === true;
  const continuationAvailable = body.continuation_available === true;
  const failure = text(output.failure) || text(record(output.failure).message) || text(record(data.error).message)
    || (typeof data.error === "string" ? data.error : "");
  const timedOut = output.timed_out === true || data.timed_out === true || ["timed_out", "timeout"].includes(status);
  const interrupted = output.interrupted === true || output.cancelled === true
    || ["stopped", "interrupted", "cancelled", "canceled", "aborted"].includes(status);
  const failed = Boolean(failure || message.error || data.ok === false) || ["failed", "failure", "error", "declined", "rejected"].includes(status);
  const exited = output.exited === true || exitCode !== null || (body.completed === true && !body.process_id)
    || (native && ["completed", "complete", "done", "success", "succeeded", "exited"].includes(status));
  const started = message.pending === true || ["started", "starting", "running", "working", "in_progress", "inprogress", "pending", "queued", "streaming", "progress"]
    .includes(invocationStatus.toLowerCase());
  const phase: CommandPhase = timedOut ? "timed_out" : interrupted ? "interrupted" : failed ? "failed"
    : running ? "running" : exited ? "exited" : started ? "running" : "unknown";
  const notices: string[] = [];
  const sections: CommandOutputSection[] = [];
  const projection = record(data._payload_projection);
  if (projection.truncated === true && projection.resolved !== true) notices.push("Command evidence is a partial preview.");
  const chunks = Array.isArray(output.chunks) ? output.chunks : [];
  if (chunks.length) {
    for (const value of chunks) {
      const chunk = record(value);
      appendSection(sections, text(chunk.stream), streamText(chunk.content, chunk.preview, notices));
    }
  } else if (Object.prototype.hasOwnProperty.call(output, "stdout") || Object.prototype.hasOwnProperty.call(output, "stderr")) {
    for (const stream of ["stdout", "stderr"] as const) {
      const value = streamText(output[stream], output[`${stream}_preview`] ?? output[`${stream}Preview`], notices);
      if (value) sections.push({ stream, text: value });
      const omitted = finiteNumber(output[`${stream}_omitted_bytes`]);
      if (output[`${stream}_truncated`] === true || (omitted !== null && omitted > 0)) {
        notices.push(`${stream}: ${omitted !== null ? `${omitted} bytes omitted` : "output truncated"}${output[`${stream}_retention`] === "head_tail" ? " · beginning and end retained" : ""}.`);
      }
    }
  } else {
    const value = data.aggregated_output ?? output.aggregated_output ?? output.aggregatedOutput ?? output.output
      ?? (typeof data.output === "string" ? data.output : undefined)
      ?? (native && typeof data.result === "string" ? data.result : undefined);
    if (typeof value === "string" && value) sections.push({ stream: "", text: value });
  }
  if (continuationAvailable) notices.push("More output is available.");
  const control = record(body.control);
  if (control.delivered === true && text(control.action)) notices.push(`Process control delivered: ${text(control.action)}.`);
  if (failure) notices.push(failure);
  if (running && phase !== "running") notices.push("The process is still running.");
  return {
    phase, invocationStatus, exitCode, running,
    yielded: running && body.completed === false && invocationStatus === "completed",
    continuationAvailable, processId: text(body.process_id), failure,
    durationMs: finiteNumber(output.duration_ms ?? output.durationMs ?? data.duration_ms ?? data.durationMs ?? message.latencyMs),
    sections, notices: [...new Set(notices)],
  };
}

export function commandOutcomeLabel(model: CommandActivityModel): string {
  if (model.phase === "running") return model.yielded ? "Still running" : "Running";
  const exit = model.exitCode === null ? "" : ` · exit code ${model.exitCode}`;
  if (model.phase === "timed_out") return `Timed out${exit}`;
  if (model.phase === "interrupted") return `Interrupted${exit}`;
  if (model.phase === "failed") return `Failed${exit}`;
  if (model.phase === "unknown") return "Outcome unknown";
  return model.exitCode === 0 ? "Success" : model.exitCode === null ? "Exited · exit code unavailable" : `Exit code ${model.exitCode}`;
}

function streamText(value: unknown, preview: unknown, notices: string[]): string {
  if (typeof value === "string") return value;
  const exact = record(value);
  if (typeof exact.text === "string") return exact.text;
  const rendered = record(preview);
  if (rendered.lossy === true) notices.push("Text preview contains replacement characters; retained bytes remain in the artifact.");
  if (typeof rendered.text === "string") return rendered.text;
  if (exact.encoding === "base64") notices.push(`Binary output${finiteNumber(exact.retained_bytes) !== null ? ` · ${exact.retained_bytes} bytes retained` : ""}; no text preview.`);
  return "";
}

function appendSection(sections: CommandOutputSection[], stream: string, value: string) {
  if (!value) return;
  const last = sections.at(-1);
  if (last?.stream === stream) last.text += value;
  else sections.push({ stream, text: value });
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function finiteNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}
