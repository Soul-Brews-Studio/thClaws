import { useEffect, useState } from "react";
import { KeyRound, X, Check, Trash2, Link as LinkIcon } from "lucide-react";
import { send, subscribe } from "../hooks/useIPC";
import { SecretsBackendDialog } from "./SecretsBackendDialog";
import {
  isOpenRouterFreeOnly,
  setOpenRouterFreeOnly,
  refreshOpenRouterFreeOnly,
} from "./ModelPickerModal";

type KeyStatus = {
  provider: string;
  env_var: string;
  configured_in_keychain: boolean;
  env_set: boolean;
  key_length: number;
  kind?: "provider" | "service";
};

type EndpointStatus = {
  provider: string;
  env_var: string;
  configured_url: string | null;
  default_url: string;
};

// Sentinel shown in the API key input when a key is already configured.
// Its length matches the actual env-var length so the user gets a visual
// cue of the key's size without ever seeing its contents. If no key is
// loaded we fall back to a short 5-char sentinel.
const FALLBACK_SENTINEL = "*****";
function sentinelFor(length: number): string {
  // Clamp so a huge key doesn't overflow the field to an absurd width.
  const clamped = Math.max(5, Math.min(length, 64));
  return "*".repeat(clamped);
}
function isSentinel(s: string): boolean {
  return s.length >= 5 && /^\*+$/.test(s);
}

const PROVIDER_LABELS: Record<string, string> = {
  "agentic-press": "Agentic Press LLM",
  anthropic: "Anthropic",
  openai: "OpenAI",
  openrouter: "OpenRouter",
  gemini: "Google Gemini",
  dashscope: "Alibaba DashScope",
  ollama: "Ollama",
  "ollama-anthropic": "Ollama (Anthropic-compatible)",
  "ollama-cloud": "Ollama Cloud",
  "opencode-go": "OpenCode Go",
  azure: "Azure AI Foundry",
  "openai-compat": "OpenAI-Compatible (custom endpoint)",
  tavily: "Tavily Search",
  "brave-search": "Brave Search",
  hal: "HAL Public API (YouTube transcript + Web scrape)",
};

