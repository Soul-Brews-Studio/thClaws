/**
 * @thclaws/paperclip-adapter — entry point.
 *
 * Paperclip discovers external adapters by loading the package's main
 * entry, calling `createServerAdapter()`, and registering the returned
 * `ServerAdapterModule` in its mutable adapter registry. See
 * paperclip/docs/adapters/external-adapters.md for the full contract.
 *
 * This file MUST stay dependency-free (no node:fs / runtime side
 * effects) — Paperclip's UI imports the same module to read metadata
 * (type, label, models, agentConfigurationDoc) without executing
 * adapter logic.
 */

import type { AdapterModel } from "@paperclipai/adapter-utils";

export const type = "thclaws_local";
export const label = "thClaws (local)";

/**
 * Models surfaced in Paperclip's agent-hire flow. The thClaws catalogue
 * itself supports 20+ providers and hundreds of models, but the picker
 * needs a curated short-list. The user-typed `model` field on the
 * Paperclip agent config maps verbatim to `thclaws -m <model>`, so any
 * id thClaws's `ProviderKind::detect` recognizes also works — these
 * are just the named defaults that appear in the UI dropdown.
 */
export const models: AdapterModel[] = [
  { id: "claude-sonnet-4-6", label: "Claude Sonnet 4.6" },
  { id: "claude-opus-4-7", label: "Claude Opus 4.7" },
  { id: "claude-haiku-4-5", label: "Claude Haiku 4.5" },
  { id: "chatgpt-codex/gpt-5.4", label: "ChatGPT Codex gpt-5.4" },
  { id: "gpt-4o", label: "GPT-4o" },
  { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
];

export const agentConfigurationDoc = `# thclaws_local agent configuration

Adapter: thclaws_local

Wraps the thClaws Rust CLI as a Paperclip agent runtime. thClaws is
a 21-provider native-Rust agent (Anthropic, OpenAI, Gemini, Codex
subscription, DashScope, OpenRouter, NVIDIA NIM, …) with KMS,
plan-mode, agent-teams, skills, MCP, and a permission system the
adapter passes through unchanged.

## Use when

- You want a provider-flexible agent that switches between vendors
  by model id alone (\`claude-sonnet-4-6\`, \`gpt-4o\`,
  \`chatgpt-codex/gpt-5.4\`, \`openrouter/anthropic/...\`).
- You want to bill against an existing ChatGPT Plus / Pro / Team
  subscription via the \`chatgpt-codex/*\` models (auto-imports the
  official Codex CLI's auth file).
- You need a built-in MCP host with knowledge-base / skills /
  agent-team primitives without wiring those individually.

## Don't use when

- Your workflow specifically needs Claude Code's tool surface (use
  \`claude_local\`) or Codex CLI's session model (use
  \`codex_local\`). thClaws has its own tool registry that doesn't
  cross subprocess boundaries from those wrappers.
- You need persistent multi-turn sessions across runs. MVP wraps
  thClaws's one-shot print mode (\`thclaws -p\`); session
  continuation lands in a future version.

## Core fields

- \`model\` (string, optional): any model id thClaws's
  ProviderKind::detect recognizes. Defaults to
  \`claude-sonnet-4-6\`.
- \`cwd\` (string, optional): absolute working directory for the
  thClaws process. Defaults to the Paperclip-managed workspace
  cwd when available.
- \`command\` (string, optional): override the thClaws binary path.
  Defaults to \`thclaws\` (resolved via \`$PATH\`).
- \`extraArgs\` (string[], optional): additional CLI args appended
  to the spawn. Examples: \`["--verbose"]\`, \`["--max-tokens",
  "8000"]\`.
- \`env\` (object, optional): KEY=VALUE environment variables. Use
  this to inject \`OPENAI_API_KEY\`, \`ANTHROPIC_API_KEY\`,
  \`DASHSCOPE_API_KEY\` (and so on) per-agent rather than relying
  on the host shell. thClaws's normal \`.env\` discovery layers
  over these.
- \`promptTemplate\` (string, optional): run prompt template
  applied to the Paperclip-issued prompt before passing to
  \`thclaws -p\`.

## Operational fields

- \`timeoutSec\` (number, optional): run timeout in seconds.
  Defaults to 0 (no adapter timeout — Paperclip's job timeout
  still applies).
- \`mode\` (\`"sync"\` | \`"async"\`, optional, default \`"sync"\`):
  \`"async"\` opts the run into thClaws's \`x_callback\` extension —
  the adapter returns as soon as thClaws ACKs (202), and the final
  result lands at the orchestrator via webhook. Use this for long-
  running agentic loops where holding an SSE connection for 30+
  minutes risks idle timeouts or tenant pod restarts.

  Requires the orchestrator to set three env vars before invoking:
  \`PAPERCLIP_API_URL\` (callback origin or full URL),
  \`PAPERCLIP_API_KEY\` (Bearer the receiver verifies), and
  \`PAPERCLIP_RUN_ID\` (echoed in callback body for correlation).
  Missing any of these ⇒ the adapter logs to stderr and silently
  falls back to sync, so \`mode: "async"\` is never load-bearing.
  See \`dev-plan/23-thclaws-async-callback.md\` for the full
  end-to-end design.

## Notes

- Permission policy is read from the workspace's
  \`.thclaws/settings.json\` (or \`~/.config/thclaws/settings.json\`
  as fallback). Paperclip's job runner does NOT auto-approve
  mutating tools — set \`"permissions": "auto"\` in the project
  settings if you want approval-less runs.
- MCP servers attached at the project (\`.thclaws/mcp.json\`) or
  user (\`~/.config/thclaws/mcp.json\`) level are available
  inside each run with no additional config.
- Output is captured from stdout / stderr verbatim. thClaws
  prints assistant text + a one-line \`[tokens: …]\` summary at
  the end; the adapter surfaces both via Paperclip's transcript.
`;

// The plugin loader looks for `createServerAdapter` on the main entry.
// Defined here as a thin re-export so the metadata + factory live in
// one place.
export { createServerAdapter } from "./server/index.js";
