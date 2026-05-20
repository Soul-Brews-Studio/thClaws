/**
 * thClaws-native HTTP client for the `/agent/run` endpoint.
 *
 * Where `runChat` (now removed) wrapped `/v1/chat/completions` in an
 * OpenAI envelope, `runAgentRun` posts to thClaws's native agent
 * endpoint with an explicit `workspace_dir` and parses native SSE
 * events. See `dev-plan/25-thclaws-as-agent.md` for why this exists.
 *
 * Wire protocol: POST `{baseUrl}/agent/run` with `stream: true`.
 * Server emits named SSE events:
 *
 *   event: text
 *   data: { "delta": "..." }
 *
 *   event: tool_use_start
 *   data: { "id": "...", "name": "Bash", "input": {...} }
 *
 *   event: skill_invoked
 *   data: { "id": "...", "name": "Skill", "input": {...} }
 *
 *   event: usage
 *   data: { "prompt_tokens": ..., "completion_tokens": ..., ... }
 *
 *   event: result
 *   data: { "model": "...", "stop_reason": "..." }
 *
 *   event: error
 *   data: { "message": "..." }
 *
 *   data: [DONE]
 *
 * Tool calls + skill invocations are surfaced on stderr as JSON
 * lines so the existing transcript renderer keeps working — same
 * `[tool] {...}` shape claude-local emits.
 */

export interface XCallback {
  url: string;
  apiKey: string;
  runId: string;
  idempotencyKey?: string;
}

export interface RunAgentRunRequest {
  baseUrl: string;
  bearerToken: string;
  /** Absolute path to the per-agent workspace dir. REQUIRED. */
  workspaceDir: string;
  prompt: string;
  /** Optional model id override (server has its own default). */
  model: string | null;
  /** Optional extra system message — appended to thClaws's default. */
  systemPrompt: string | null;
  /** Reserved for session resume. Phase A on the server ignores it. */
  sessionId: string | null;
  temperature: number | null;
  maxTokens: number | null;
  onLogStdout: (chunk: string) => Promise<void> | void;
  onLogStderr: (chunk: string) => Promise<void> | void;
  /**
   * Opt into thClaws's x_callback async mode. When set, the server
   * returns 202 immediately and POSTs the terminal result to
   * `xCallback.url` when the agentic loop finishes. Caller is
   * responsible for the receiver side.
   */
  xCallback?: XCallback;
}

export interface RunAgentRunResult {
  ok: boolean;
  /** Concatenated assistant text deltas. */
  summary: string | null;
  /** thClaws-reported stop reason from the `result` event. */
  stopReason: string | null;
  usage?: {
    inputTokens: number;
    outputTokens: number;
    cachedInputTokens?: number;
    cacheCreationInputTokens?: number;
    reasoningOutputTokens?: number;
  };
  errorMessage?: string;
  errorCode?: string;
  upstreamError?: boolean;
  /** Set by the async path when thClaws ACK'd with 202. */
  asyncAccepted?: boolean;
  acceptedRunId?: string;
}

interface AgentRunEvent {
  name: string;
  data: unknown;
}

interface UsageEventPayload {
  prompt_tokens?: number;
  completion_tokens?: number;
  cached_input_tokens?: number;
  cache_creation_input_tokens?: number;
  reasoning_output_tokens?: number;
}

interface ResultEventPayload {
  model?: string;
  stop_reason?: string | null;
}

interface ErrorEventPayload {
  message?: string;
}

interface ToolEventPayload {
  id?: string;
  name?: string;
  input?: unknown;
  status?: string;
  output?: unknown;
}

