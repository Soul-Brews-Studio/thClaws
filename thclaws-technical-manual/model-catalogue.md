# Model catalogue

How thClaws stores per-model metadata — context window, max output,
modality, **pricing** — and how downstream consumers (external
dashboards, schedulers, billing) discover and use it. Reference for [dev-plan/24](../dev-plan/24-model-catalogue-pricing.md).

> **TL;DR**: thClaws ships a JSON catalogue compiled into the binary
> ([`crates/core/resources/model_catalogue.json`][cat]). It tracks
> context, max output, free-tier flag, modality, **and per-token-type
> pricing in USD per million tokens** (input / output / cache-read /
> cache-write / reasoning). The HTTP surface exposes it via
> [`GET /v1/models`](openai-api.md#get-v1models). Pricing is sourced
> from [LiteLLM's][litellm] community-maintained database via
> [`scripts/sync-catalogue-pricing.py`][sync], refreshed quarterly
> (or when a vendor announces a price change).

[cat]: ../thclaws/crates/core/resources/model_catalogue.json
[litellm]: https://github.com/BerriAI/litellm
[sync]: ../scripts/sync-catalogue-pricing.py

## Schema

JSON file at `crates/core/resources/model_catalogue.json`, embedded
into the binary via `include_str!`. Current version: **schema 4**
(bumped from 3 in v0.11.0 to add pricing fields). Older binaries see
a schema-version mismatch and fall back to compiled-in defaults —
graceful degradation, no panic.

```json
{
  "schema": 4,
  "source": "baseline 2026-05-19 — pricing synced from LiteLLM",
  "fetched_at": "2026-05-19T00:00:00Z",
  "providers": {
    "anthropic": {
      "list_url": "https://api.anthropic.com/v1/models",
      "default_context": 200000,
      "models": {
        "claude-sonnet-4-6": {
          "context": 200000,
          "max_output": 64000,
          "source": "https://docs.anthropic.com/... + litellm:claude-sonnet-4-6",
          "verified_at": "2026-05-19",
          "input_per_mtok": 3.0,
          "output_per_mtok": 15.0,
          "cached_input_per_mtok": 0.3,
          "cache_creation_per_mtok": 3.75
        }
      }
    }
  },
  "aliases": {
    "claude-sonnet": "claude-sonnet-4-6"
  }
}
```

### `ModelEntry` fields

| Field | Type | Required | Notes |
|---|---|---|---|
| `context` | `u32` | no | Total tokens (prompt + completion). Used by [context-composer.md](context-composer.md) for compaction triggers. |
| `max_output` | `u32` | no | Per-turn output ceiling. Used to cap `max_tokens` on completion calls. Falls through to provider default when absent. |
| `source` | string | no | URL of the doc page the row was verified against (vendor pricing page, LiteLLM commit, etc.). Multi-source rows separate with ` + `. |
| `verified_at` | `YYYY-MM-DD` | no | When the row was last reconciled against `source`. Stale rows trigger the refresh workflow. |
| `chat` | `bool` | no | `false` ⇒ non-chat modality (embeddings, audio, image-only). Filtered out of `/v1/models` and the chat picker. |
| `free` | `bool` | no | `true` ⇒ provider lists this model at $0. Cost compute returns `0`. |
| **`input_per_mtok`** | `f64` | no | USD per 1M uncached prompt tokens. |
| **`output_per_mtok`** | `f64` | no | USD per 1M completion tokens. |
| **`cached_input_per_mtok`** | `f64` | no | USD per 1M cache-READ tokens. Anthropic publishes a discounted rate (e.g. Sonnet 4.6: `$0.30`); OpenAI's `prompt_tokens_details.cached_tokens` falls back to `input_per_mtok` when this field is absent. |
| **`cache_creation_per_mtok`** | `f64` | no | USD per 1M cache-WRITE tokens. Anthropic charges a write premium (Sonnet 4.6: `$3.75` for 5min TTL); OpenAI auto-manages, leaves this `null`. The 5min vs 1h TTL distinction is collapsed into one field for v1; split later if a real use case demands it. |
| **`reasoning_per_mtok`** | `f64` | no | USD per 1M o1/o3 hidden reasoning tokens. Most providers fold this into output_per_mtok — leave absent there. |
| **`tier_billed`** | `bool` | no | `true` ⇒ the model has no fixed per-token price to meter — either a subscription-bundled model (Codex via ChatGPT Plus/Pro/Team) **or** a variable-priced router (`openrouter/fusion`, `openrouter/fusion+`) that can't carry a gateway price. `compute_cost_usd` returns `None`; the `audit-pricing` gate exempts these (they're BYOK-only, off-gateway). |
| `price_per_image_usd` | `f64` | no | USD per generated image — image models (`gpt-image-2`, `gemini-3.1-*-image`, `qwen-image-2.0`). Per-image, outside the `*_per_mtok` sum. |
| `video_price_per_second_usd` | `f64` | no | USD per second of generated video — LTX / DashScope / Veo. Metered per output-second at the gateway (`video_forward`). |
| `tts_price_per_kchar_usd` | `f64` | no | USD per 1,000 input characters — TTS models (`elevenlabs/eleven_v3`, `openai/gpt-4o-mini-tts`, `minimax/speech-02-hd`). Rows carry `kind:"tts"`. |
| `audio_price_per_second_usd` | `f64` | no | USD per second of input audio — speech-to-text (Groq Whisper). |

Media rows carry only their media price (token `*_per_mtok` = 0) and are
metered per-unit at the gateway, not per-token. **These media fields live
in the JSON + `model_pricing` only** — the engine's Rust `ModelEntry`
loader carries just `price_per_image_usd`, so `video`/`tts`/`audio`/`kind`
are gateway-side (silently ignored by the engine). Bold rows added in
schema 4 (dev-plan/24); image/video/tts/audio metering in dev-plan/40 + /53.

> **Pseudo-model rows.** `openrouter/fusion+` is a thClaws pseudo-model with no upstream `/v1/models` entry. It carries the variable-price sentinel and is **pinned** in `refresh-model-catalogue.py` (`PROVIDERS["openrouter"]["pin"]`) so a normal `make catalogue` run never prunes it. See [`provider-openai.md`](provider-openai.md) §7.

### `ProviderCatalogue` fields

| Field | Notes |
|---|---|
| `list_url` | Informational — the provider's `/v1/models` endpoint. Hit by `catalogue-seed` to discover new ids; not hit at runtime. |
| `default_context` | Fallback context when a model id routes to this provider but isn't catalogued. |
| `models` | Map keyed by exact model id as the provider's API returns them. `BTreeMap` for deterministic diffs. |

### `aliases`

User-friendly id → canonical id. Lets callers pass `claude-sonnet` and
get routed to `claude-sonnet-4-6`. `compute_cost_usd` resolves aliases
before lookup, so pricing reads through aliases too.

## Lookup semantics

```
Catalogue::find_entry(model)
  → resolve_alias(model) → canonical id
  → search owning provider (ProviderKind::detect)
  → fallback: strip `vendor/` prefixes one at a time
    (e.g. `openrouter/anthropic/claude-…` → `claude-…`)
  → first match wins
```

Same prefix-stripping cascade is used for context lookups and pricing
lookups, so a model id always resolves consistently across surfaces.

## `compute_cost_usd` decision tree

```rust
catalogue.compute_cost_usd(model, &TokenUsage { ... }) -> Option<f64>
```

| Entry state | Returns |
|---|---|
| Not in catalogue | `None` (caller surfaces "Cost unavailable") |
| `tier_billed: true` | `None` (caller surfaces "Tier-billed" — per-token math doesn't reflect actual billing) |
| `free: true` | `Some(0.0)` |
| Has at least one `*_per_mtok` field | `Some(sum)`, missing fields contribute `$0` |
| Has no pricing fields at all | `None` (distinct from free — caller surfaces "Pricing not yet curated") |

Cache math (Anthropic convention):

```
uncached_input = prompt_tokens − cached_input_tokens
                                  (saturating subtract — guards against
                                   buggy providers sending cached > prompt)
cost = uncached_input        × input_per_mtok / 1M
     + cached_input_tokens   × (cached_input_per_mtok ?? input_per_mtok) / 1M
     + cache_creation_tokens × cache_creation_per_mtok / 1M
     + completion_tokens     × output_per_mtok / 1M
     + reasoning_tokens      × (reasoning_per_mtok ?? output_per_mtok) / 1M
```

`compute_cost_usd` lives in [`crates/core/src/model_catalogue.rs`][cat-rs]
as Rust API only — **NOT exposed via HTTP**. It's used by thClaws's own
CLI / REPL / GUI to show per-turn cost in the local UI. Downstream
consumers compute their own cost from `/v1/models` rates × usage
counts; see [`openai-api.md` §usage block](openai-api.md#post-v1chatcompletions).

[cat-rs]: ../thclaws/crates/core/src/model_catalogue.rs

## Pricing data source

`model_catalogue.json` itself is the **single source of truth** for
pricing — every row is provenance-tagged (`source`, `verified_at`).
Provider `/v1/models` endpoints don't include rates, so figures are
hand-curated, with **[LiteLLM][litellm]** used as one convenient *input*
for per-token rates (below).

### Cloud billing pipeline (catalogue → gateway)

The catalogue → the gateway's `model_pricing` table is a one-way sync:

```
model_catalogue.json  ──make sync-cloud-pricing──▶  model_pricing (cloud DB)  ──▶  gateway meter
     (SSOT, raw prices)      (only writer)              (per-provider rows)         (×1.25 markup here)
```

- **`make sync-cloud-pricing`** (`scripts/sync-cloud-pricing.py`) is the
  **only** writer of `model_pricing`; it upserts token rows plus the media
  rows (`tts_price_per_kchar_usd` → `tts_micros_per_kchar`,
  `audio_price_per_second_usd`, `price_per_image_usd`,
  `video_price_per_second_usd`). `--prune` deactivates rows no longer in
  the catalogue (use with care — some rows, e.g. a Gemini TTS model, may
  live only in the DB).
- The **×1.25 platform markup is applied ONLY at gateway meter time**,
  never stored in a catalogue or `model_pricing` row (`meter.rs`).
- **`make audit-pricing`** is the release gate — every gateway-servable
  Featured model must carry a priced row (variable-priced routers are
  marked `tier_billed: true` and exempted).

### LiteLLM token-price refresh (a catalogue input, not the SSOT)

Refresh the catalogue's `*_per_mtok` fields from LiteLLM:

```sh
# From the workspace root:
make catalogue-pricing-sync       # fetches LiteLLM JSON, merges into catalogue
make catalogue-pricing-sync DRY_RUN=1   # preview only, no writes
```

The script (`scripts/sync-catalogue-pricing.py`):

1. Fetches `model_prices_and_context_window.json` from LiteLLM's
   main branch.
2. For each (provider, model_id) in the catalogue, probes LiteLLM
   for matching ids (bare id, then a small set of provider-prefix
   variants — `anthropic/`, `openai/`, `gemini/`, `vertex_ai/`,
   `openrouter/`, etc.). First match wins.
3. Converts LiteLLM's per-token cost → per-Mtok (× 1e6).
4. Merges into the catalogue, preserving non-pricing fields
   (`context`, `max_output`, `chat`, `free`). `source` gets
   ` + litellm:<key>` appended; `verified_at` updates to today.
5. Logs unmatched entries (provider-specific models LiteLLM doesn't
   carry — typically local Ollama, custom NVIDIA NIM passes,
   internal `agentic-press` / `thaillm` providers). Those rows
   simply have no pricing — `compute_cost_usd` returns `None`.

Initial sync (2026-05-19): **282 of 992 catalogue entries matched**.
Coverage by provider:

| Provider | Matched / Total |
|---|---|
| openai | 76 / 77 |
| anthropic | 11 / 14 |
| openai-responses | 3 / 3 |
| gemini | 11 / 12 |
| openrouter | 138 / 386 |
| dashscope | 25 / 166 |
| Local / custom | 0 (expected — LiteLLM doesn't track) |

## Adding a model manually

If LiteLLM doesn't carry an id you need, edit
`crates/core/resources/model_catalogue.json` directly:

```json
"some-provider": {
  "models": {
    "your-custom-model-id": {
      "context": 128000,
      "source": "https://your-provider.example.com/pricing",
      "verified_at": "2026-05-19",
      "input_per_mtok": 1.50,
      "output_per_mtok": 6.00
    }
  }
}
```

Conventions:

- `source` should be a URL the next maintainer can hit to re-verify.
- `verified_at` is `YYYY-MM-DD` of the day you eyeballed the price.
  Stale entries (>90 days) are flagged for re-verification by the
  refresh workflow.
- Pricing fields are USD per **million tokens** (not per token —
  this is the conversion from LiteLLM's wire format).
- Omit fields you don't have data for. `compute_cost_usd` handles
  missing fields by treating them as `$0` contribution (entries
  with no priced fields at all return `None` instead).

After editing, re-run `make build` so the binary picks up the new
embedded data. `cargo test` exercises the catalogue tests; the
`schema_v3_rejected_after_bump` test guards against unintentional
schema rollback.

## Versioning + migration

`CURRENT_SCHEMA: u32 = 4` (was 3 in v0.9.10 and earlier). The
loader (`Catalogue::from_json_str`) returns `None` on mismatch —
silent fallback to compiled-in defaults instead of crashing.

Migration policy:

- **Additive changes** (new optional fields on `ModelEntry`): no
  schema bump needed; older binaries ignore unknown fields.
- **Required-field changes / renames**: bump `CURRENT_SCHEMA`. The
  bump is a semver-minor on thClaws (the schema is part of the
  public API surface that LiteLLM-compatible clients read).

## Downstream consumers

- **External tools** (n8n, Zapier, custom dashboards) hit
  [`GET /v1/models`](openai-api.md#get-v1models) and read the
  `pricing` block per model. The shape is stable — fields are
  additive, never renamed.

## See also

- [`openai-api.md`](openai-api.md) — `/v1/models` response shape +
  `usage` block fields
- [`../dev-plan/24-model-catalogue-pricing.md`](../dev-plan/24-model-catalogue-pricing.md)
  — full design rationale and phase breakdown
