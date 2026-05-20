# `GET /v1/agent/info` — capability snapshot

Read-only endpoint that returns this daemon's capability snapshot —
skills, MCP servers, model catalogue, version, optional external
URL, feature flags. Used by orchestrators that treat thClaws as a
sovereign agent (the Freelancer model — see
[`dev-plan/26-thclaws-pod-as-freelancer.md`](../dev-plan/26-thclaws-pod-as-freelancer.md))
so they can show what the freelancer is configured with without
having pushed any of it.

For the corresponding *invocation* endpoint, see
[`agent-endpoint.md`](agent-endpoint.md) (`POST /agent/run`). For the
OpenAI-compatible chat surface, see [`openai-api.md`](openai-api.md).

## Why this exists

dev-plan/25 (`thclaws_local`) and dev-plan/26 (`thclaws_pod`) treat
thClaws differently:

- An **Employee** (`thclaws_local`) shares the orchestrator's
  filesystem. thcompany materializes skills/MCP/instructions into
  the agent's workspace before each run — thcompany knows what it
  pushed.
- A **Freelancer** (`thclaws_pod`) lives anywhere with HTTPS
  reachability — own pod, cloud VPS, laptop. thcompany doesn't
  manage the freelancer's toolkit. To still show "this agent has
  these skills" in the UI, thcompany polls
  `GET /v1/agent/info` periodically and caches the snapshot.

The endpoint is also useful for operators verifying a fresh pod
("does the daemon I just spun up see my skills?") and for any
external tooling that needs to enumerate capabilities before
dispatching work.

## Request

```http
GET /v1/agent/info
Authorization: Bearer <THCLAWS_API_TOKEN>
```

No body, no query params. Pure read.

## Response

```json
{
  "version": "0.11.0",
  "git_sha": "b6c3773",
  "git_dirty": false,
  "build_profile": "release",
  "workspace_dir": "/workspace",
  "skills": [
    {
      "key": "deploy",
      "name": "deploy",
      "description": "Deploy this repo to staging",
      "when_to_use": "When the user asks to deploy or ship a build",
      "source": "project"
    }
  ],
  "mcp_servers": [
    {
      "name": "filesystem",
      "command": "mcp-server-filesystem /workspace",
      "tool_count": null
    }
  ],
  "model_capabilities": {
    "default_model": "claude-sonnet-4-6",
    "available_models": [
      "claude-haiku-4-5",
      "claude-sonnet-4-6",
      "gpt-5-pro"
    ],
    "supports_streaming": true,
    "supports_x_callback": true,
    "supports_agent_run": true
  },
  "external_access": {
    "ui_url": "https://agent-abc.tenant.thcompany.ai",
    "configured": true
  },
  "features": {
    "agent_info": true,
    "agent_run": true,
    "chat_completions": true,
    "x_callback": true
  }
}
```

### Field reference

| Field | Type | Notes |
|---|---|---|
| `version` | string | `Cargo.toml` package version. |
| `git_sha` | string | Short commit hash at build time, or `"unknown"` if the build environment had no git. |
| `git_dirty` | bool | `true` when the working tree had uncommitted changes at build time. |
| `build_profile` | string | `"debug"` or `"release"`. |
| `workspace_dir` | string | Daemon's CWD at start. For a pod that's typically `/workspace`. For a `thclaws_local` subprocess it's whatever the parent paperclip-adapter set. |
| `skills[]` | array | Same shape `SkillStore::discover()` produces. `source` is bucketed into `"builtin" \| "user" \| "plugin" \| "project"`. |
| `mcp_servers[]` | array | Configured MCP servers (config + plugin contributions, name-deduped). `command` is the spawn command summary or the URL (for HTTP-transport servers). `tool_count` is **`null` in v1** — counting tools requires spawning each server, which is too expensive for an info endpoint. The field is reserved for a future enrichment pass that populates it post-first-run. |
| `model_capabilities.default_model` | string | `AppConfig.model` — the daemon's configured default. |
| `model_capabilities.available_models[]` | array | Every chat-capable model id in the effective catalogue (cache layer + baseline, deduped, sorted). |
| `model_capabilities.supports_streaming` | bool | Always `true` in v1; reserved for future fallback signaling. |
| `model_capabilities.supports_x_callback` | bool | Always `true` in v1. |
| `model_capabilities.supports_agent_run` | bool | Always `true` in v1. |
| `external_access` | object \| null | Populated from `$THCLAWS_EXTERNAL_URL` (operator-set at daemon launch). `null` when unset — the UI then falls back to the OpenAI-compatible base URL. |
| `features.agent_info` | bool | `true` since dev-plan/26 Phase A. |
| `features.agent_run` | bool | `true` since dev-plan/25 Phase A. |
| `features.chat_completions` | bool | `true` since dev-plan/19. |
| `features.x_callback` | bool | `true` since dev-plan/23. |