export async function runAgentRun(
  req: RunAgentRunRequest,
): Promise<RunAgentRunResult> {
  const body: Record<string, unknown> = {
    prompt: req.prompt,
    workspace_dir: req.workspaceDir,
  };
  if (req.model && req.model.trim().length > 0) body.model = req.model;
  if (req.systemPrompt && req.systemPrompt.trim().length > 0) {
    body.system = req.systemPrompt;
  }
  if (req.sessionId && req.sessionId.trim().length > 0) {
    body.session_id = req.sessionId;
  }
  if (req.temperature !== null) body.temperature = req.temperature;
  if (req.maxTokens !== null) body.max_tokens = req.maxTokens;

  if (req.xCallback) {
    body.x_callback = {
      url: req.xCallback.url,
      api_key: req.xCallback.apiKey,
      run_id: req.xCallback.runId,
      ...(req.xCallback.idempotencyKey
        ? { idempotency_key: req.xCallback.idempotencyKey }
        : {}),
    };
  } else {
    body.stream = true;
  }

  let res: Response;
  try {
    res = await fetch(`${req.baseUrl}/agent/run`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: req.xCallback ? "application/json" : "text/event-stream",
        authorization: `Bearer ${req.bearerToken}`,
      },
      body: JSON.stringify(body),
    });
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return {
      ok: false,
      summary: null,
      stopReason: null,
      errorCode: "transport_unreachable",
      errorMessage: `Could not reach thClaws /agent/run at ${req.baseUrl}: ${msg}`,
      upstreamError: true,
    };
  }

  // Async branch: 202 ACK ⇒ run continues server-side, terminal
  // result lands at xCallback.url. We're done.
  if (req.xCallback) {
    if (res.status === 202) {
      let runId = req.xCallback.runId;
      try {
        const ack = (await res.json()) as { run_id?: string };
        if (typeof ack.run_id === "string" && ack.run_id.length > 0) {
          runId = ack.run_id;
        }
      } catch {
        // best-effort
      }
      return {
        ok: true,
        summary: null,
        stopReason: "accepted_async",
        asyncAccepted: true,
        acceptedRunId: runId,
      };
    }
    if (!res.ok) {
      const text = await res.text().catch(() => "(no body)");
      return {
        ok: false,
        summary: null,
        stopReason: null,
        errorCode: res.status === 401 ? "invalid_api_key" : `http_${res.status}`,
        errorMessage: `thClaws rejected async /agent/run: ${res.status} ${text.slice(0, 500)}`,
        upstreamError: res.status >= 500,
      };
    }
    return {
      ok: false,
      summary: null,
      stopReason: null,
      errorCode: "async_not_supported",
      errorMessage: `thClaws returned ${res.status} but async requested — server may not support x_callback`,
      upstreamError: false,
    };
  }

  // Sync (streaming) branch.
  if (!res.ok || !res.body) {
    const text = await res.text().catch(() => "(no body)");
    return {
      ok: false,
      summary: null,
      stopReason: null,
      errorCode:
        res.status === 400
          ? "invalid_workspace_dir"
          : res.status === 401
            ? "invalid_api_key"
            : `http_${res.status}`,
      errorMessage: `thClaws /agent/run returned ${res.status}: ${text.slice(0, 500)}`,
      upstreamError: res.status >= 500,
    };
  }

  const stdoutChunks: string[] = [];
  let stopReason: string | null = null;
  let errorMessage: string | null = null;
  let usage: RunAgentRunResult["usage"];

  try {
    for await (const ev of parseSseStream(res.body)) {
      switch (ev.name) {
        case "text": {
          const delta = readDelta(ev.data);
          if (delta) {
            stdoutChunks.push(delta);
            await req.onLogStdout(delta);
          }
          break;
        }
        case "thinking": {
          const delta = readDelta(ev.data);
          if (delta) {
            // Reasoning content stays on stderr (out of the visible
            // assistant transcript) but with a marker the UI can use.
            await req.onLogStderr(`[thinking] ${delta}`);
          }
          break;
        }
        case "tool_use_start":
        case "tool_use_result":
        case "tool_use_denied": {
          // Preserve the `[tool] {...}` shape claude-local and the
          // chat endpoint's x_thclaws_tool_use chunks both emit, so
          // existing transcript renderers don't have to change.
          const payload = ev.data as ToolEventPayload;
          const status =
            ev.name === "tool_use_start"
              ? "started"
              : ev.name === "tool_use_denied"
                ? "denied"
                : (payload.status ?? "completed");
          await req.onLogStderr(
            `[tool] ${JSON.stringify({
              id: payload.id,
              name: payload.name,
              status,
              input: payload.input,
              output: payload.output,
            })}\n`,
          );
          break;
        }
        case "skill_invoked":
        case "skill_invoked_result": {
          // Distinct event name on the wire, but render through the
          // same [tool] line shape — the renderer parses by name.
          const payload = ev.data as ToolEventPayload;
          const status = ev.name === "skill_invoked" ? "started" : "completed";
          await req.onLogStderr(
            `[tool] ${JSON.stringify({
              id: payload.id,
              name: payload.name ?? "Skill",
              status,
              input: payload.input,
              output: payload.output,
            })}\n`,
          );
          break;
        }
        case "usage": {
          const u = ev.data as UsageEventPayload;
          usage = {
            inputTokens: u.prompt_tokens ?? 0,
            outputTokens: u.completion_tokens ?? 0,
            ...(u.cached_input_tokens !== undefined
              ? { cachedInputTokens: u.cached_input_tokens }
              : {}),
            ...(u.cache_creation_input_tokens !== undefined
              ? { cacheCreationInputTokens: u.cache_creation_input_tokens }
              : {}),
            ...(u.reasoning_output_tokens !== undefined
              ? { reasoningOutputTokens: u.reasoning_output_tokens }
              : {}),
          };
          break;
        }
        case "result": {
          const r = ev.data as ResultEventPayload;
          stopReason = r.stop_reason ?? null;
          break;
        }
        case "error": {
          const e = ev.data as ErrorEventPayload;
          errorMessage = e.message ?? "(no message)";
          break;
        }
        default:
          // Unknown event — log to stderr for debugging but don't fail.
          await req.onLogStderr(
            `[agent] unknown event ${ev.name}: ${JSON.stringify(ev.data).slice(0, 200)}\n`,
          );
      }
    }
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return {
      ok: false,
      summary: stdoutChunks.join("") || null,
      stopReason,
      usage,
      errorCode: "sse_parse_failed",
      errorMessage: `SSE stream broke after ${stdoutChunks.length} chunks: ${msg}`,
      upstreamError: true,
    };
  }

  const summary = stdoutChunks.join("").trim() || null;

  if (errorMessage) {
    return {
      ok: false,
      summary,
      stopReason,
      usage,
      errorCode: "upstream_error",
      errorMessage,
      upstreamError: true,
    };
  }

  return {
    ok: true,
    summary,
    stopReason,
    usage,
  };
}

