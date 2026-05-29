# Chapter 22 — Paperclip adapter

Hire a thClaws agent inside a [Paperclip](https://paperclip.ai)
company orchestration — alongside Claude, Codex, Cursor, Gemini, and
the other built-in adapters. One config block + a model id, and
Paperclip can dispatch jobs to thClaws's full provider catalogue,
KMS, skills, MCP, and agent-team primitives.

Shipped in v0.9.5 as an external npm package — the adapter lives at
[`@thclaws/paperclip-adapter`](https://www.npmjs.com/package/@thclaws/paperclip-adapter),
not bundled with the desktop binary.

## Self-hosted sandbox / in-process sandbox

[Anthropic's Managed Agents](https://platform.claude.com/docs/en/managed-agents/self-hosted-sandboxes)
calls this pattern a *self-hosted sandbox*: the orchestrator runs the
agent loop upstream while tool execution happens inside the
customer's perimeter. thClaws fits the same shape — with two
deployment modes that map cleanly to Anthropic's terminology:

| Mode | What it is | Anthropic-equivalent term |
|---|---|---|
| **`thclaws_local`** (Employee) | A `thclaws -p` subprocess spawned per Paperclip run, sharing the host's filesystem + `.thclaws/`. | In-process sandbox |
| **`thclaws_pod`** (Freelancer) | A standalone `thclaws --serve` running on a VPS, k3s pod, or thcompany.ai instance. Orchestrator calls `/agent/run` over HTTPS. | Self-hosted sandbox |

Either way, **the agent loop runs inside *your* infrastructure** —
no model traffic is proxied through Paperclip / Anthropic infra
beyond what the upstream LLM provider would have seen anyway. The
distinction is whether tool execution shares the orchestrator's host
(Employee) or runs in a separate process / pod / cloud you control
(Freelancer). For the multi-tenant case, **Freelancer (`thclaws_pod`)
deployed to thcompany.ai is the turn-key option** — you don't bring
your own k3s, but you still own the per-tenant sandbox boundary.

## Why bother

- **Provider-flexible agent inside Paperclip.** Switch between
  Anthropic / OpenAI / Gemini / OpenRouter / DashScope / Codex
  subscription / 15+ others by changing one `model` field on the
  agent config. No per-provider Paperclip adapter to add for each.
- **Subscription-billed Codex.** The `chatgpt-codex/*` model ids
  route through your existing ChatGPT Plus / Pro / Team account
  (auto-imports the official Codex CLI's `~/.codex/auth.json`) —
  no extra OpenAI API key needed.
- **thClaws's tool surface for free.** Every run inside Paperclip
  has the agent's KMS, plan-mode, agent-teams, skills, MCP servers,
  and approval system available with no per-job config — just
  whatever you've set up in your project's `.thclaws/`.

## When to skip it

- Your Paperclip job specifically needs Claude Code's tool surface
  (use the `claude_local` adapter) or Codex CLI's session model
  (use `codex_local`). thClaws's tool registry doesn't cross
  subprocess boundaries from those wrappers — they're separate
  execution paths.
- (Session continuation is supported as of the `/agent/run` adapter
  path — see "Multi-turn sessions" below.)

## Prerequisites

1. **Paperclip with external-adapter plugin support** — the
   `adapter-plugin` Phase 1 changes. See your Paperclip docs.
2. **`thclaws` binary on `$PATH`** (or specify an absolute path
   in the agent config). Install:
   ```sh
   git clone https://github.com/thClaws/thClaws
   cd thClaws/crates/core && cargo install --path .
   ```
3. **At least one provider API key** reachable to thClaws — either
   in your shell env, in `~/.config/thclaws/.env`, or in the
   project's `.thclaws/.env`. The adapter doesn't manage thClaws
   credentials; it only spawns the binary.

## Install

In your Paperclip instance:

```sh
pnpm add @thclaws/paperclip-adapter
```

Then register it via the Paperclip plugin store (the exact
register / enable flow lives in Paperclip's own docs under
adapter plugins).

## Hire a thClaws agent

The minimum agent config:

```json
{
  "adapterType": "thclaws_local",
  "model": "claude-sonnet-4-6"
}
```

That's it. Paperclip's UI picker shows a curated short-list of
models (Claude Sonnet 4.6, Claude Opus 4.7, GPT-4o, Codex GPT-5.4,
a Qwen variant, a Gemini variant, an OpenRouter variant), but you
can type any model id thClaws's `ProviderKind::detect` recognizes —
`openrouter/anthropic/claude-3.5-sonnet`, `dashscope/qwen-max`,
`gemini-2.5-flash`, `chatgpt-codex/gpt-5.4`, etc.

## Agent config fields

| Field | Type | Default | Notes |
|---|---|---|---|
| `adapterType` | string | required | Must be `"thclaws_local"`. |
| `model` | string | `claude-sonnet-4-6` | Any thClaws-recognised model id. |
| `cwd` | string | Paperclip workspace cwd | Absolute working dir for the thClaws process. |
| `command` | string | `thclaws` | Override the binary path. Useful if you keep thClaws under a custom install prefix. |
| `extraArgs` | string[] | `[]` | Appended verbatim to the `thclaws -p` spawn. e.g. `["--max-tokens", "8000"]`. |
| `env` | object | `{}` | Per-agent env vars. Inject `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `DASHSCOPE_API_KEY` etc here rather than relying on the shell. thClaws's `.env` discovery layers over these. |
| `promptTemplate` | string | none | Optional template applied to the Paperclip-issued prompt before `thclaws -p` sees it. |
| `timeoutSec` | number | `0` (no adapter timeout) | Run timeout in seconds; Paperclip's job-level timeout still applies. |

## What the agent has access to

Inside every Paperclip run that dispatches to a `thclaws_local`
agent, thClaws gets its normal stack:

- **Permission policy** is read from the workspace's
  `.thclaws/settings.json` (or `~/.config/thclaws/settings.json`
  as fallback). Paperclip's job runner does NOT auto-approve
  mutating tools — set `"permissions": "auto"` in the project
  settings if you want approval-less runs (see
  [chapter 5](ch05-permissions.md)).
- **MCP servers** attached at the project (`.thclaws/mcp.json`)
  or user (`~/.config/thclaws/mcp.json`) level are available
  with no extra config (see [chapter 14](ch14-mcp.md)).
- **Skills, KMS, hooks, agent-teams** — same as the standalone
  CLI. The thClaws process runs to completion, then exits.

Output is captured from stdout / stderr verbatim. thClaws prints
the assistant text plus a one-line `[tokens: …]` summary at the
end; both flow back to Paperclip as the run transcript.

## Multi-turn sessions

The `/agent/run` path supports session continuation. On the first
turn omit `sessionId` — thClaws mints a fresh id and returns it:

- **Sync / streaming responses:** the first SSE event is
  `event: session\ndata: {"id": "sess-…"}`, emitted before any
  text deltas.
- **Async (`x_callback`) responses:** the 202 ACK carries
  `session_id` alongside `run_id`.

On subsequent turns pass that id back as `config.sessionId` (the
adapter forwards it to thClaws as `session_id`). The server loads
`<workspaceDir>/.thclaws/sessions/<id>.jsonl`, hydrates the agent's
history from it, runs the new turn, and persists the updated
history back to the same file. The same id is returned again so
the caller can keep feeding it forward.

If `sessionId` is supplied but no JSONL exists at that path,
thClaws returns 404 `session_not_found` rather than silently
minting a fresh session under your id — that prevents a typo from
masking as "the agent forgot everything."

## Limitations

- **No incremental tool-call rendering on the legacy `thclaws -p`
  path.** Stdout buffers until the process exits, then surfaces as
  a single transcript block. The `/agent/run` adapter path streams
  tool calls live via SSE.
- **The adapter doesn't manage thClaws credentials.** API keys
  come from env vars, `.env` files, or the OS keychain — whatever
  thClaws's normal lookup chain finds.

## See also

- [Chapter 6 — Providers, models & API keys](ch06-providers-models-api-keys.md)
- [Chapter 14 — MCP servers](ch14-mcp.md)
- [Chapter 17 — Agent teams](ch17-agent-teams.md)
- Technical manual:
  [`paperclip-adapter.md`](../thclaws-technical-manual/paperclip-adapter.md)
  for the adapter's internal contract + spawn semantics.