Unknown fields a caller doesn't recognize should be ignored
(forward-compat — we may add fields like `policies[]`, `plugins[]`,
or `version_capabilities` in later iterations).

## Caching

The handler caches the snapshot for **30 seconds**. A
thcompany page that fans out to N pods on every refresh shouldn't
melt them. The cache is process-global (one slot per daemon, not
per-caller) — back-to-back polls within the window return the same
content.

If you need an unconditional fresh read, wait 30s or restart the
daemon. There's no `?force=1` query param by design — operators
have other tools (curl + restart) for diagnostic forcing.

## Auth

Same Bearer extractor as `/v1/chat/completions` and `/agent/run`.
Three modes via `THCLAWS_API_TOKEN`:

- Unset → endpoint returns 404 (API disabled).
- `disable-auth` → no header required (loopback-only; enforced at
  server start).
- `<value>` → `Authorization: Bearer <value>` with constant-time
  compare.

## Error codes

| HTTP | Meaning |
|---|---|
| `200` | Success (cached or freshly built). |
| `401` | Bearer token mismatch. |
| `404` | `THCLAWS_API_TOKEN` unset — `/v1/*` is disabled on this daemon. |

The endpoint has no body validation, so there's no `400` path.
Server errors return `500` only if the underlying skill scan or
catalogue load throws (very unlikely — both are reads from files
the daemon already loaded at startup).

## Worked example

```sh
# Start the daemon
export THCLAWS_API_TOKEN=secret-xyz
export THCLAWS_EXTERNAL_URL=https://agent.example.com
thclaws --serve --bind 127.0.0.1 --port 8443

# Poll capability info
curl -s -H 'Authorization: Bearer secret-xyz' \
  http://127.0.0.1:8443/v1/agent/info | jq .

# Returns the snapshot shape above. Add a skill to ~/.thclaws/skills/
# and re-poll within 30s → SAME response (cache). Wait 30s or
# restart → fresh response reflecting the new skill.
```

## How thcompany consumes this

thcompany's `agentPodCapabilitiesService.pollAndStore(agentId)`
(`thcompany/server/src/services/agent-pod-capabilities.ts`) hits
this endpoint, persists the snapshot on the agent row
(`agents.pod_capabilities` JSONB), and surfaces it in the agent's
"Capabilities" UI card. See
[`adapter-thclaws-pod.md`](../thcompany-technical-manual/adapter-thclaws-pod.md)
for the orchestrator side. The TypeScript client lives at
`thcompany/packages/adapters/thclaws-pod/src/server/info-poll.ts`
with a tolerant validator + specific error codes
(`transport_unreachable`, `invalid_api_key`, `endpoint_unavailable`,
`parse_failed`, `http_<status>`).

## See also

- [`agent-endpoint.md`](agent-endpoint.md) — `POST /agent/run` (invocation surface).
- [`openai-api.md`](openai-api.md) — `POST /v1/chat/completions` (external-client surface).
- [`model-catalogue.md`](model-catalogue.md) — `available_models[]` source of truth.
- [`../dev-plan/26-thclaws-pod-as-freelancer.md`](../dev-plan/26-thclaws-pod-as-freelancer.md) — the Freelancer architecture this endpoint enables.

## Implementation pointers

- Handler + caching: [`crates/core/src/api_v1/info.rs`](../thclaws/crates/core/src/api_v1/info.rs)
- Skill source classification: `classify_skill_source()` in the same file.
- Cache: `tokio::sync::Mutex<OnceLock<Option<Cached>>>`, TTL = 30s.
- Tests: 4 unit tests in the same module — covers source classification, snapshot structure, cache identity within TTL, `THCLAWS_EXTERNAL_URL` env round-trip.
