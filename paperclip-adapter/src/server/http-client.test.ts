/**
 * Smoke tests for runAgentRun's native SSE event parsing.
 *
 * No real thClaws daemon — we stub global.fetch with a synthetic SSE
 * stream and verify the parser routes events to the right onLog
 * channels + builds the right RunAgentRunResult.
 *
 * Run: `node --test --import tsx src/server/http-client.test.ts`
 */
import test from "node:test";
import assert from "node:assert/strict";
import { runAgentRun } from "./http-client.js";

type CaptureLog = { stdout: string[]; stderr: string[] };

function captureLogs(): { log: CaptureLog; onLogStdout: (s: string) => void; onLogStderr: (s: string) => void } {
  const log: CaptureLog = { stdout: [], stderr: [] };
  return {
    log,
    onLogStdout: (s) => {
      log.stdout.push(s);
    },
    onLogStderr: (s) => {
      log.stderr.push(s);
    },
  };
}

function sseStream(events: string[]): Response {
  const body = events.join("");
  const stream = new ReadableStream({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(body));
      controller.close();
    },
  });
  return new Response(stream, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  });
}

function stubFetch(response: Response | (() => Response)): () => void {
  const original = globalThis.fetch;
  globalThis.fetch = async () =>
    typeof response === "function" ? response() : response;
  return () => {
    globalThis.fetch = original;
  };
}

test("text events accumulate into summary + stream to stdout", async () => {
  const restore = stubFetch(
    sseStream([
      'event: text\ndata: {"delta":"Hello "}\n\n',
      'event: text\ndata: {"delta":"world."}\n\n',
      'event: usage\ndata: {"prompt_tokens":10,"completion_tokens":2}\n\n',
      'event: result\ndata: {"model":"claude-sonnet-4-6","stop_reason":"stop"}\n\n',
      "data: [DONE]\n\n",
    ]),
  );
  const { log, onLogStdout, onLogStderr } = captureLogs();
  const result = await runAgentRun({
    baseUrl: "http://stub",
    bearerToken: "t",
    workspaceDir: "/tmp/agent",
    prompt: "say hi",
    model: null,
    systemPrompt: null,
    sessionId: null,
    temperature: null,
    maxTokens: null,
    onLogStdout,
    onLogStderr,
  });
  restore();
  assert.equal(result.ok, true);
  assert.equal(result.summary, "Hello world.");
  assert.equal(result.stopReason, "stop");
  assert.deepEqual(log.stdout, ["Hello ", "world."]);
  assert.equal(result.usage?.inputTokens, 10);
  assert.equal(result.usage?.outputTokens, 2);
});

test("skill_invoked routes to stderr as [tool] line", async () => {
  const restore = stubFetch(
    sseStream([
      'event: skill_invoked\ndata: {"id":"abc","name":"pdf","input":{"file":"x.pdf"}}\n\n',
      'event: skill_invoked_result\ndata: {"id":"abc","name":"pdf","status":"ok","output":"done"}\n\n',
      'event: result\ndata: {"stop_reason":"stop"}\n\n',
      "data: [DONE]\n\n",
    ]),
  );
  const { log, onLogStdout, onLogStderr } = captureLogs();
  await runAgentRun({
    baseUrl: "http://stub",
    bearerToken: "t",
    workspaceDir: "/tmp/agent",
    prompt: "x",
    model: null,
    systemPrompt: null,
    sessionId: null,
    temperature: null,
    maxTokens: null,
    onLogStdout,
    onLogStderr,
  });
  restore();
  assert.equal(log.stdout.length, 0, "no stdout for skill-only run");
  assert.equal(log.stderr.length, 2);
  assert.match(log.stderr[0], /\[tool\] /);
  assert.match(log.stderr[0], /"name":"pdf"/);
  assert.match(log.stderr[0], /"status":"started"/);
  assert.match(log.stderr[1], /"status":"completed"/);
});

