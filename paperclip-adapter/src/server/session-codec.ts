/**
 * Session codec for thClaws.
 *
 * thClaws persists sessions per-workspace at
 * `<workspaceDir>/.thclaws/sessions/<id>.jsonl`. The native `/agent/run`
 * endpoint accepts a `session_id` on input (loads + hydrates the
 * agent's history from that JSONL before running the new turn) and
 * returns the id back on output (via a `session` SSE event on the
 * sync/stream path, or the `session_id` field of the 202 ACK on the
 * async path). The codec just shuttles that id between Paperclip's
 * orchestration layer and the execute() result's `sessionParams`.
 *
 * (The OpenAI-compatible `/v1/chat/completions` surface remains
 * stateless per request — only `/agent/run` carries session continuity.)
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
