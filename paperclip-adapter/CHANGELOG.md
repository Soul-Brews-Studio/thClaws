# Changelog

All notable changes to `@thclaws/paperclip-adapter`.

## 1.0.0 — TBD (dev-plan/21)

Full feature parity with paperclip's claude-local adapter:

### Added
- Long-lived `thclaws --serve` subprocess driven via OpenAI Chat
  Completions API instead of one-shot `thclaws -p` invocations
- Streaming text deltas (SSE) — incremental transcript updates
- Tool-use events (`x_thclaws_tool_use`) rendered in transcript
- Usage tallies (input/output tokens) for billing
- `listSkills` + `syncSkills` (mode: ephemeral) — Paperclip Skills
  tab now functional
- `supportsInstructionsBundle: true` + `instructionsPathKey:
  "instructionsFilePath"` — Paperclip Instructions tab pushes
  CLAUDE.md to the agent's cwd, thClaws picks it up natively via
  its settings layering
- `modelProfiles` — `cheap` preset → Haiku-class
- `sessionCodec` — opaque session-id pass-through for multi-turn
  resume scaffolding (richer resume lands when thClaws adds
  resume-by-id over /v1)
- Shared `http-client.ts` exports — can be reused by other
  thClaws-API-compatible adapters (e.g. remote-pod transport)

### Changed
- Adapter no longer parses plain-text stdout from `thclaws -p`. The
  flag `--output-format stream-json` was declared in the thClaws CLI
  but never wired through `run_print_mode`, so per-event parsing
  isn't actually available on that path. Going via `--serve` uses
  the OpenAI SSE format which DOES carry the structured events.

### Removed
- One-shot `thclaws -p prompt` invocation. The per-process singleton
  subprocess pattern replaces it.

## 0.1.0 — 2026-02 (initial)

Minimal stub. Spawned `thclaws -p prompt` per turn; parsed plain text.
