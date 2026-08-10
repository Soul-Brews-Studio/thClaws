# thClaws Gateway Overlay

`thclaws_gateway.rs` (`providers/thclaws_gateway.rs`, 203 LOC) is **not a `Provider` impl** — it's a transparent overlay that runs inside `build_provider` (`repl.rs:3188+`) and rewrites the base URL + auth value of cloud providers when the user has toggled the gateway on for that provider and has a gateway access key configured.

When active, the provider keeps its native wire format — OpenAI clients still talk Chat Completions, Anthropic still talks `/v1/messages`, Gemini still talks the GenerateContent API. The gateway is a path-prefix-routed reverse proxy that re-injects the real upstream credentials on its side, so the only knobs that change at the desktop are:

1. **Base URL** → `<gateway>/<provider-segment>/<original-path>`
2. **Auth header value** → the gateway access key

The header **scheme** is unchanged: OpenAI / OpenRouter clients still send `Authorization: Bearer …`, Anthropic still sends `x-api-key`, Gemini still sends `x-goog-api-key`. The gateway's `auth::require_bearer` accepts all three carriers.

> **Not to be confused with the EE policy gateway.** [`provider-gateway.md`](provider-gateway.md) documents `providers/gateway.rs`, an **enterprise-policy substitution** that replaces every cloud provider with a single OpenAI-Chat-Completions client pointed at LiteLLM / Portkey / Helicone / etc. That overlay is org-policy driven and unconditional; this overlay is user-toggled per-provider and preserves wire shape. They share a name and never share a file.

