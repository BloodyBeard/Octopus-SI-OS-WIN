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

function config(baseUrl: string) {
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
