/**
 * IPC bridge between React frontend and Rust backend.
 *
 * Two transports — same protocol, same `send()` / `subscribe()` API.
 * The transport is chosen at module-load time by sniffing
 * `window.ipc`:
 *
 * - Desktop GUI (wry): `window.ipc.postMessage(json)` → Rust;
 *   `window.__thclaws_dispatch(json)` ← Rust (called via evaluate_script).
 * - Webapp (M6.36 `--serve` mode): WebSocket at `ws[s]://<host>/ws`.
 *   Inbound frames are dispatched to subscribers exactly like wry's
 *   `__thclaws_dispatch`; outbound `send()` calls map to `ws.send`.
 *
 * The webapp transport reconnects automatically with exponential
 * backoff capped at 5s. While disconnected, the singleton dispatches
 * `{type: "ws_status", status: "disconnected"|"connecting"|"connected"}`
 * envelopes so any banner component can render a "reconnecting…" UI.
 * On reconnect, the frontend re-sends `frontend_ready` so the server
 * pushes a fresh initial-state snapshot — Phase 1A semantics from the
 * M6.36 design (snapshot-then-stream).
 */

export type IPCMessage = {
  type: string;
  [key: string]: unknown;
};

type Handler = (msg: IPCMessage) => void;
const handlers = new Set<Handler>();

function dispatchToSubscribers(msg: IPCMessage) {
  handlers.forEach((h) => {
    try {
      h(msg);
    } catch (e) {
      console.error("[ipc] handler error:", e);
    }
  });
}

// ── Wry desktop transport (existing) ─────────────────────────────────

let wrySend: ((msg: IPCMessage) => void) | null = null;

if (typeof window !== "undefined" && window.ipc) {
  wrySend = (msg) => window.ipc!.postMessage(JSON.stringify(msg));
  window.__thclaws_dispatch = (json: string) => {
    try {
      const msg: IPCMessage = JSON.parse(json);
      dispatchToSubscribers(msg);
    } catch (e) {
      console.error("[ipc] wry dispatch parse error:", e);
    }
  };
}

// ── Webapp WebSocket transport (M6.36 SERVE4) ───────────────────────

let ws: WebSocket | null = null;
let wsSend: ((msg: IPCMessage) => void) | null = null;
let reconnectDelayMs = 250;
const RECONNECT_MAX_MS = 5000;

// Messages sent while the socket is still CONNECTING, flushed on open.
// Components that send on mount (ThemeProvider's `theme_get`, sidebar
// fetches, …) race the initial WS handshake; without this buffer those
// requests were silently dropped, so e.g. the persisted theme never
// loaded on a page refresh. Only the initial-connect window buffers —
// sends while disconnected still drop (reconnect re-syncs via the
// `frontend_ready` snapshot instead).
let wsConnectQueue: IPCMessage[] = [];

/**
 * Path prefix the frontend lives under, with a trailing slash.
 *
 * Local + self-hosted deploys serve at `/` so this returns `/`. Hosted
 * agents on thclaws.cloud serve under `/u/<handle>/<slug>/` — every
 * runtime URL (WebSocket, /upload, etc.) needs to include that prefix
 * so requests reach the right pod after Caddy strips it. Recomputed
 * lazily at call time so we pick up the actual page location, not a
 * cached value from another tab.
 */
export function basePath(): string {
  const p = window.location.pathname;
  return p.endsWith("/") ? p : p + "/";
}

function wsUrl(): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}${basePath()}ws`;
}

function emitStatus(status: "disconnected" | "connecting" | "connected") {
  // Synthetic event the React banner can subscribe to. Doesn't go
  // through the WS — purely a frontend-side signal.
  dispatchToSubscribers({ type: "ws_status", status });
}

function connectWs() {
  emitStatus("connecting");
  ws = new WebSocket(wsUrl());
  ws.onopen = () => {
    reconnectDelayMs = 250; // reset backoff
    emitStatus("connected");
    // M6.36 Phase 1A: re-send frontend_ready on every (re)connect so
    // the server pushes the latest snapshot. The server's
    // `frontend_ready` arm calls `on_send_initial_state` which (today)
    // is a stub but will become the snapshot builder per SERVE9.
    wsSend!({ type: "frontend_ready" });
    const queued = wsConnectQueue;
    wsConnectQueue = [];
    queued.forEach((m) => wsSend!(m));
  };
  ws.onmessage = (ev) => {
    try {
      const msg: IPCMessage = JSON.parse(ev.data);
      dispatchToSubscribers(msg);
    } catch (e) {
      console.error("[ipc] ws dispatch parse error:", e);
    }
  };
  ws.onclose = () => {
    emitStatus("disconnected");
    // Exponential backoff up to RECONNECT_MAX_MS.
    setTimeout(() => {
      reconnectDelayMs = Math.min(reconnectDelayMs * 2, RECONNECT_MAX_MS);
      connectWs();
    }, reconnectDelayMs);
  };
  ws.onerror = () => {
    // onclose fires after onerror — let onclose handle reconnect.
  };
}

if (typeof window !== "undefined" && !window.ipc) {
  // No wry — assume webapp transport.
  wsSend = (msg) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(msg));
    } else if (ws && ws.readyState === WebSocket.CONNECTING) {
      wsConnectQueue.push(msg);
    } else {
      console.warn("[ipc] ws not open, dropped:", msg);
    }
  };
  connectWs();
}

// ── Public API ──────────────────────────────────────────────────────

export function send(msg: IPCMessage) {
  if (wrySend) {
    wrySend(msg);
  } else if (wsSend) {
    wsSend(msg);
  } else {
    console.warn("[ipc] no backend — running in browser dev mode?", msg);
  }
}

export function subscribe(handler: Handler): () => void {
  handlers.add(handler);
  return () => {
    handlers.delete(handler);
  };
}
