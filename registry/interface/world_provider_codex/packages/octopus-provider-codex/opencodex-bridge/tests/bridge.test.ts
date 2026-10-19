import { afterEach, expect, test } from "bun:test";
import { resolve } from "node:path";

type SpawnedBridge = {
  child: ReturnType<typeof Bun.spawn>;
  origin: string;
};

const children: Array<ReturnType<typeof Bun.spawn>> = [];
const servers: Array<ReturnType<typeof Bun.serve>> = [];

afterEach(async () => {
  for (const child of children.splice(0)) {
    child.kill();
    await child.exited;
  }
  for (const server of servers.splice(0)) server.stop(true);
});

function config(baseUrl: string, provider: Record<string, unknown> = {}) {
  return {
    schema_version: 1,
    interface_id: "world_provider_test_exact",
    provider: {
      id: "test-provider",
      adapter: "openai-chat",
      base_url: baseUrl,
      models: ["test-model"],
      default_model: "test-model",
      api_key: "test-secret",
      auth_header: "Authorization",
      auth_value_prefix: "Bearer ",
      headers: { "X-Octopus-Test": "exact" },
      allow_private_network: true,
      options: {},
      ...provider,
    },
    limits: {
      max_request_bytes: 1048576,
      max_response_bytes: 1048576,
      max_translated_call_bytes: 262144,
      max_translated_turn_bytes: 1048576,
      request_timeout_ms: 10000,
    },
  };
}

async function readLine(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let text = "";
  for (;;) {
    const next = await reader.read();
    if (next.done) throw new Error("bridge exited before readiness");
    text += decoder.decode(next.value, { stream: true });
    const newline = text.indexOf("\n");
    if (newline >= 0) {
      reader.releaseLock();
      return text.slice(0, newline);
    }
  }
}

async function spawnBridge(value: unknown): Promise<SpawnedBridge> {
  const binary = resolve(import.meta.dir, "../bin/octopus-opencodex-bridge");
  const child = Bun.spawn([binary], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  children.push(child);
  child.stdin.write(JSON.stringify(value));
  child.stdin.end();
  const ready = JSON.parse(await readLine(child.stdout));
  expect(ready).toEqual({
    schema_version: 1,
    status: "ready",
    port: expect.any(Number),
    interface_id: "world_provider_test_exact",
  });
  return { child, origin: `http://127.0.0.1:${ready.port}` };
}

test("actual OpenCodex chat adapter and Responses bridge preserve exact model and stream", async () => {
  let observed: Record<string, unknown> | undefined;
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      observed = {
        path: new URL(request.url).pathname,
        authorization: request.headers.get("authorization"),
        marker: request.headers.get("x-octopus-test"),
        body: await request.json(),
      };
      return new Response([
        'data: {"id":"chatcmpl_test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"hello"},"finish_reason":null}]}\n\n',
        'data: {"id":"chatcmpl_test","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}\n\n',
        "data: [DONE]\n\n",
      ].join(""), { headers: { "Content-Type": "text/event-stream" } });
    },
  });
  servers.push(upstream);
  const bridge = await spawnBridge(config(`http://127.0.0.1:${upstream.port}/v1`));
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "test-model",
      stream: true,
      input: [{ role: "user", content: [{ type: "input_text", text: "hi" }] }],
      tools: [{ type: "function", name: "exact_tool", description: "Exact", parameters: { type: "object", properties: {} } }],
    }),
  });
  expect(response.status).toBe(200);
  const text = await response.text();
  expect(text).toContain("response.output_text.delta");
  expect(text).toContain("hello");
  expect(text).toContain("response.completed");
  expect(observed).toEqual({
    path: "/v1/chat/completions",
    authorization: "Bearer test-secret",
    marker: "exact",
    body: expect.objectContaining({ model: "test-model", stream: true }),
  });
});

test("model outside the exact interface grant is rejected before upstream", async () => {
  let requests = 0;
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch() {
      requests += 1;
      return Response.json({});
    },
  });
  servers.push(upstream);
  const bridge = await spawnBridge(config(`http://127.0.0.1:${upstream.port}/v1`));
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: "other-model", input: "hi", stream: true }),
  });
  expect(response.status).toBe(400);
  expect(await response.text()).toContain("outside the exact interface model grant");
  expect(requests).toBe(0);
});

