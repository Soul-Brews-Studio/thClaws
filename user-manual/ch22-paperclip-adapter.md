# Chapter 22 — Paperclip adapter (retired)

**This chapter was removed in v0.110.0.** It documented
`@thclaws/paperclip-adapter`, an npm package that let a
[Paperclip](https://paperclip.ai) orchestration hire a thClaws agent as one
of its runtimes. The product line it served has been retired, and the
adapter's source no longer ships in this repository.

The page is kept as a stub so existing links don't break.

## If you came here to drive thClaws from another system

That still works — it was never specific to Paperclip. thClaws exposes two
HTTP surfaces under `thclaws --serve`, and any orchestrator, scheduler, or CI
job can use them:

- **`POST /agent/run`** — the thClaws-native shape. Takes a prompt and an
  optional `workspace_dir`, runs the full skill / MCP / plugin / policy
  bootstrap scoped to that directory, and streams native events (tool calls,
  skill invocations) rather than pretending to be OpenAI tokens. Supports
  multi-turn continuation via `session_id` and fire-and-forget delivery via
  `x_callback`.
- **`POST /v1/chat/completions`** — OpenAI-compatible, for clients that
  already speak that protocol (Cursor, Aider, n8n, `openai-python`).

`GET /v1/agent/info` reports what a running daemon has available — skills,
MCP servers, model catalogue, version — so an orchestrator can show
capabilities it never pushed itself.

Full reference: `agent-endpoint.md`, `openai-api.md`, and
`agent-info-endpoint.md` in the technical manual.

## If you were using the npm package

It remains published on npm and installable, but it is unmaintained and no
longer built or tested from this repository. Point your integration at
`POST /agent/run` instead — it is the same capability without the wrapper.
