/**
 * Session codec for thClaws.
 *
 * thClaws persists sessions per-cwd in `.thclaws/projects/<hash>/<id>.jsonl`
 * (mirroring Claude Code's `~/.claude/projects/...` layout — clean-room
 * compat). The OpenAI Chat Completions API is stateless per request,
 * so multi-turn resume in v1 relies on Paperclip's higher-layer turn
 * folding rather than passing a session id back to thClaws.
 *
 * This codec records the session id Paperclip wants to track but is
 * currently a pass-through — full resume semantics land alongside a
 * thClaws CLI subcommand for resume-by-id in a follow-up.
 */

import type { AdapterSessionCodec } from "@paperclipai/adapter-utils";

export const sessionCodec: AdapterSessionCodec = {
  deserialize(raw: unknown): Record<string, unknown> | null {
    if (!raw || typeof raw !== "object") return null;
    const obj = raw as Record<string, unknown>;
    if (typeof obj.sessionId !== "string" || obj.sessionId.length === 0) return null;
    return { sessionId: obj.sessionId };
  },
  serialize(params: Record<string, unknown> | null): Record<string, unknown> | null {
    if (!params || typeof params.sessionId !== "string" || params.sessionId.length === 0) {
      return null;
    }
    return { sessionId: params.sessionId };
  },
  getDisplayId(params: Record<string, unknown> | null): string | null {
    if (!params || typeof params.sessionId !== "string") return null;
    return params.sessionId.slice(0, 8);
  },
};
