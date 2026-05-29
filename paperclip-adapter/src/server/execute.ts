/**
 * Drive thClaws as a sovereign agent via its `/agent/run` endpoint.
 *
 * Flow per `dev-plan/25-thclaws-as-agent.md`:
 *   1. Resolve the per-agent workspace_dir from config.
 *   2. Materialize thcompany-managed state (skills, AGENT.md, MCP
 *      config) into that workspace dir.
 *   3. Spawn / reuse the singleton `thclaws --serve` daemon.
 *   4. POST `/agent/run` with `{workspace_dir, prompt, ...}` — the
 *      daemon discovers skills + MCP from the workspace at request
 *      time and runs the full agent loop with them in scope.
 *   5. Parse native SSE events (text / tool_use_* / skill_invoked /
 *      usage / result / error / [DONE]); emit to the orchestrator's
 *      transcript via ctx.onLog.
 *
 * The OpenAI-shaped /v1/chat/completions path is no longer used here
 * — external clients (Cursor, Aider, n8n) still hit that endpoint
 * directly. paperclip-adapter treats thClaws like claude-code: an
 * agent peer with filesystem-shaped configuration.
 */
import type {
  AdapterExecutionContext,
  AdapterExecutionResult,
} from "@paperclipai/adapter-utils";
import { acquireAgentLock } from "./agent-mutex.js";
import { runAgentRun, type XCallback } from "./http-client.js";
import {
  extractMaterializeInput,
  materializeAgentWorkspace,
} from "./materialize-workspace.js";
import { tokensToCostUsd } from "./pricing.js";
import { getLocalThclawsEndpoint } from "./spawn-lifecycle.js";

function asString(config: Record<string, unknown>, key: string, fallback: string): string {
  const v = config[key];
  return typeof v === "string" && v.trim().length > 0 ? v : fallback;
}

function asOptionalString(config: Record<string, unknown>, key: string): string | null {
  const v = config[key];
  return typeof v === "string" && v.trim().length > 0 ? v : null;
}