**Source:** `crates/core/src/providers/thclaws_gateway.rs`
**Server-side:** `thclaws-cloud/gateway/` (workspace-only — not in the public repo; runs on the operator's k3s cluster behind `gateway.thclaws.cloud`)
**Trigger:** per-provider toggle in `AppConfig.gateway_use_for: Vec<String>` + a resolvable access key
**Shipped:** v0.9.6 (gateway server + desktop overlay landed together)

---

## 1. Base URL

```rust
pub const GATEWAY_BASE_URL: &str = "https://gateway.thclaws.cloud";
```

The base URL is **fixed**. End users can't change it from the Settings UI — there's nothing to misconfigure. The DNS resolves to the operator's k3s ingress; the cluster terminates TLS via cert-manager and routes by Host header.

For development against a staging gateway or a local docker-compose run, set `THCLAWS_GATEWAY_BASE_URL`:

```sh
THCLAWS_GATEWAY_BASE_URL=http://localhost:8080 cargo run --bin thclaws
```

`resolve_base_url()` honors the env var at lookup time (not at startup), so a flip survives without an app restart.

---

## 2. Per-provider segment

Each provider gets a fixed segment under the gateway base. The matching server-side routes live in `thclaws-cloud/gateway/src/routes/mod.rs`:

```rust
pub fn provider_segment(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::OpenAI | ProviderKind::OpenAIResponses => Some("openai"),
        ProviderKind::Anthropic => Some("anthropic"),
        ProviderKind::Gemini => Some("google"),
        ProviderKind::OpenRouter => Some("openrouter"),
        _ => None,
    }
}
```

**10 LLM segments** are wired (`shared::GATEWAY_ALL_PROVIDERS`) — exactly the `ProviderTier::Featured` providers, an equality a unit test enforces. The ones with an OpenAI-compatible upstream share the `compat` proxy:

| ProviderKind | Segment | Example URL |
|---|---|---|
| `OpenAI` / `OpenAIResponses` | `openai` | `https://gateway.thclaws.cloud/openai/v1/chat/completions` |
| `Anthropic` | `anthropic` | `https://gateway.thclaws.cloud/anthropic/v1/messages` |
| `Gemini` | `google` | `https://gateway.thclaws.cloud/google/v1/...` |
| `OpenRouter` | `openrouter` | `https://gateway.thclaws.cloud/openrouter/...` |
| `DashScope` `ZAi` `DeepSeek` `Minimax` `XAi` `Moonshot` | `dashscope` / `zai` / `deepseek` / `minimax` / `xai` / `moonshot` | `…/<segment>/v1/chat/completions` (compat proxy) |

Anything outside this set (Ollama, LMStudio, AgentSdk, OllamaCloud, TokenRouter, …) returns `None` from `provider_segment` and bypasses the overlay — they call upstream directly (local providers need no proxy; OllamaCloud/TokenRouter have their own auth/pricing model).

`QwenCloud`, `ThaiLLM` and `Groq` were in this table until 2026-08-10. The gateway now proxies only what it sells, and the sold tier is the ten Featured providers: thaillm is free upstream but rate-limited far below anything a paid tier can promise, and qwen-cloud duplicates dashscope from another region. All three still work on the desktop with the user's own key — what was withdrawn is the proxy, not the provider. Groq is the subtle one: its **LLM** segment is gone while `/groq/audio` (Whisper) stays, because the media routes are billed and routed independently. Its Whisper rows therefore keep syncing to `model_pricing` while its chat rows do not — see `MEDIA_ONLY` in `scripts/sync-cloud-pricing.py`.

**Media routes.** Beyond LLMs the gateway meters generation per-unit (dev-plan/53): per-second video (`/ltx`, DashScope `video-synthesis`), per-character TTS (`/elevenlabs`, `/openai/v1/audio/speech`, `/minimax/v1/t2a_v2`), per-image (`/dashscope/...multimodal-generation`), Whisper per-second (`/groq/audio`), and Kie async jobs billed by `creditsConsumed`. See [`provider-gateway.md`](provider-gateway.md) and the gateway server's `proxy/media.rs`.

---

## 3. The overlay shape

```rust
pub struct GatewayOverlay {
    pub base_url: String,    // <GATEWAY_BASE_URL>/<segment>, no trailing slash
    pub access_key: String,  // resolved gateway key
}

pub fn for_kind(config: &AppConfig, kind: ProviderKind) -> Option<GatewayOverlay>;
```

`for_kind` returns `Some(overlay)` when **all three** conditions hold:

1. `provider_segment(kind)` is `Some(...)` — the kind has a gateway route.
2. `config.gateway_use_for` contains the kind's segment (case-insensitive) — the user has toggled the per-provider switch on in Settings.
3. `resolve_access_key()` returns `Some(key)` — the user has a gateway access key set.

Any of those failing → `None`, and `build_provider` falls through to the native upstream URL with the user's per-provider API key.

---

## 4. Access key resolution

```rust
fn resolve_access_key() -> Option<String> {
    if let Ok(v) = std::env::var("THCLAWS_GATEWAY_API_KEY") {
        let trimmed = v.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    crate::secrets::get("gateway")
}
```

Priority:

1. **`THCLAWS_GATEWAY_API_KEY`** env var. Handy for CI / scripted runs / debugging — no Settings UI interaction needed.
2. **OS keychain bundle**, account `gateway`. This is what the Settings UI writes when the user pastes their access key.

The keychain path honors the user's `Backend` preference (`Keychain` vs `Dotenv`) via `crate::secrets::get`. Gateway access keys are long-lived, organisation-billed credentials — distinct from short-lived SSO id_tokens (which always go to keychain via `keychain_get_raw`, see [`sso.md`](sso.md)).

---

## 5. The build_provider integration

Every gateway-eligible `ProviderKind` arm in `repl.rs::build_provider` queries `for_kind` first:

```rust
ProviderKind::Anthropic => {
    let overlay = crate::providers::thclaws_gateway::for_kind(config, kind);
    let provider = match overlay {
        Some(o) => AnthropicProvider::new(o.access_key)
            .with_base_url(format!("{}/v1/messages", o.base_url)),
        None => AnthropicProvider::new(api_key),
    };
    Ok(Arc::new(provider))
}
```

The shape is identical for OpenAI, OpenRouter, and Gemini — only the path suffix differs (`/v1/chat/completions`, `/api/v1/chat/completions`, no suffix). The wire format is the provider's native shape; the gateway transparently forwards.

---

## 6. Settings UI surface

The frontend exposes two controls on the Settings → Gateway pane:

1. **Access key** input — single-line password field. Saves to keychain account `gateway` via the standard `secrets::set` IPC path.
2. **Use for** checkboxes — one per gateway-eligible provider (OpenAI, Anthropic, Google, OpenRouter). Maps 1:1 to `AppConfig.gateway_use_for`.

The IPC handlers (`ipc.rs:1187` and `:1203`) expose this state as `{ "access_key_set": bool, "use_for": [String] }` and accept updates via `set_gateway_use_for`.

A user with no access key but checked toggles → `for_kind` returns `None` (access-key gate), and providers transparently fall back to upstream. Same for a key set with no toggles. The two-knob design is intentional: toggling off "use gateway for Anthropic" while keeping OpenAI on the gateway is a single-checkbox change, no key rotation.

---

## 7. Tests

`thclaws_gateway::tests` — 5 tests, all under a shared `ENV_LOCK: Mutex<()>` since they mutate `THCLAWS_GATEWAY_*` env vars and cargo parallelises lib tests:

- `provider_segment_covers_supported_kinds` — Anthropic / OpenAI / Gemini / OpenRouter map to their segments; Ollama / LMStudio map to `None`.
- `for_kind_returns_none_when_provider_not_enabled` — toggle off → no overlay.
- `for_kind_returns_none_when_access_key_missing` — no key → no overlay.
- `for_kind_uses_fixed_base_url_by_default` — default base URL.
- `for_kind_honors_base_url_env_override` — `THCLAWS_GATEWAY_BASE_URL` flips the base URL at lookup time.

No end-to-end test of the live wire path — that requires the gateway server (workspace-only `thclaws-cloud/gateway/`) running. Manual repro: `docker compose up gateway` in the workspace + `THCLAWS_GATEWAY_BASE_URL=http://localhost:8080 cargo run`.

---

## 8. Notable behaviors / gotchas

- **Base URL is constant in shipped builds.** Don't try to add a Settings UI knob — the operator commits to a single canonical host, and rotating it would require a coordinated push of new desktop builds. Use `THCLAWS_GATEWAY_BASE_URL` for dev only.
- **Access key vs SSO id_token are different things.** SSO authenticates *which user* you are against the gateway. The access key authorises *what your client can do*. Keys are minted via `POST /v1/keys` with a valid id_token (see [`docs/azure-setting.md`](../docs/azure-setting.md) step 6 for a `curl` example).
- **No per-key gateway preferences yet.** Every key carries the same upstream-routing surface. Per-key rate-limit / model-allowlist is a server-side feature for a later release.
- **OllamaCloud is NOT routed through the gateway.** It's hosted (so not "local") but has its own auth model; no segment is wired.
- **Header passthrough preserves wire shape.** A request bug in the Anthropic provider would manifest the same way whether the gateway is on or off — the gateway just changes the destination + the credential, not the body or other headers.
- **Server crate is workspace-only.** The `thclaws-cloud/gateway/` Axum service is excluded from `make sync-public` because it's operator infra (runs on our k3s cluster). The desktop overlay you're reading about here ships normally to end users.

---

## 9. Cross-references

- [`provider-gateway.md`](provider-gateway.md) — the EE-policy gateway overlay (distinct mechanism, same name, easy to confuse).
- [`sso.md`](sso.md) — SSO id_token flow that mints the access keys consumed by this overlay.
- [`docs/azure-setting.md`](../docs/azure-setting.md) — operator walkthrough for Azure Entra registration + `POST /v1/keys` smoke test.
- [`providers.md`](providers.md) — `build_provider` dispatch overview.
