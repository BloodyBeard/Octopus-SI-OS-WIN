export type WorkerEnvironmentLocalMutationOperation =
  | "file.write"
  | "git.apply"
  | "git.revert"
  | "git.stage"
  | "git.unstage";

export type WorkerEnvironmentLocalMutation = {
  schemaVersion: 1;
  workerEnvironmentId: string;
  surfaceId: string;
  originId: string;
  operation: WorkerEnvironmentLocalMutationOperation;
  paths: string[];
  recordedAtMs: number;
};

const LOCAL_MUTATION_EVENT = "octopus:worker-environment-local-mutation";
const LOCAL_MUTATION_CHANNEL = "octopus.worker-environment-local-mutation.v1";
const rendererId = crypto.randomUUID();
let channel: BroadcastChannel | null | undefined;

export function dispatchWorkerEnvironmentLocalMutation(input: Omit<WorkerEnvironmentLocalMutation, "schemaVersion" | "recordedAtMs">): void {
  const mutation = normalizeMutation({
    ...input,
    schemaVersion: 1,
    recordedAtMs: Date.now(),
  });
  if (!mutation) throw new Error("Worker Environment local mutation receipt is invalid");
  window.dispatchEvent(new CustomEvent(LOCAL_MUTATION_EVENT, { detail: mutation }));
  mutationChannel()?.postMessage({ rendererId, mutation });
}

export function subscribeWorkerEnvironmentLocalMutations(
  listener: (mutation: WorkerEnvironmentLocalMutation) => void,
): () => void {
  const receive = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const mutation = normalizeMutation(event.detail);
    if (mutation) listener(mutation);
  };
  window.addEventListener(LOCAL_MUTATION_EVENT, receive);
  mutationChannel();
  return () => window.removeEventListener(LOCAL_MUTATION_EVENT, receive);
}

function mutationChannel(): BroadcastChannel | null {
  if (channel !== undefined) return channel;
  if (typeof BroadcastChannel === "undefined") {
    channel = null;
    return channel;
  }
  channel = new BroadcastChannel(LOCAL_MUTATION_CHANNEL);
  channel.addEventListener("message", (event: MessageEvent<unknown>) => {
    const record = objectValue(event.data);
    if (!record || record.rendererId === rendererId) return;
    const mutation = normalizeMutation(record.mutation);
    if (mutation) window.dispatchEvent(new CustomEvent(LOCAL_MUTATION_EVENT, { detail: mutation }));
  });
  return channel;
}

function normalizeMutation(value: unknown): WorkerEnvironmentLocalMutation | null {
  const record = objectValue(value);
  if (!record || record.schemaVersion !== 1) return null;
  const workerEnvironmentId = boundedText(record.workerEnvironmentId, 512);
  const surfaceId = boundedText(record.surfaceId, 512);
  const originId = boundedText(record.originId, 512);
  const operation = record.operation;
  const recordedAtMs = Number(record.recordedAtMs);
  const paths = Array.isArray(record.paths)
    ? record.paths.map((path) => boundedText(path, 4_096)).filter(Boolean).slice(0, 200)
    : [];
  if (!workerEnvironmentId || !surfaceId || !originId || !isOperation(operation) || !Number.isSafeInteger(recordedAtMs) || recordedAtMs <= 0) return null;
  return { schemaVersion: 1, workerEnvironmentId, surfaceId, originId, operation, paths, recordedAtMs };
}

function isOperation(value: unknown): value is WorkerEnvironmentLocalMutationOperation {
  return value === "file.write"
    || value === "git.apply"
    || value === "git.revert"
    || value === "git.stage"
    || value === "git.unstage";
}

function boundedText(value: unknown, limit: number): string {
  const text = typeof value === "string" ? value.trim() : "";
  return text && text.length <= limit && !/[\u0000-\u001f]/.test(text) ? text : "";
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}