export function SettingsModal({ onClose }: { onClose: () => void }) {
  const [keys, setKeys] = useState<KeyStatus[]>([]);
  const [endpoints, setEndpoints] = useState<EndpointStatus[]>([]);
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  const [urlDrafts, setUrlDrafts] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [flash, setFlash] = useState<Record<string, { ok: boolean; msg: string }>>({});
  // Storage backend: null until we hear back from the backend. If the
  // backend reports `null` (user never picked), we show the chooser
  // dialog first and only render the key fields after the user picks.
  const [backend, setBackend] = useState<"keychain" | "dotenv" | null>(null);
  const [backendKnown, setBackendKnown] = useState(false);

  // Ask the backend for the stored preference first. Nothing else
  // happens until we know — no api_key_status, no keychain reads.
  useEffect(() => {
    const unsub = subscribe((msg) => {
      if (msg.type === "secrets_backend") {
        const value = (msg.backend as string | null) ?? null;
        setBackend(value === "keychain" || value === "dotenv" ? value : null);
        setBackendKnown(true);
      }
    });
    send({ type: "secrets_backend_get" });
    return unsub;
  }, []);

  useEffect(() => {
    // Only subscribe for key / endpoint data once the user has picked.
    if (!backend) return;
    const unsub = subscribe((msg) => {
      if (msg.type === "api_key_status" && Array.isArray(msg.keys)) {
        const next = msg.keys as KeyStatus[];
        setKeys(next);
        // Every key field starts at a sentinel string sized to the actual
        // key length — the user sees at a glance that a key is loaded and
        // roughly how long it is, without the value being exposed. Preserves
        // in-flight edits so a status refresh doesn't clobber typing.
        setKeyDrafts((prev) => {
          const out = { ...prev };
          for (const k of next) {
            const cur = out[k.provider];
            const sentinel = k.key_length > 0 ? sentinelFor(k.key_length) : FALLBACK_SENTINEL;
            if (cur === undefined || cur === "" || isSentinel(cur)) {
              out[k.provider] = sentinel;
            }
          }
          return out;
        });
      } else if (msg.type === "endpoint_status" && Array.isArray(msg.endpoints)) {
        const next = msg.endpoints as EndpointStatus[];
        setEndpoints(next);
        // Pre-fill URL drafts with the configured value so the user sees it
        // and can edit in place. Leave actively-edited drafts alone.
        setUrlDrafts((prev) => {
          const out = { ...prev };
          for (const e of next) {
            const cur = out[e.provider];
            if (cur === undefined || cur === "" || cur === e.configured_url) {
              out[e.provider] = e.configured_url ?? "";
            }
          }
          return out;
        });
      } else if (msg.type === "api_key_result" || msg.type === "endpoint_result") {
        const provider = msg.provider as string;
        const flashKey = `${provider}:${msg.type === "api_key_result" ? "key" : "url"}`;
        setBusy(null);
        setFlash((f) => ({
          ...f,
          [flashKey]: {
            ok: Boolean(msg.ok),
            msg: msg.ok
              ? msg.action === "set"
                ? msg.storage === "dotenv"
                  ? "Saved to ~/.config/thclaws/.env (keychain unavailable)"
                  : "Saved to OS keychain"
                : "Cleared"
              : String(msg.error ?? "Failed"),
          },
        }));
        if (msg.ok) {
          if (msg.type === "api_key_result") {
            // Reset the draft; the follow-up api_key_status will repopulate
            // with a sentinel sized to the new key length.
            setKeyDrafts((d) => ({ ...d, [provider]: "" }));
          }
          // URL drafts get re-synced from the follow-up endpoint_status below.
        }
        send({ type: msg.type === "api_key_result" ? "api_key_status" : "endpoint_status" });
        setTimeout(() => {
          setFlash((f) => {
            const next = { ...f };
            delete next[flashKey];
            return next;
          });
        }, 2500);
      }
    });
    send({ type: "api_key_status" });
    send({ type: "endpoint_status" });
    return unsub;
  }, [backend]);

  // Merge keys + endpoints by provider so each provider renders once.
  // Insertion order from `keys[]` is preserved by Map — backend
  // already orders providers (LLM) first, services (search) last.
  const providers = new Map<string, { key?: KeyStatus; endpoint?: EndpointStatus }>();
  keys.forEach((k) => {
    const entry = providers.get(k.provider) ?? {};
    entry.key = k;
    providers.set(k.provider, entry);
  });
  endpoints.forEach((e) => {
    const entry = providers.get(e.provider) ?? {};
    entry.endpoint = e;
    providers.set(e.provider, entry);
  });
  const llmEntries = Array.from(providers.entries()).filter(
    ([, row]) => (row.key?.kind ?? "provider") !== "service",
  );
  const serviceEntries = Array.from(providers.entries()).filter(
    ([, row]) => row.key?.kind === "service",
  );

  const handleSaveKey = (provider: string) => {
    const key = (keyDrafts[provider] ?? "").trim();
    // Empty or any asterisk-only sentinel → nothing to save (user didn't edit).
    if (!key || isSentinel(key)) return;
    setBusy(`${provider}:key`);
    send({ type: "api_key_set", provider, key });
  };

  const handleClearKey = (provider: string) => {
    setBusy(`${provider}:key`);
    send({ type: "api_key_clear", provider });
  };

  const handleSaveUrl = (provider: string) => {
    const url = (urlDrafts[provider] ?? "").trim();
    if (!url) return;
    // Unchanged → skip the round-trip.
    const current = endpoints.find((e) => e.provider === provider)?.configured_url ?? "";
    if (url === current) return;
    setBusy(`${provider}:url`);
    send({ type: "endpoint_set", provider, url });
  };

  const handleClearUrl = (provider: string) => {
    setBusy(`${provider}:url`);
    send({ type: "endpoint_clear", provider });
  };

  // Still waiting to hear from the backend → render nothing (a flash
  // of empty modal is worse than a tiny delay).
  if (!backendKnown) return null;

  // User hasn't picked a storage backend yet — show the chooser
  // dialog first. Once they pick, backend flips to the chosen value
  // and the real modal renders.
  if (backend === null) {
    return (
      <SecretsBackendDialog
        onPicked={(choice) => setBackend(choice)}
        onCancel={onClose}
      />
    );
  }

  return (
    <div
      className="fixed inset-0 flex items-center justify-center z-50"
      style={{ background: "var(--modal-backdrop)" }}
      // Close on backdrop mousedown only when the gesture *started* on
      // the backdrop — keeps drag-to-select inside the modal from
      // accidentally dismissing it on mouseup outside.
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="rounded-lg shadow-2xl p-5 max-w-xl w-full mx-4 max-h-[85vh] overflow-y-auto"
        style={{ background: "var(--bg-secondary)", border: "1px solid var(--border)" }}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <KeyRound size={16} style={{ color: "var(--accent)" }} />
            <h2 className="text-sm font-semibold" style={{ color: "var(--text-primary)" }}>
              Providers
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-white/10"
            style={{ color: "var(--text-secondary)" }}
            title="Close"
          >
            <X size={14} />
          </button>
        </div>
        <p className="text-xs mb-4" style={{ color: "var(--text-secondary)" }}>
          {backend === "keychain"
            ? "API keys live in your OS keychain (encrypted, tied to your user account)."
            : (
              <>
                API keys are stored in{" "}
                <span className="font-mono">~/.config/thclaws/.env</span>{" "}
                (plain text — no keychain prompts).
              </>
            )}{" "}
          Base URLs are saved to{" "}
          <span className="font-mono">~/.config/thclaws/endpoints.json</span>.
          A shell <span className="font-mono">export</span> always wins.{" "}
          <button
            type="button"
            onClick={() => setBackend(null)}
            className="underline"
            style={{ color: "var(--text-primary)", opacity: 0.7 }}
            title="Change storage backend"
          >
            Change
          </button>
        </p>

        <div className="flex flex-col gap-3">
          <GatewaySettingsSection />
          <DeployTargetSection />
          <AutoLearnSection />

          {llmEntries.map(([provider, row]) =>
            renderProviderCard(
              provider,
              row,
              keyDrafts,
              setKeyDrafts,
              urlDrafts,
              setUrlDrafts,
              handleSaveKey,
              handleClearKey,
              handleSaveUrl,
              handleClearUrl,
              busy,
              flash,
            ),
          )}

          {serviceEntries.length > 0 && (
            <>
              <div
                className="text-[10px] uppercase tracking-wider mt-2"
                style={{ color: "var(--text-secondary)" }}
              >
                Service keys
              </div>
              {serviceEntries.map(([provider, row]) =>
                renderProviderCard(
                  provider,
                  row,
                  keyDrafts,
                  setKeyDrafts,
                  urlDrafts,
                  setUrlDrafts,
                  handleSaveKey,
                  handleClearKey,
                  handleSaveUrl,
                  handleClearUrl,
                  busy,
                  flash,
                ),
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function renderProviderCard(
  provider: string,
  row: { key?: KeyStatus; endpoint?: EndpointStatus },
  keyDrafts: Record<string, string>,
  setKeyDrafts: React.Dispatch<React.SetStateAction<Record<string, string>>>,
  urlDrafts: Record<string, string>,
  setUrlDrafts: React.Dispatch<React.SetStateAction<Record<string, string>>>,
  handleSaveKey: (p: string) => void,
  handleClearKey: (p: string) => void,
  handleSaveUrl: (p: string) => void,
  handleClearUrl: (p: string) => void,
  busy: string | null,
  flash: Record<string, { ok: boolean; msg: string }>,
) {
  const label = PROVIDER_LABELS[provider] ?? provider;
  return (
    <div
      key={provider}
      className="rounded p-3"
      style={{ background: "var(--bg-tertiary)", border: "1px solid var(--border)" }}
    >
      <div className="flex items-center justify-between mb-2">
        <div className="text-xs font-semibold" style={{ color: "var(--text-primary)" }}>
          {label}
        </div>
      </div>

      {row.key && (
        <KeyRow
          status={row.key}
          draft={
            keyDrafts[provider] ??
            (row.key.key_length > 0
              ? sentinelFor(row.key.key_length)
              : FALLBACK_SENTINEL)
          }
          onDraft={(v) => setKeyDrafts((d) => ({ ...d, [provider]: v }))}
          onSave={() => handleSaveKey(provider)}
          onClear={() => handleClearKey(provider)}
          busy={busy === `${provider}:key`}
          flash={flash[`${provider}:key`]}
        />
      )}

      {row.endpoint && (
        <UrlRow
          status={row.endpoint}
          draft={urlDrafts[provider] ?? (row.endpoint.configured_url ?? "")}
          onDraft={(v) => setUrlDrafts((d) => ({ ...d, [provider]: v }))}
          onSave={() => handleSaveUrl(provider)}
          onClear={() => handleClearUrl(provider)}
          busy={busy === `${provider}:url`}
          flash={flash[`${provider}:url`]}
          hasKeyRow={Boolean(row.key)}
        />
      )}

      {provider === "openrouter" && <OpenRouterFreeOnlyToggle />}
      {GATEWAY_PROVIDERS.has(provider) && <GatewayPerProviderToggle provider={provider} />}
    </div>
  );
}

/// Provider names that have an upstream route on the thClaws Gateway.
/// Must match `crate::providers::thclaws_gateway::provider_segment`.
const GATEWAY_PROVIDERS = new Set(["openai", "anthropic", "gemini", "openrouter"]);

/// OpenRouter-only inline toggle. When on, both the model picker
/// and the `/models` slash command hide non-free rows. Persisted
/// server-side via `openrouter_free_only_set` so the slash-command
/// handler (server-side rendering) sees the same flag the UI shows.
/// localStorage is just a fast-paint cache; the on-mount IPC fetch
/// corrects drift against `.thclaws/settings.json`.
function OpenRouterFreeOnlyToggle() {
  const [on, setOn] = useState<boolean>(() => isOpenRouterFreeOnly());
  useEffect(() => refreshOpenRouterFreeOnly(setOn), []);
  return (
    <label
      className="flex items-center gap-2 mt-2 text-xs select-none cursor-pointer"
      style={{ color: "var(--text-secondary)" }}
      title="When on, the model picker and /models slash command show only OpenRouter models with $0 prompt + $0 completion pricing."
    >
      <input
        type="checkbox"
        checked={on}
        onChange={(e) => {
          setOn(e.target.checked);
          setOpenRouterFreeOnly(e.target.checked);
        }}
      />
      <span>Free only — filter the model picker and /models to $0 / $0 pricing</span>
    </label>
  );
}

/// Frontend provider id → gateway path segment. Must mirror
/// `crate::providers::thclaws_gateway::provider_segment`.
const PROVIDER_TO_GATEWAY_SEGMENT: Record<string, string> = {
  openai: "openai",
  anthropic: "anthropic",
  gemini: "google",
  openrouter: "openrouter",
};

type GatewaySettings = {
  base_url: string;
  use_for: string[];
};

let cachedGatewaySettings: GatewaySettings | null = null;
const gatewayListeners = new Set<(s: GatewaySettings) => void>();

function applyGatewaySettings(s: GatewaySettings) {
  cachedGatewaySettings = s;
  for (const cb of gatewayListeners) cb(s);
}

function ensureGatewaySubscription() {
  if ((ensureGatewaySubscription as { inited?: boolean }).inited) return;
  (ensureGatewaySubscription as { inited?: boolean }).inited = true;
  subscribe((msg) => {
    if (msg.type === "gateway_settings" || msg.type === "gateway_settings_result") {
      const settings = {
        base_url: String((msg as { base_url?: string }).base_url ?? ""),
        use_for: ((msg as { use_for?: string[] }).use_for ?? []).map((s) => String(s)),
      };
      applyGatewaySettings(settings);
    }
  });
  send({ type: "gateway_settings_get" });
}

function useGatewaySettings(): GatewaySettings {
  const [state, setState] = useState<GatewaySettings>(
    () => cachedGatewaySettings ?? { base_url: "", use_for: [] },
  );
  useEffect(() => {
    ensureGatewaySubscription();
    gatewayListeners.add(setState);
    if (cachedGatewaySettings) setState(cachedGatewaySettings);
    return () => {
      gatewayListeners.delete(setState);
    };
  }, []);
  return state;
}

function persistGatewaySettings(use_for: string[]) {
  // Base URL is fixed on the backend; the IPC echoes it back in
  // gateway_settings_result so we always render the current value.
  const current = cachedGatewaySettings?.base_url ?? "";
  applyGatewaySettings({ base_url: current, use_for });
  send({ type: "gateway_settings_set", use_for });
}

/// Top-of-modal card: access key field for the fixed thClaws Gateway.
/// Access key persists to the keychain via the existing api_key_set
/// IPC (provider name "gateway"). The gateway base URL is hard-coded
/// on the backend (see `providers::thclaws_gateway::GATEWAY_BASE_URL`)
/// — users only paste their key and flip the per-provider toggles.
function GatewaySettingsSection() {
  const settings = useGatewaySettings();
  const [keyDraft, setKeyDraft] = useState("");
  const onSaveKey = () => {
    const trimmed = keyDraft.trim();
    if (!trimmed) return;
    send({ type: "api_key_set", provider: "gateway", key: trimmed });
    setKeyDraft("");
  };
  return (
    <div
      className="rounded-lg p-3"
      style={{ background: "var(--bg-tertiary)", border: "1px solid var(--border)" }}
    >
      <div className="flex items-center gap-2 mb-1">
        <LinkIcon size={12} style={{ color: "var(--accent)" }} />
        <span className="text-sm font-semibold" style={{ color: "var(--text-primary)" }}>
          thClaws Gateway
        </span>
      </div>
      <p className="text-xs mb-2" style={{ color: "var(--text-secondary)" }}>
        Route per-provider traffic through <span className="font-mono">{settings.base_url}</span>.
        Paste your access key here, then flip "Use thClaws Gateway" on the provider cards below.
      </p>
      <FieldLabel icon={<KeyRound size={11} />} text="Access key" env="THCLAWS_GATEWAY_API_KEY" />
      <div className="flex gap-1.5">
        <input
          type="password"
          placeholder="Paste gateway access key (gw_v1_…)"
          className="flex-1 px-2.5 py-1.5 rounded text-xs font-mono outline-none"
          style={{
            background: "var(--bg-primary)",
            color: "var(--text-primary)",
            border: "1px solid var(--border)",
          }}
          value={keyDraft}
          onChange={(e) => setKeyDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSaveKey();
          }}
          autoComplete="off"
        />
        <button
          type="button"
          onClick={onSaveKey}
          disabled={!keyDraft.trim()}
          className="px-2 py-1.5 rounded text-xs"
          style={{
            background: keyDraft.trim() ? "var(--accent)" : "var(--bg-primary)",
            color: keyDraft.trim() ? "var(--accent-fg)" : "var(--text-secondary)",
            border: "1px solid var(--border)",
            cursor: keyDraft.trim() ? "pointer" : "not-allowed",
          }}
        >
          Save
        </button>
      </div>
    </div>
  );
}

/// Per-provider "Use thClaws Gateway" checkbox in the provider card.
/// Always enabled — gateway routing kicks in once the user has also
/// pasted the access key (overlay returns None and falls back to the
/// upstream when the key is missing, so toggling without a key is a
/// no-op rather than an error).
function GatewayPerProviderToggle({ provider }: { provider: string }) {
  const settings = useGatewaySettings();
  const segment = PROVIDER_TO_GATEWAY_SEGMENT[provider];
  const on = !!segment && settings.use_for.includes(segment);
  const onChange = (checked: boolean) => {
    if (!segment) return;
    const next = new Set(settings.use_for);
    if (checked) next.add(segment);
    else next.delete(segment);
    persistGatewaySettings(Array.from(next));
  };
  return (
    <label
      className="flex items-center gap-2 mt-2 text-xs select-none cursor-pointer"
      style={{ color: "var(--text-secondary)" }}
      title={`Route ${provider} traffic through the thClaws Gateway.`}
    >
      <input
        type="checkbox"
        checked={on}
        disabled={!segment}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span>Use thClaws Gateway</span>
    </label>
  );
}

function KeyRow({
  status,
  draft,
  onDraft,
  onSave,
  onClear,
  busy,
  flash,
}: {
  status: KeyStatus;
  draft: string;
  onDraft: (v: string) => void;
  onSave: () => void;
  onClear: () => void;
  busy: boolean;
  flash?: { ok: boolean; msg: string };
}) {
  const trimmed = draft.trim();
  const showingSentinel = isSentinel(draft);
  const unchanged = trimmed === "" || showingSentinel;
  return (
    <div>
      <FieldLabel icon={<KeyRound size={11} />} text="API Key" env={status.env_var} />
      <div className="flex gap-1.5">
        <input
          // While the sentinel is showing we use `text` so the literal
          // asterisks are visible; once the user starts typing a real key
          // we flip to `password` so the characters mask.
          type={showingSentinel ? "text" : "password"}
          placeholder="Paste API key"
          className="flex-1 px-2.5 py-1.5 rounded text-xs font-mono outline-none"
          style={{
            background: "var(--bg-primary)",
            color: "var(--text-primary)",
            border: "1px solid var(--border)",
          }}
          value={draft}
          onChange={(e) => onDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSave();
          }}
          onFocus={(e) => {
            // Clicking the sentinel selects it so typing replaces in one go.
            if (isSentinel(e.currentTarget.value)) e.currentTarget.select();
          }}
          disabled={busy}
          autoComplete="off"
          spellCheck={false}
        />
        <SaveButton onClick={onSave} disabled={unchanged || busy} />
        {/* Show Clear whenever any key is loaded — from the keychain or
            from an .env file / shell export. The backend clears the
            keychain entry (if any) and unsets the env var for the
            running process. */}
        {(status.configured_in_keychain || status.env_set) && (
          <ClearButton
            onClick={onClear}
            disabled={busy}
            title={
              status.configured_in_keychain
                ? "Remove from OS keychain"
                : "Unset for this session (edit .env to remove permanently)"
            }
          />
        )}
      </div>
      <FlashLine flash={flash} />
    </div>
  );
}

function UrlRow({
  status,
  draft,
  onDraft,
  onSave,
  onClear,
  busy,
  flash,
  hasKeyRow,
}: {
  status: EndpointStatus;
  draft: string;
  onDraft: (v: string) => void;
  onSave: () => void;
  onClear: () => void;
  busy: boolean;
  flash?: { ok: boolean; msg: string };
  hasKeyRow: boolean;
}) {
  const trimmed = draft.trim();
  const current = status.configured_url ?? "";
  const unchanged = trimmed === "" || trimmed === current;
  return (
    <div style={{ marginTop: hasKeyRow ? 10 : 0 }}>
      <FieldLabel icon={<LinkIcon size={11} />} text="Base URL" env={status.env_var} />
      <div className="flex gap-1.5">
        <input
          type="text"
          placeholder={status.default_url}
          className="flex-1 px-2.5 py-1.5 rounded text-xs font-mono outline-none"
          style={{
            background: "var(--bg-primary)",
            color: "var(--text-primary)",
            border: "1px solid var(--border)",
          }}
          value={draft}
          onChange={(e) => onDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSave();
          }}
          disabled={busy}
          autoComplete="off"
          spellCheck={false}
        />
        <SaveButton onClick={onSave} disabled={unchanged || busy} />
        {status.configured_url && <ClearButton onClick={onClear} disabled={busy} />}
      </div>
      <FlashLine flash={flash} />
    </div>
  );
}

function FieldLabel({ icon, text, env }: { icon: React.ReactNode; text: string; env: string }) {
  return (
    <div
      className="flex items-center gap-1.5 mb-1"
      style={{ color: "var(--text-secondary)", fontSize: "10px" }}
    >
      {icon}
      <span className="uppercase tracking-wider">{text}</span>
      <span className="font-mono" style={{ opacity: 0.7 }}>
        {env}
      </span>
    </div>
  );
}

function SaveButton({ onClick, disabled }: { onClick: () => void; disabled: boolean }) {
  return (
    <button
      className="px-3 py-1.5 rounded text-xs font-medium shrink-0 flex items-center gap-1"
      style={{
        background: "var(--accent)",
        color: "#fff",
        opacity: disabled ? 0.4 : 1,
        cursor: disabled ? "not-allowed" : "pointer",
      }}
      onClick={onClick}
      disabled={disabled}
    >
      <Check size={12} /> Save
    </button>
  );
}

function ClearButton({
  onClick,
  disabled,
  title = "Remove",
}: {
  onClick: () => void;
  disabled: boolean;
  title?: string;
}) {
  return (
    <button
      className="px-2.5 py-1.5 rounded text-xs font-medium shrink-0 flex items-center gap-1"
      style={{
        background: "var(--bg-primary)",
        color: "var(--text-secondary)",
        border: "1px solid var(--border)",
        opacity: disabled ? 0.4 : 1,
        cursor: disabled ? "not-allowed" : "pointer",
      }}
      onClick={onClick}
      disabled={disabled}
      title={title}
    >
      <Trash2 size={12} />
    </button>
  );
}

function FlashLine({ flash }: { flash?: { ok: boolean; msg: string } }) {
  if (!flash) return null;
  return (
    <div
      className="mt-1 text-[10px] text-right"
      style={{ color: flash.ok ? "var(--accent)" : "var(--danger, #e06c75)" }}
    >
      {flash.msg}
    </div>
  );
}

// ── Deploy target (dev-plan/28) ──────────────────────────────────────
// Pairs with /deploy slash command. URL persists to settings.json;
// token persists to the OS keychain bundle (same as provider API keys).
// Either can be set independently. Token-set shows ••••• and a Clear
// button.
interface RemoteAgentConfig {
  url: string | null;
  has_token: boolean;
  token_length: number;
  env_var_set: boolean;
  keychain_writable: boolean;
}

function DeployTargetSection() {
  const [cfg, setCfg] = useState<RemoteAgentConfig>({
    url: null,
    has_token: false,
    token_length: 0,
    env_var_set: false,
    keychain_writable: true,
  });
  const [urlDraft, setUrlDraft] = useState("");
  const [tokenDraft, setTokenDraft] = useState("");
  const [flash, setFlash] = useState<{ ok: boolean; msg: string } | undefined>(undefined);

  useEffect(() => {
    const unsub = subscribe((msg) => {
      if (msg.type === "remote_agent_config") {
        const next = msg as unknown as RemoteAgentConfig & { type: string };
        setCfg({
          url: next.url ?? null,
          has_token: !!next.has_token,
          token_length: typeof next.token_length === "number" ? next.token_length : 0,
          env_var_set: !!next.env_var_set,
          keychain_writable: !!next.keychain_writable,
        });
      } else if (msg.type === "remote_agent_result") {
        const r = msg as {
          url_ok?: boolean;
          url_error?: string;
          token_ok?: boolean;
          token_error?: string;
        };
        if (r.url_ok === false || r.token_ok === false) {
          const parts: string[] = [];
          if (r.url_ok === false) parts.push(`URL: ${r.url_error ?? "failed"}`);
          if (r.token_ok === false) parts.push(`Token: ${r.token_error ?? "failed"}`);
          setFlash({ ok: false, msg: parts.join(" · ") });
        } else {
          setFlash({ ok: true, msg: "saved" });
          // Reset drafts so the URL field re-pre-fills with the saved
          // value and the token field re-shows the sentinel — matches
          // the post-save state of the provider rows.
          setUrlDraft("");
          setTokenDraft("");
          setTimeout(() => setFlash(undefined), 2500);
        }
        send({ type: "remote_agent_get" });
      }
    });
    send({ type: "remote_agent_get" });
    return unsub;
  }, []);

  const onSaveUrl = () => {
    const trimmed = urlDraft.trim();
    if (!trimmed) return;
    send({ type: "remote_agent_set", url: trimmed });
  };
  const onClearUrl = () => {
    send({ type: "remote_agent_set", url: "" });
    setUrlDraft("");
  };
  const onSaveToken = () => {
    const trimmed = tokenDraft.trim();
    if (!trimmed || isSentinel(trimmed)) return;
    send({ type: "remote_agent_set", token: trimmed });
  };
  const onClearToken = () => {
    send({ type: "remote_agent_set", token: "" });
    setTokenDraft("");
  };

  // URL field pre-fills with the saved value so the user can edit in
  // place; Save is "dirty when draft != saved" (mirrors UrlRow).
  const urlValue = urlDraft || cfg.url || "";
  const urlDirty = urlValue.trim() !== (cfg.url ?? "").trim() && urlValue.trim().length > 0;

  // Token field shows a ••••• sentinel sized to match other rows when
  // stored. Save is "dirty when the user typed a new non-sentinel
  // value." Mirrors KeyRow.
  const tokenSentinel = cfg.has_token
    ? cfg.token_length > 0
      ? sentinelFor(cfg.token_length)
      : FALLBACK_SENTINEL
    : "";
  const tokenValue = tokenDraft || tokenSentinel;
  const tokenDirty = tokenDraft.trim().length > 0 && !isSentinel(tokenDraft.trim());

  return (
    <div
      className="rounded-lg p-3"
      style={{ background: "var(--bg-tertiary)", border: "1px solid var(--border)" }}
    >
      <div className="flex items-center gap-2 mb-1">
        <LinkIcon size={12} style={{ color: "var(--accent)" }} />
        <span className="text-sm font-semibold" style={{ color: "var(--text-primary)" }}>
          Deploy target
        </span>
      </div>
      <p className="text-xs mb-2" style={{ color: "var(--text-secondary)" }}>
        Default pod for the <span className="font-mono">/deploy</span> slash command.
        Ship <span className="font-mono">.thclaws/</span> to a remote
        <span className="font-mono"> thclaws --serve</span> instance with one word.
      </p>

      <FieldLabel
        icon={<LinkIcon size={11} />}
        text="Pod URL"
        env="remoteAgentUrl in settings.json"
      />
      <div className="flex gap-1.5 mb-2">
        <input
          type="text"
          placeholder="https://agent-name.thcompany.ai"
          className="flex-1 px-2.5 py-1.5 rounded text-xs font-mono outline-none"
          style={{
            background: "var(--bg-primary)",
            color: "var(--text-primary)",
            border: "1px solid var(--border)",
          }}
          value={urlValue}
          onChange={(e) => setUrlDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSaveUrl();
          }}
          autoComplete="off"
        />
        <SaveButton onClick={onSaveUrl} disabled={!urlDirty} />
        <ClearButton
          onClick={onClearUrl}
          disabled={!cfg.url}
          title="Clear configured URL"
        />
      </div>

      <FieldLabel
        icon={<KeyRound size={11} />}
        text="Bearer token"
        env="THCLAWS_REMOTE_AGENT_TOKEN"
      />
      <div className="flex gap-1.5">
        <input
          // Same trick as KeyRow: while the sentinel is showing we use
          // `text` so the literal asterisks render at the actual token
          // length; once the user starts typing a real value we flip
          // to `password` so the new characters mask.
          type={isSentinel(tokenValue) ? "text" : "password"}
          placeholder="Paste pod's API token"
          className="flex-1 px-2.5 py-1.5 rounded text-xs font-mono outline-none"
          style={{
            background: "var(--bg-primary)",
            color: "var(--text-primary)",
            border: "1px solid var(--border)",
          }}
          value={tokenValue}
          onChange={(e) => setTokenDraft(e.target.value)}
          onFocus={(e) => {
            // Clicking the sentinel selects it so typing replaces it
            // in one go (matches KeyRow's UX — no manual select-all).
            if (isSentinel(e.currentTarget.value)) e.currentTarget.select();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSaveToken();
          }}
          autoComplete="off"
          spellCheck={false}
        />
        <SaveButton onClick={onSaveToken} disabled={!tokenDirty} />
        <ClearButton
          onClick={onClearToken}
          disabled={!cfg.has_token}
          title="Clear stored token"
        />
      </div>
      <FlashLine flash={flash} />
    </div>
  );
}

/// Project-scope toggle for `auto_learn` (dev-plan/27). The setting
/// drives session-end ingestion into the project's KMS plus the
/// throttled reconcile pass. Reading via `auto_learn_get` returns
/// the effective AppConfig value (project overlay wins over user
/// settings); writing via `auto_learn_set` updates the project's
/// `.thclaws/settings.json`. The shell reloads its AppConfig
/// immediately so the next session-end pass sees the new state
/// without a restart.
function AutoLearnSection() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [flash, setFlash] = useState<{ ok: boolean; msg: string } | undefined>(undefined);

  useEffect(() => {
    const unsub = subscribe((msg) => {
      if (msg.type === "auto_learn") {
        setEnabled(Boolean((msg as { enabled?: boolean }).enabled));
      } else if (msg.type === "auto_learn_result") {
        const r = msg as { ok?: boolean; error?: string; enabled?: boolean };
        if (r.ok === false) {
          setFlash({ ok: false, msg: r.error ?? "failed" });
        } else {
          setEnabled(Boolean(r.enabled));
          setFlash({ ok: true, msg: r.enabled ? "auto-learn enabled" : "auto-learn disabled" });
          setTimeout(() => setFlash(undefined), 2500);
        }
      }
    });
    send({ type: "auto_learn_get" });
    return unsub;
  }, []);

  const onToggle = (next: boolean) => {
    // Optimistic update — the `auto_learn_result` reply will
    // overwrite this with the canonical value if the save failed.
    setEnabled(next);
    send({ type: "auto_learn_set", enabled: next });
  };

  return (
    <div
      className="rounded p-3"
      style={{ background: "var(--bg-tertiary)", border: "1px solid var(--border)" }}
    >
      <div className="flex items-center gap-2 mb-1">
        <span className="text-xs font-semibold" style={{ color: "var(--text-primary)" }}>
          Auto-learn
        </span>
        <span className="text-[10px]" style={{ color: "var(--text-secondary)" }}>
          project-scope · saved to .thclaws/settings.json
        </span>
      </div>
      <p className="text-xs mb-2" style={{ color: "var(--text-secondary)" }}>
        At the end of each session, ingest the conversation into the project's KMS
        (one page per session) and reconcile the index periodically. The model
        can search and cite prior sessions on later turns. Token cost applies on
        ingest + reconcile.
      </p>
      <label
        className="flex items-center gap-2 text-xs select-none cursor-pointer"
        style={{ color: "var(--text-primary)" }}
      >
        <input
          type="checkbox"
          checked={enabled === true}
          disabled={enabled === null}
          onChange={(e) => onToggle(e.target.checked)}
        />
        <span>
          Enable auto-learn for this project
        </span>
      </label>
      <FlashLine flash={flash} />
    </div>
  );
}
