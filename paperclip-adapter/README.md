# @thclaws/paperclip-adapter

Paperclip adapter for [thClaws](https://github.com/thClaws/thClaws). Lets
you hire a thClaws agent inside a Paperclip company orchestration —
alongside Claude, Codex, Cursor, Gemini, and the other built-in adapters.

## What you get

- A **`thclaws_local`** adapter type that drives a long-lived
  `thclaws --serve` subprocess via its OpenAI-compatible Chat
  Completions API — streaming text deltas, tool-use events, usage
  tallies all surface in Paperclip's transcript.
- All 21 thClaws providers reachable by model id alone:
  `claude-sonnet-4-6`, `gpt-4o`, `chatgpt-codex/gpt-5.4`,
  `openrouter/anthropic/…`, `gemini-2.5-flash`, `qwen-max`, etc.
- Paperclip Skills tab works (`mode: ephemeral` — customer manages
  the underlying `.thclaws/skills/` files via thClaws's own UI).
- Paperclip Instructions bundle works (writes to a path thClaws
  natively reads — `.claude/CLAUDE.md`, `.thclaws/system.md`, etc.).
- Model profiles (`cheap` → Haiku-class).
- Multi-turn session continuity via `sessionCodec` (pass-through
  in v1; richer resume lands when thClaws adds resume-by-id over
  the `/v1` API).

## Install

```sh
npm install @thclaws/paperclip-adapter
```

Then in your Paperclip server's adapter registry:

```ts
import { createServerAdapter } from "@thclaws/paperclip-adapter/server";

registerAdapter(createServerAdapter());
```

The transport-agnostic HTTP client is also exposed for callers that
want to drive a remote `thclaws --serve` listener (e.g. a per-agent
pod over a tenant subdomain — see thClaws's thcompany SaaS).

## Runtime requirements

- A `thclaws` binary on `$PATH` (or `command` in adapterConfig).
  Releases at https://github.com/thClaws/thClaws/releases
- An upstream LLM API key in the environment thClaws can read
  (e.g. `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) — thClaws's own
  settings layering covers .env files, keychain, etc.
- Node ≥18 for native `fetch` + `ReadableStream`.

## How it works

On first execute(), the adapter lazily spawns
`thclaws --serve --bind 127.0.0.1 --port 0` as a process-wide
singleton, discovers the OS-assigned port from the daemon's banner,
polls `/healthz`, and caches the endpoint. Subsequent runs share
that endpoint over the OpenAI Chat Completions API. SIGTERM cleanup
is registered on the parent process.

For multi-tenant deployments where per-agent isolation matters
(separate `/workspace`, separate keychain, externally-reachable
`/v1/*` for Cursor/Aider connection), Paperclip orchestrators can
also drive a remote `thclaws --serve` pod over a public URL with
the same HTTP client — see thClaws's `thclaws_pod` adapter pattern.

## Configuration

Minimum agent config:

```json
{
  "adapterType": "thclaws_local",
  "model": "claude-sonnet-4-6"
}
```

Optional fields:

| Field | Default | Notes |
|---|---|---|
| `command` | `thclaws` | Binary path or name on $PATH |
| `cwd` | process.cwd() | Working dir the subprocess runs from. Determines where thClaws looks for `.claude/CLAUDE.md` + `.thclaws/skills/` |
| `model` | `claude-sonnet-4-6` | Any id thClaws's `ProviderKind::detect` recognizes |
| `systemPrompt` | none | Prepended as a `system` message |
| `temperature` | none | Forwarded if set |
| `maxTokens` | none | Forwarded if set |
| `instructionsFilePath` | none | Set if Paperclip pushes an instructions bundle — write target |

Full field list including the rich `agentConfigurationDoc` markdown
also lives in `src/index.ts` (displayed on the Paperclip agent-hire UI).

## Building

```sh
pnpm install
pnpm build
# Outputs dist/{index.js, server/*.js, ui-parser.js}
```

## Versioning

Follows thClaws release versions where major thClaws features touch
the adapter surface, otherwise independent patch versions for
adapter-only fixes.

## License

MIT — same as thClaws.
