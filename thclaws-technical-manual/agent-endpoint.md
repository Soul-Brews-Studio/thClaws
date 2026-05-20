# `POST /agent/run` — agent endpoint

The thClaws-native HTTP surface for orchestrators that drive thClaws
as a sovereign agent peer (paperclip-adapter / thcompany / custom
schedulers), not as an OpenAI-compatible LLM endpoint.

For the OpenAI-compatible chat endpoint that external clients
(Cursor, Aider, n8n, openai-python) use, see
[`openai-api.md`](openai-api.md). Both endpoints live behind the same
`--serve` listener and the same `THCLAWS_API_TOKEN` bearer auth.

Background: [`dev-plan/25-thclaws-as-agent.md`](../dev-plan/25-thclaws-as-agent.md).

## Why a separate endpoint

`/v1/chat/completions` accepts `{model, messages, stream, …}` — the
OpenAI wire shape. That shape encodes a *model* call: "predict tokens
given messages." It has no place for skills, MCP servers, plugins, or
workspace context, so orchestrators using it cannot inject those
things into thClaws.

`/agent/run` takes `{prompt, workspace_dir, …}` and runs thClaws's
full agent runtime against the supplied workspace dir:

```
/agent/run
  → SkillStore::discover_in(workspace_dir)
  → MCP servers loaded (currently from AppConfig — workspace-driven is a follow-up)
  → plugins resolved (daemon-level for now)
  → Skill tool registered, catalog appended to system prompt
  → Agent::new(...).run_turn(prompt)
```

So if the orchestrator writes `<workspace_dir>/.thclaws/skills/<name>/SKILL.md`
files before calling, those skills are active for that request. Same
contract claude-code uses with `.claude/skills/`. (MCP and policy
files written into the same workspace are not yet consumed
per-request — see the comparison table for current status.)

## Request

```http
POST /agent/run
Authorization: Bearer <THCLAWS_API_TOKEN>
Content-Type: application/json

{
  "prompt":         "Summarize today's open PRs.",
  "workspace_dir":  "/abs/path/to/agent/workspace",
  "system":         "You are a careful release manager.",
  "model":          "claude-sonnet-4-6",
  "session_id":     "01J9PQ8R…",
  "stream":         true,
  "temperature":    0.2,
  "max_tokens":     8192,
  "x_callback":     null
}
```

### Field reference

