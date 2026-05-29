import { useEffect, useState } from "react";
import { X, Send, CheckCircle2, AlertCircle, UserCheck, UserX } from "lucide-react";
import { send, subscribe } from "../hooks/useIPC";

/// Connect-then-status modal for the Telegram bridge (dev-plan/29 Tier 1).
///
/// Two states:
/// - **Disconnected** — paste the @BotFather bot token + Connect.
///   Submits `telegram_connect`; the worker validates via getMe, spawns
///   the long-poll session, and broadcasts `telegram_status`.
/// - **Connected** — shows @botname + live counts, the pending-pairings
///   list (Approve / Reject), and a Disconnect button.
///
/// `telegram_status` envelopes flow in via subscribe() so the modal
/// stays in sync with the worker; we also poll every few seconds so a
/// freshly-minted pairing request (created in the polling task, not the
/// worker loop) appears without the owner reopening the modal.

type Pairing = {
  code: string;
  user_id: number;
  chat_id: number;
  display: string;
};

type Status = {
  state: "connected" | "disconnected";
  bot_username: string | null;
  pending_approvals: number;
  pending_pairings: number;
  active_chats: number;
  pairings: Pairing[];
};

const EMPTY: Status = {
  state: "disconnected",
  bot_username: null,
  pending_approvals: 0,
  pending_pairings: 0,
  active_chats: 0,
  pairings: [],
};

export function TelegramConnectModal({ onClose }: { onClose: () => void }) {
  const [status, setStatus] = useState<Status>(EMPTY);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const unsub = subscribe((msg) => {
      if (msg.type === "telegram_status") {
        setStatus({
          state: (msg.state as Status["state"]) ?? "disconnected",
          bot_username: (msg.bot_username as string) ?? null,
          pending_approvals: (msg.pending_approvals as number) ?? 0,
          pending_pairings: (msg.pending_pairings as number) ?? 0,
          active_chats: (msg.active_chats as number) ?? 0,
          pairings: (msg.pairings as Pairing[]) ?? [],
        });
        // A status envelope carrying an error (bad token) ends the busy
        // spinner and surfaces the reason.
        if (msg.error) setError(msg.error as string);
        if (msg.state === "connected") {
          setBusy(false);
          setError(null);
          setToken("");
        }
      } else if (msg.type === "telegram_connect_ack") {
        if (!msg.ok) {
          setBusy(false);
          setError((msg.error as string) ?? "connect failed");
        }
        // On ok we wait for the worker's telegram_status (after getMe).
      } else if (msg.type === "telegram_disconnect_ack") {
        setBusy(false);
      }
    });
    send({ type: "telegram_status" });
    // Poll for new pending pairings while the modal is open.
    const poll = setInterval(() => send({ type: "telegram_status" }), 3000);
    return () => {
      clearInterval(poll);
      unsub();
    };
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [onClose]);

  const handleConnect = () => {
    setError(null);
    setBusy(true);
    send({ type: "telegram_connect", bot_token: token.trim() });
  };

  const handleDisconnect = () => {
    setBusy(true);
    send({ type: "telegram_disconnect" });
  };

  const approve = (code: string) =>
    send({ type: "telegram_pairing_approve", code });
  const reject = (code: string) =>
    send({ type: "telegram_pairing_reject", code });

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.5)" }}
      onClick={onClose}
    >
      <div
        className="rounded-lg shadow-2xl"
        style={{
          background: "var(--bg-primary)",
          border: "1px solid var(--border)",
          width: "460px",
          maxWidth: "90vw",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between px-4 py-3 border-b"
          style={{ borderColor: "var(--border)" }}
        >
          <div className="flex items-center gap-2">
            <Send size={16} style={{ color: "var(--accent)" }} />
            <span
              className="font-semibold text-sm"
              style={{ color: "var(--text-primary)" }}
            >
              Telegram Connect
            </span>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-white/10"
            style={{ color: "var(--text-secondary)" }}
            title="Close (Esc)"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-4 space-y-4">
          {status.state === "connected" ? (
            <ConnectedView
              status={status}
              busy={busy}
              onDisconnect={handleDisconnect}
              onApprove={approve}
              onReject={reject}
            />
          ) : (
            <DisconnectedView
              token={token}
              setToken={setToken}
              busy={busy}
              error={error}
              onConnect={handleConnect}
            />
          )}
        </div>
      </div>
    </div>
  );
}