test("tool_use events route to stderr with name preserved", async () => {
  const restore = stubFetch(
    sseStream([
      'event: tool_use_start\ndata: {"id":"1","name":"Bash","input":{"cmd":"ls"}}\n\n',
      'event: tool_use_result\ndata: {"id":"1","name":"Bash","status":"ok","output":"file.txt"}\n\n',
      'event: result\ndata: {"stop_reason":"stop"}\n\n',
      "data: [DONE]\n\n",
    ]),
  );
  const { log, onLogStdout, onLogStderr } = captureLogs();
  await runAgentRun({
    baseUrl: "http://stub",
    bearerToken: "t",
    workspaceDir: "/tmp/agent",
    prompt: "x",
    model: null,
    systemPrompt: null,
    sessionId: null,
    temperature: null,
    maxTokens: null,
    onLogStdout,
    onLogStderr,
  });
  restore();
  assert.equal(log.stderr.length, 2);
  assert.match(log.stderr[0], /"name":"Bash"/);
});

test("error event yields ok:false with errorMessage", async () => {
  const restore = stubFetch(
    sseStream([
      'event: text\ndata: {"delta":"started "}\n\n',
      'event: error\ndata: {"message":"provider timeout"}\n\n',
      "data: [DONE]\n\n",
    ]),
  );
  const { log, onLogStdout, onLogStderr } = captureLogs();
  const result = await runAgentRun({
    baseUrl: "http://stub",
    bearerToken: "t",
    workspaceDir: "/tmp/agent",
    prompt: "x",
    model: null,
    systemPrompt: null,
    sessionId: null,
    temperature: null,
    maxTokens: null,
    onLogStdout,
    onLogStderr,
  });
  restore();
  assert.equal(result.ok, false);
  assert.equal(result.errorCode, "upstream_error");
  assert.match(result.errorMessage ?? "", /provider timeout/);
  assert.equal(result.summary, "started");
  // Even when the run errors, the partial stdout streams.
  assert.deepEqual(log.stdout, ["started "]);
});

test("transport failure surfaces transport_unreachable", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = async () => {
    throw new TypeError("fetch failed: ECONNREFUSED");
  };
  const { onLogStdout, onLogStderr } = captureLogs();
  const result = await runAgentRun({
    baseUrl: "http://localhost:1",
    bearerToken: "t",
    workspaceDir: "/tmp/agent",
    prompt: "x",
    model: null,
    systemPrompt: null,
    sessionId: null,
    temperature: null,
    maxTokens: null,
    onLogStdout,
    onLogStderr,
  });
  globalThis.fetch = original;
  assert.equal(result.ok, false);
  assert.equal(result.errorCode, "transport_unreachable");
  assert.equal(result.upstreamError, true);
});

test("400 returns invalid_workspace_dir error code", async () => {
  const restore = stubFetch(
    new Response("bad path", { status: 400 }),
  );
  const { onLogStdout, onLogStderr } = captureLogs();
  const result = await runAgentRun({
    baseUrl: "http://stub",
    bearerToken: "t",
    workspaceDir: "/bad",
    prompt: "x",
    model: null,
    systemPrompt: null,
    sessionId: null,
    temperature: null,
    maxTokens: null,
    onLogStdout,
    onLogStderr,
  });
  restore();
  assert.equal(result.errorCode, "invalid_workspace_dir");
});

test("async path returns 202 ACK + asyncAccepted", async () => {
  const restore = stubFetch(
    new Response(JSON.stringify({ run_id: "rid-xyz", status: "accepted" }), {
      status: 202,
      headers: { "content-type": "application/json" },
    }),
  );
  const { onLogStdout, onLogStderr } = captureLogs();
  const result = await runAgentRun({
    baseUrl: "http://stub",
    bearerToken: "t",
    workspaceDir: "/tmp/agent",
    prompt: "x",
    model: null,
    systemPrompt: null,
    sessionId: null,
    temperature: null,
    maxTokens: null,
    onLogStdout,
    onLogStderr,
    xCallback: { url: "http://receiver/cb", apiKey: "k", runId: "rid-xyz" },
  });
  restore();
  assert.equal(result.ok, true);
  assert.equal(result.asyncAccepted, true);
  assert.equal(result.acceptedRunId, "rid-xyz");
});