| Field | Type | Required | Notes |
|---|---|---|---|
| `prompt` | string | yes | The user turn. Joined from wake/handoff/task markdown by the orchestrator. |
| `workspace_dir` | string | no | Absolute path. Daemon's `SkillStore::discover_in` reads `<dir>/.claude/skills/` + `<dir>/.thclaws/skills/`. Rejected with 400 if relative, missing, or outside `THCLAWS_AGENT_WORKSPACE_ROOT` when that env is set. **Optional since dev-plan/26 Phase B**: when omitted or empty, the daemon falls back to its own CWD. Freelancer-mode pods (dev-plan/26) leave it off; Employee-mode adapters (paperclip-adapter for `thclaws_local`) supply it. |
| `system` | string | no | Appended to thClaws's default system prompt. Does NOT replace the default — the default carries the tool-aware scaffolding the agent needs. |
| `model` | string | no | Override the daemon's configured default. Routes via the same `ProviderKind::detect` logic the chat endpoint uses. |
| `session_id` | string | no | Reserved for session resume across turns. Phase A on the server accepts but does not yet persist; phase B/C will. |
| `stream` | bool | no | `true` (default) → SSE response. `false` → wait for completion, return one JSON. Ignored when `x_callback` is present (async always returns 202). |
| `temperature` | float | no | Forwarded to the provider when honored. |
| `max_tokens` | u32 | no | Per-turn output ceiling. |
| `x_callback` | object | no | Fire-and-forget mode — see [Async mode](#async-mode-x_callback). |

Unknown fields are silently ignored (matches OpenAI tolerance — gives
forward-compatible room for `mcp_overrides`, `policy_overrides`, etc.
without versioning the endpoint).

### `workspace_dir` validation

When `workspace_dir` is **supplied**, hard rules enforced by
[`agent_runtime::validate_workspace_dir`][validate]:

- Must be absolute.
- Must exist + be a directory.
- Daemon canonicalizes the path; symlink traversal is followed once.
- If `THCLAWS_AGENT_WORKSPACE_ROOT=/abs/path` is set, the canonical
  workspace path must live inside it. Operators set this on the
  daemon's environment to prevent a misconfigured (or malicious)
  orchestrator from pointing thClaws at sensitive system paths.

Validation failures return `400 Bad Request` with an
`invalid_workspace_dir` error code.

When `workspace_dir` is **omitted** (dev-plan/26 Phase B freelancer
mode), the daemon uses its own current working directory and skips
the `THCLAWS_AGENT_WORKSPACE_ROOT` check — operators control the
daemon's CWD at launch time, which IS the safety boundary. The
caller's omission is interpreted as "use whatever you booted from",
which for a pod is typically `/workspace`.

[validate]: ../thclaws/crates/core/src/agent_runtime.rs

## Response — sync (`stream: false`)

```http
200 OK
Content-Type: application/json

{
  "model": "claude-sonnet-4-6",
  "workspace_dir": "/abs/path/to/agent/workspace",
  "summary": "...",
  "stop_reason": "stop",
  "iterations": 3,
  "usage": {
    "prompt_tokens": 1234,
    "completion_tokens": 567,
    "cached_input_tokens": 800,
    "cache_creation_input_tokens": 0,
    "reasoning_output_tokens": null
  }
}
```

Errors during the turn surface as `5xx` with an `OpenAiError` envelope.

## Response — SSE (`stream: true`, default)

```http
200 OK
Content-Type: text/event-stream

event: text
data: {"delta":"Hello "}

event: text
data: {"delta":"world."}

event: tool_use_start
data: {"id":"01J9…","name":"Bash","input":{"cmd":"ls"}}

event: tool_use_result
data: {"id":"01J9…","name":"Bash","status":"ok","output":"file.txt\n"}

event: skill_invoked
data: {"id":"01J9…","name":"Skill","input":{"name":"pdf"}}

event: skill_invoked_result
data: {"id":"01J9…","name":"Skill","status":"ok","output":"…skill body…"}

event: usage
data: {"prompt_tokens":1234,"completion_tokens":567,"cached_input_tokens":800}

event: result
data: {"model":"claude-sonnet-4-6","stop_reason":"stop"}

data: [DONE]
```

### Event types

| Event | When | Payload |
|---|---|---|
| `text` | Assistant text delta | `{delta: string}` |
| `thinking` | Reasoning-model `<think>` deltas | `{delta: string}` |
| `tool_use_start` | Tool about to be called | `{id, name, input}` |
| `tool_use_result` | Tool completed | `{id, name, status: "ok"\|"error", output}` |
| `tool_use_denied` | Permission denied by approver | `{id, name}` |
| `skill_invoked` | The `Skill` tool was called | `{id, name: "Skill", input}` |
| `skill_invoked_result` | `Skill` returned | `{id, name: "Skill", status, output}` |
| `usage` | Final token counts | `{prompt_tokens, completion_tokens, cached_input_tokens?, cache_creation_input_tokens?, reasoning_output_tokens?}` |
| `result` | Turn complete (terminal) | `{model, stop_reason}` |
| `error` | Mid-stream failure (terminal) | `{message: string}` |

Terminal sentinel: `data: [DONE]` follows `result`. When `error` is
emitted the stream closes immediately without `[DONE]`.

Skill invocations are tool calls under the hood (the registered
`Skill` tool). The endpoint detects `name === "Skill"` on
`ToolCallStart`/`ToolCallResult` and emits the distinct
`skill_invoked` / `skill_invoked_result` events so consumers can
render skill activity separately without parsing tool names.

## Async mode (`x_callback`)

Same envelope and semantics as
[`x_callback` on the chat endpoint](openai-api.md#x_callback) — pass
the same `{url, api_key, run_id}` and thClaws will:

1. Respond `202 Accepted` immediately:
   ```json
   {
     "run_id": "01J9PQ8R…",
     "status": "accepted",
     "model": "claude-sonnet-4-6",
     "workspace_dir": "/abs/path/to/agent/workspace"
   }
   ```
2. Run the agentic loop on a detached tokio task.
3. POST the terminal payload to `x_callback.url` (same
   `CallbackPayload` shape as the chat endpoint — receivers built for
   `/v1/chat/completions` x_callback work unchanged).

Retry policy (3 attempts over ~90s), JWT verification semantics, and
the `idempotency_key` field are identical to the chat endpoint.

## Error codes

| HTTP | Code | Meaning |
|---|---|---|
| `400` | `invalid_workspace_dir` | Path is relative / missing / outside `THCLAWS_AGENT_WORKSPACE_ROOT`. |
| `400` | `invalid_x_callback` | Malformed `x_callback` envelope. |
| `401` | `invalid_api_key` | Bearer token mismatch. |
| `404` | (no body) | `THCLAWS_API_TOKEN` unset — API disabled. |
| `500` | `internal_server_error` | Provider failure, agent panic, etc. |

Mid-stream failures land as an `event: error` in the SSE response,
not as an HTTP status — by that point response headers have already
been flushed.

## Comparison with `/v1/chat/completions`

| | `/agent/run` | `/v1/chat/completions` |
|---|---|---|
| Caller | thClaws orchestrators | OpenAI-compatible clients |
| Request shape | thClaws-native | OpenAI standard |
| `workspace_dir` | required | n/a |
| Skill discovery | per-request from `workspace_dir` (`SkillStore::discover_in`) | not loaded |
| MCP servers | currently daemon-level (`AppConfig.mcp_servers`); per-request workspace-driven plumbing is a documented follow-up | not loaded |
| System prompt | thClaws default + skill catalog + client `system` | thClaws default + client `system` |
| Tool calls in SSE | `event: tool_use_*` named events | `x_thclaws_tool_use` extension field on OpenAI chunks |
| Skill calls in SSE | `event: skill_invoked` distinct | folded into `x_thclaws_tool_use` |
| `x_callback` async | yes | yes (identical semantics) |
| Streaming | named SSE events + `[DONE]` | OpenAI chunks + `[DONE]` |

> **MCP status (2026-05): the per-request workspace path for MCP servers is wired on the adapter side — `materialize-workspace.ts` writes `<workspace_dir>/.thclaws/mcp.json` when the orchestrator supplies servers — but the thClaws daemon's `build_runtime_for_workspace` still loads MCP from `AppConfig.mcp_servers` only. The file is written defensively for the upcoming closer of this loop; until then, MCP server changes require a daemon restart, same as before dev-plan/25. Tracking under dev-plan/25 "Open questions".

External tools (Cursor, Aider) keep using `/v1/chat/completions`
unchanged. orchestrators that previously used the chat endpoint
should switch to `/agent/run` to gain skill / MCP / plugin
injection — paperclip-adapter does this in dev-plan/25 Phase B.

## Auth

Same bearer extractor as the chat endpoint. Three modes via
`THCLAWS_API_TOKEN`:

- Unset → `/agent/run` returns 404 (API disabled).
- `disable-auth` → no header required. Refused unless the listener is
  loopback-bound; enforced at server start, not per-request.
- `<value>` → `Authorization: Bearer <value>` with constant-time
  compare.

## Worked example

Start thClaws with workspace-root gating:

```sh
export THCLAWS_API_TOKEN=secret-xyz
export THCLAWS_AGENT_WORKSPACE_ROOT=/var/thcompany/agents
thclaws --serve --bind 127.0.0.1 --port 8443
```

Make a workspace and drop a skill in:

```sh
mkdir -p /var/thcompany/agents/agent-1/.thclaws/skills/deploy
cat > /var/thcompany/agents/agent-1/.thclaws/skills/deploy/SKILL.md <<'EOF'
---
name: deploy
description: Deploy this repo to staging
whenToUse: When the user asks to deploy or ship a build
---
Run `make deploy` in the project root.
EOF
```

Call the endpoint:

```sh
curl -N http://127.0.0.1:8443/agent/run \
  -H 'Authorization: Bearer secret-xyz' \
  -H 'Content-Type: application/json' \
  -d '{
    "prompt": "Ship the current branch to staging.",
    "workspace_dir": "/var/thcompany/agents/agent-1"
  }'
```

You should see `event: skill_invoked` for `deploy` in the SSE stream
followed by `event: tool_use_*` events for whatever bash commands the
skill body told the agent to run, then `event: result` + `[DONE]`.

## Implementation pointers

- Handler: [`crates/core/src/api_v1/agent.rs`](../thclaws/crates/core/src/api_v1/agent.rs)
- Runtime builder: [`crates/core/src/agent_runtime.rs`](../thclaws/crates/core/src/agent_runtime.rs)
- Workspace-scoped skill discovery: [`SkillStore::discover_in`](../thclaws/crates/core/src/skills.rs)
- Path validator: `agent_runtime::validate_workspace_dir`
- Async callback machinery (shared with chat endpoint): [`crates/core/src/api_v1/callback.rs`](../thclaws/crates/core/src/api_v1/callback.rs)

## See also

- [`openai-api.md`](openai-api.md) — `/v1/chat/completions` (the external-client endpoint).
- [`model-catalogue.md`](model-catalogue.md) — `/v1/models` pricing block (unchanged; shared between both endpoints).
- [`../dev-plan/25-thclaws-as-agent.md`](../dev-plan/25-thclaws-as-agent.md) — the architectural pivot this endpoint implements.
- thcompany side: [`thcompany-technical-manual/adapter-thclaws-local.md`](../thcompany-technical-manual/adapter-thclaws-local.md).