function DisconnectedView({
  token,
  setToken,
  busy,
  error,
  onConnect,
}: {
  token: string;
  setToken: (s: string) => void;
  busy: boolean;
  error: string | null;
  onConnect: () => void;
}) {
  return (
    <>
      <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
        Create a bot with{" "}
        <span className="font-mono" style={{ color: "var(--text-primary)" }}>
          @BotFather
        </span>{" "}
        on Telegram, then paste its token below. The agent stays on this
        machine; Telegram is just the chat surface. The first person to DM
        the bot gets a pairing code you approve here.
      </p>
      <div className="space-y-2">
        <label
          className="block text-xs font-semibold"
          style={{ color: "var(--text-primary)" }}
        >
          Bot token
        </label>
        <input
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onConnect();
          }}
          placeholder="123456789:AA…"
          className="w-full px-3 py-2 rounded font-mono text-sm"
          style={{
            background: "var(--bg-secondary)",
            border: "1px solid var(--border)",
            color: "var(--text-primary)",
          }}
          autoFocus
        />
        <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
          Leave blank to use the{" "}
          <span className="font-mono">TELEGRAM_BOT_TOKEN</span> env var.
        </p>
      </div>
      {error && (
        <div
          className="flex items-start gap-2 text-xs px-3 py-2 rounded"
          style={{
            background: "var(--bg-secondary)",
            color: "var(--danger, #e06c75)",
            border: "1px solid var(--border)",
          }}
        >
          <AlertCircle size={14} className="shrink-0 mt-0.5" />
          <span>{error}</span>
        </div>
      )}
      <div className="flex justify-end gap-2">
        <button
          onClick={onConnect}
          disabled={busy}
          className="px-3 py-1.5 rounded text-xs font-semibold"
          style={{
            background: busy ? "var(--bg-secondary)" : "var(--accent)",
            color: "var(--accent-fg, #ffffff)",
            opacity: busy ? 0.5 : 1,
          }}
        >
          {busy ? "Connecting…" : "Connect"}
        </button>
      </div>
    </>
  );
}

function ConnectedView({
  status,
  busy,
  onDisconnect,
  onApprove,
  onReject,
}: {
  status: Status;
  busy: boolean;
  onDisconnect: () => void;
  onApprove: (code: string) => void;
  onReject: (code: string) => void;
}) {
  return (
    <>
      <div
        className="flex items-start gap-2 text-xs px-3 py-2 rounded"
        style={{
          background: "var(--bg-secondary)",
          border: "1px solid var(--border)",
        }}
      >
        <CheckCircle2
          size={14}
          className="shrink-0 mt-0.5"
          style={{ color: "var(--success, #98c379)" }}
        />
        <div className="space-y-1">
          <div style={{ color: "var(--text-primary)" }}>
            <strong>Connected</strong>
            {status.bot_username ? ` as ${status.bot_username}` : ""}. DM the
            bot to verify end-to-end.
          </div>
          <div
            className="font-mono"
            style={{ color: "var(--text-secondary)", fontSize: "10px" }}
          >
            {status.active_chats} chat(s)
            {status.pending_approvals > 0
              ? ` · ${status.pending_approvals} approval(s) pending`
              : ""}
          </div>
        </div>
      </div>

      {status.pairings.length > 0 && (
        <div className="space-y-2">
          <div
            className="text-xs font-semibold"
            style={{ color: "var(--text-primary)" }}
          >
            Pairing requests
          </div>
          {status.pairings.map((p) => (
            <div
              key={p.code}
              className="flex items-center justify-between gap-2 px-3 py-2 rounded"
              style={{
                background: "var(--bg-secondary)",
                border: "1px solid var(--border)",
              }}
            >
              <div className="min-w-0">
                <div
                  className="text-xs truncate"
                  style={{ color: "var(--text-primary)" }}
                >
                  {p.display}
                </div>
                <div
                  className="font-mono"
                  style={{ color: "var(--text-secondary)", fontSize: "10px" }}
                >
                  code {p.code}
                </div>
              </div>
              <div className="flex items-center gap-1 shrink-0">
                <button
                  onClick={() => onApprove(p.code)}
                  className="flex items-center gap-1 px-2 py-1 rounded text-xs font-semibold"
                  style={{
                    background: "var(--accent)",
                    color: "var(--accent-fg, #ffffff)",
                  }}
                  title="Approve"
                >
                  <UserCheck size={12} /> Approve
                </button>
                <button
                  onClick={() => onReject(p.code)}
                  className="flex items-center gap-1 px-2 py-1 rounded text-xs font-semibold"
                  style={{
                    background: "var(--bg-primary)",
                    border: "1px solid var(--border)",
                    color: "var(--danger, #e06c75)",
                  }}
                  title="Reject"
                >
                  <UserX size={12} />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="flex justify-end">
        <button
          onClick={onDisconnect}
          disabled={busy}
          className="px-3 py-1.5 rounded text-xs font-semibold"
          style={{
            background: "var(--bg-secondary)",
            border: "1px solid var(--border)",
            color: "var(--danger, #e06c75)",
            opacity: busy ? 0.5 : 1,
          }}
        >
          {busy ? "Disconnecting…" : "Disconnect"}
        </button>
      </div>
    </>
  );
}