test("unknown config fields fail closed before the listener starts", async () => {
  const value = config("http://127.0.0.1:1/v1") as ReturnType<typeof config> & { fallback_provider?: string };
  value.fallback_provider = "forbidden";
  const binary = resolve(import.meta.dir, "../bin/octopus-opencodex-bridge");
  const child = Bun.spawn([binary], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  children.push(child);
  child.stdin.write(JSON.stringify(value));
  child.stdin.end();
  expect(await child.exited).toBe(1);
  expect(await new Response(child.stderr).text()).toContain("unsupported field 'fallback_provider'");
});

function sse(events: Array<[string | null, unknown]>): Response {
  const body = events.map(([event, data]) => `${event ? `event: ${event}\n` : ""}data: ${typeof data === "string" ? data : JSON.stringify(data)}\n\n`).join("");
  return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
}

async function exchange(provider: Record<string, unknown>, upstreamReply: Response, model = "test-model", tools: unknown[] = []) {
  let observed: { path: string; search: string; headers: Headers; body: Record<string, unknown> } | undefined;
  const upstream = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      const url = new URL(request.url);
      observed = { path: url.pathname, search: url.search, headers: request.headers, body: await request.json() };
      return upstreamReply;
    },
  });
  servers.push(upstream);
  const { base_url: suffix, ...rest } = provider;
  const bridge = await spawnBridge(config(`http://127.0.0.1:${upstream.port}${suffix ?? "/v1"}`, rest));
  const response = await fetch(`${bridge.origin}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model, stream: true, input: [{ role: "user", content: [{ type: "input_text", text: "hi" }] }], tools }),
  });
  return { status: response.status, text: await response.text(), observed: observed! };
}

const exactTool = { type: "function", name: "exact_tool", description: "Exact", parameters: { type: "object", properties: {} } };

test("chat tool calls come back as Responses function calls", async () => {
  const result = await exchange({}, sse([
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{ index: 0, id: "call_1", type: "function", function: { name: "exact_tool", arguments: "{}" } }] }, finish_reason: null }] }],
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] }],
    [null, "[DONE]"],
  ]), "test-model", [exactTool]);
  expect(result.status).toBe(200);
  expect(result.text).toContain("function_call");
  expect(result.text).toContain("exact_tool");
  expect(result.text).toContain("response.completed");
});

test("registry entry and model alias reach the upstream as the configured model", async () => {
  const result = await exchange({
    registry_entry: "ollama",
    models: ["ollama-model"],
    default_model: "ollama-model",
    model_aliases: { "ollama-model": "qwen3:8b" },
    api_key: undefined,
    auth_header: undefined,
    auth_value_prefix: undefined,
  }, sse([
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: "local" }, finish_reason: null }] }],
    [null, { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }],
    [null, "[DONE]"],
  ]), "ollama-model");
  expect(result.status).toBe(200);
  expect(result.observed.body.model).toBe("qwen3:8b");
  expect(result.observed.headers.get("authorization")).toBeNull();
  expect(result.text).toContain("local");
  expect(result.text).toContain("\"model\":\"ollama-model\"");
});

test("anthropic messages stream through the bridge", async () => {
  const result = await exchange({ adapter: "anthropic", base_url: "", auth_header: "x-api-key", auth_value_prefix: "" }, sse([
    ["message_start", { type: "message_start", message: { id: "msg_1", type: "message", role: "assistant", model: "test-model", content: [], stop_reason: null, usage: { input_tokens: 3, output_tokens: 0 } } }],
    ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
    ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "claude says hi" } }],
    ["content_block_stop", { type: "content_block_stop", index: 0 }],
    ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 4 } }],
    ["message_stop", { type: "message_stop" }],
  ]));
  expect(result.status).toBe(200);
  expect(result.observed.path).toBe("/v1/messages");
  expect(result.observed.headers.get("x-api-key")).toBe("test-secret");
  expect(result.text).toContain("claude says hi");
  expect(result.text).toContain("response.completed");
});

test("gemini generateContent streams through the bridge", async () => {
  const result = await exchange({ adapter: "google", base_url: "", auth_header: "x-goog-api-key", auth_value_prefix: "" }, sse([
    [null, { candidates: [{ index: 0, content: { role: "model", parts: [{ text: "gemini says hi" }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 3, candidatesTokenCount: 3, totalTokenCount: 6 } }],
  ]));
  expect(result.status).toBe(200);
  expect(result.observed.path).toBe("/v1beta/models/test-model:streamGenerateContent");
  expect(result.observed.search).toBe("?alt=sse");
  expect(result.text).toContain("gemini says hi");
  expect(result.text).toContain("response.completed");
});

for (const adapter of ["openai-responses", "azure"]) {
  test(`${adapter} Responses stream through the bridge`, async () => {
    const item = { id: "msg_1", type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text: "responses says hi", annotations: [] }] };
    const result = await exchange({ adapter, ...(adapter === "azure" ? { base_url: "/openai", auth_header: "api-key", auth_value_prefix: "" } : {}) }, sse([
      ["response.created", { type: "response.created", response: { id: "resp_1", object: "response", status: "in_progress", model: "test-model", output: [] } }],
      ["response.output_item.added", { type: "response.output_item.added", output_index: 0, item: { ...item, status: "in_progress", content: [] } }],
      ["response.content_part.added", { type: "response.content_part.added", item_id: "msg_1", output_index: 0, content_index: 0, part: { type: "output_text", text: "", annotations: [] } }],
      ["response.output_text.delta", { type: "response.output_text.delta", item_id: "msg_1", output_index: 0, content_index: 0, delta: "responses says hi" }],
      ["response.output_text.done", { type: "response.output_text.done", item_id: "msg_1", output_index: 0, content_index: 0, text: "responses says hi" }],
      ["response.content_part.done", { type: "response.content_part.done", item_id: "msg_1", output_index: 0, content_index: 0, part: item.content[0] }],
      ["response.output_item.done", { type: "response.output_item.done", output_index: 0, item }],
      ["response.completed", { type: "response.completed", response: { id: "resp_1", object: "response", status: "completed", model: "test-model", output: [item], usage: { input_tokens: 3, output_tokens: 3, total_tokens: 6 } } }],
    ]));
    expect(result.status).toBe(200);
    expect(result.observed.path).toBe(adapter === "azure" ? "/openai/v1/responses" : "/v1/responses");
    expect(result.observed.headers.get(adapter === "azure" ? "api-key" : "authorization")).toBe(adapter === "azure" ? "test-secret" : "Bearer test-secret");
    expect(result.text).toContain("responses says hi");
    expect(result.text).toContain("response.completed");
  });
}

test("provider descriptors match providers.json and the pinned OpenCodex registry", () => {
  const result = Bun.spawnSync([process.execPath, "run", "scripts/sync-providers.ts", "--check"], { cwd: resolve(import.meta.dir, "..") });
  expect(new TextDecoder().decode(result.stderr)).toBe("");
  expect(result.exitCode).toBe(0);
});