function readDelta(data: unknown): string | null {
  if (data && typeof data === "object") {
    const d = (data as { delta?: unknown }).delta;
    if (typeof d === "string" && d.length > 0) return d;
  }
  return null;
}

/**
 * SSE parser for thClaws's native event stream. Yields each `event: X`
 * + `data: {...}` pair as `{name, data}`. Returns on `data: [DONE]`.
 */
async function* parseSseStream(
  body: ReadableStream<Uint8Array>,
): AsyncIterable<AgentRunEvent> {
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buf = "";
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let idx: number;
    while ((idx = buf.indexOf("\n\n")) !== -1) {
      const rawEvent = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      const parsed = parseSseBlock(rawEvent);
      if (parsed === null) continue;
      if (parsed === "DONE") return;
      yield parsed;
    }
  }
  if (buf.trim().length > 0) {
    const parsed = parseSseBlock(buf);
    if (parsed && parsed !== "DONE") yield parsed;
  }
}

function parseSseBlock(raw: string): AgentRunEvent | "DONE" | null {
  let eventName = "message";
  const dataLines: string[] = [];
  for (const line of raw.split("\n")) {
    if (line.startsWith("event:")) {
      eventName = line.slice(6).trim();
    } else if (line.startsWith("data:")) {
      dataLines.push(line.slice(5).trimStart());
    }
  }
  if (dataLines.length === 0) return null;
  const dataStr = dataLines.join("\n");
  if (dataStr === "[DONE]") return "DONE";
  try {
    return { name: eventName, data: JSON.parse(dataStr) };
  } catch {
    return null;
  }
}