function asOptionalNumber(config: Record<string, unknown>, key: string): number | null {
  const v = config[key];
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function asEnvRecord(config: Record<string, unknown>, key: string): Record<string, string> {
  const v = config[key];
  if (!v || typeof v !== "object" || Array.isArray(v)) return {};
  const out: Record<string, string> = {};
  for (const [k, val] of Object.entries(v)) {
    if (typeof val === "string" && val.length > 0) out[k] = val;
  }
  return out;
}

export async function execute(
  ctx: AdapterExecutionContext,
): Promise<AdapterExecutionResult> {
  const config = (ctx.config ?? {}) as Record<string, unknown>;

  const command = asString(config, "command", "thclaws");
  const cwd = asString(config, "cwd", process.cwd());
  const model = asString(config, "model", "claude-sonnet-4-6");
  const systemPrompt = asOptionalString(config, "systemPrompt");
  const sessionId = asOptionalString(config, "sessionId");
  const temperature = asOptionalNumber(config, "temperature");
  const maxTokens = asOptionalNumber(config, "maxTokens");
  const mode = asString(config, "mode", "sync");
  const xCallback = mode === "async" ? deriveXCallback(ctx, model) : null;
  const envOverrides = asEnvRecord(config, "env");

  const materializeInput = extractMaterializeInput(ctx);
  if (!materializeInput) {
    return {
      exitCode: 1,
      signal: null,
      timedOut: false,
      errorCode: "missing_workspace_dir",
      errorFamily: null,
      errorMessage:
        "thclaws_local execute() requires `config.workspaceDir` (absolute path to the per-agent workspace). The orchestrator's adapter spec hasn't populated it.",
      model,
    };
  }

  // User prompt: same join order as before, just with task/wake/handoff
  // sections folded into a single user message. The persistent
  // instructions live in materializeInput.agentInstructions → AGENT.md.
  const ctxAny = ctx.context as Record<string, unknown>;
  const promptSections = [
    asString(ctxAny, "wakePrompt", ""),
    asString(ctxAny, "paperclipSessionHandoffMarkdown", ""),
    asString(ctxAny, "paperclipTaskMarkdown", ""),
    asString(ctxAny, "prompt", ""),
  ]
    .map((s) => s.trim())
    .filter((s) => s.length > 0);

  if (promptSections.length === 0) {
    return {
      exitCode: 1,
      signal: null,
      timedOut: false,
      errorCode: "empty_prompt",
      errorFamily: null,
      errorMessage:
        "thclaws_local execute() got no prompt content — context had no wakePrompt, paperclipSessionHandoffMarkdown, paperclipTaskMarkdown, or .prompt field.",
      model,
    };
  }

  const userPrompt = promptSections.join("\n\n");

  await ctx.onMeta?.({
    adapterType: "thclaws_local",
    command: `${command} --serve (via /agent/run)`,
    cwd,
    prompt: userPrompt,
    context: {
      model,
      transport: "local-http",
      streaming: true,
      workspaceDir: materializeInput.workspaceDir,
    },
  });

  // Per-agent mutex (dev-plan/25 Phase D): serializes the
  // materialize → POST window so a second concurrent run of the same
  // agent doesn't overwrite skills the first run is about to read.
  // thcompany's withAgentStartLock + maxConcurrentRuns provides the
  // primary serialization; this is defense in depth for the in-process
  // adapter boundary.
  const releaseAgentLock = await acquireAgentLock(ctx.agent.id);

  let materialized;
  try {
    materialized = await materializeAgentWorkspace(materializeInput);
  } catch (e) {
    releaseAgentLock();
    const msg = e instanceof Error ? e.message : String(e);
    return {
      exitCode: 1,
      signal: null,
      timedOut: false,
      errorCode: "materialize_failed",
      errorFamily: null,
      errorMessage: `Could not materialize workspace ${materializeInput.workspaceDir}: ${msg}`,
      model,
    };
  }
  if (materialized.skillsRemoved.length > 0) {
    await ctx.onLog(
      "stderr",
      `[materialize] removed stale skills: ${materialized.skillsRemoved.join(", ")}\n`,
    );
  }

  // Lazy spawn the daemon. Singleton per (command, cwd, env) tuple —
  // see spawn-lifecycle.ts.
  let endpoint;
  try {
    endpoint = await getLocalThclawsEndpoint({ command, cwd, env: envOverrides });
  } catch (e) {
    releaseAgentLock();
    const msg = e instanceof Error ? e.message : String(e);
    return {
      exitCode: 1,
      signal: null,
      timedOut: false,
      errorCode: "spawn_failed",
      errorFamily: null,
      errorMessage: `Could not start thclaws --serve: ${msg}`,
      model,
    };
  }

  let result;
  try {
    result = await runAgentRun({
      baseUrl: endpoint.baseUrl,
      bearerToken: endpoint.bearerToken,
      workspaceDir: materializeInput.workspaceDir,
      prompt: userPrompt,
      model,
      systemPrompt,
      sessionId,
      temperature,
      maxTokens,
      onLogStdout: (chunk) => ctx.onLog("stdout", chunk),
      onLogStderr: (chunk) => ctx.onLog("stderr", chunk),
      ...(xCallback ? { xCallback } : {}),
    });
  } finally {
    releaseAgentLock();
  }

  // dev-plan/25 Phase B': session continuation. The thClaws server
  // emits a `session` SSE event (sync path) or carries `session_id` on
  // the 202 ACK (async path); surface it through the codec-shaped
  // `sessionParams` so the orchestrator persists it for the next turn.
  const sessionParams = result.sessionId
    ? { sessionId: result.sessionId }
    : null;
  const sessionDisplayId = result.sessionId
    ? result.sessionId.slice(0, 8)
    : null;

  if (result.asyncAccepted) {
    await ctx.onLog(
      "stderr",
      `[async] thClaws accepted run ${result.acceptedRunId ?? "(unknown)"} — awaiting webhook callback at ${xCallback?.url}\n`,
    );
    return {
      exitCode: null,
      signal: null,
      timedOut: false,
      status: "running_async",
      model,
      ...(sessionParams ? { sessionParams, sessionDisplayId } : {}),
    } as AdapterExecutionResult & { status: "running_async" };
  }

  if (result.ok) {
    const costUsd =
      result.usage !== undefined
        ? tokensToCostUsd(model, {
            prompt_tokens: result.usage.inputTokens,
            completion_tokens: result.usage.outputTokens,
            cached_input_tokens: result.usage.cachedInputTokens,
            cache_creation_tokens: result.usage.cacheCreationInputTokens,
            reasoning_tokens: result.usage.reasoningOutputTokens,
          })
        : null;
    return {
      exitCode: 0,
      signal: null,
      timedOut: false,
      model,
      summary: result.summary,
      usage: result.usage,
      ...(costUsd !== null ? { costUsd } : {}),
      ...(sessionParams ? { sessionParams, sessionDisplayId } : {}),
    };
  }

  return {
    exitCode: 1,
    signal: null,
    timedOut: false,
    errorCode: result.errorCode,
    errorFamily: result.upstreamError ? "transient_upstream" : null,
    errorMessage: result.errorMessage,
    model,
    summary: result.summary,
    usage: result.usage,
    ...(sessionParams ? { sessionParams, sessionDisplayId } : {}),
  };
}

function deriveXCallback(
  ctx: AdapterExecutionContext,
  model: string,
): XCallback | null {
  const configEnv = asEnvRecord(ctx.config as Record<string, unknown>, "env");
  const url = (configEnv.PAPERCLIP_API_URL ?? process.env.PAPERCLIP_API_URL ?? "").trim();
  const apiKey = (configEnv.PAPERCLIP_API_KEY ?? process.env.PAPERCLIP_API_KEY ?? "").trim();
  const runId = (configEnv.PAPERCLIP_RUN_ID ?? process.env.PAPERCLIP_RUN_ID ?? "").trim();
  const missing: string[] = [];
  if (!url) missing.push("PAPERCLIP_API_URL");
  if (!apiKey) missing.push("PAPERCLIP_API_KEY");
  if (!runId) missing.push("PAPERCLIP_RUN_ID");
  if (missing.length > 0) {
    void ctx.onLog(
      "stderr",
      `[async] mode=async requested but ${missing.join(", ")} not set — falling back to sync (model=${model})\n`,
    );
    return null;
  }
  let callbackUrl = url!;
  try {
    const parsed = new URL(callbackUrl);
    if (parsed.pathname === "" || parsed.pathname === "/") {
      parsed.pathname = `/api/runs/${runId!}/complete`;
      callbackUrl = parsed.toString();
    }
  } catch {
    // surface invalid URL via thClaws's validation downstream
  }
  return { url: callbackUrl, apiKey: apiKey!, runId: runId! };
}
