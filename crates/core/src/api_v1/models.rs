//! `GET /v1/models` — OpenAI-compatible model list.
//!
//! Walks the [`crate::model_catalogue::EffectiveCatalogue`] (cache layer
//! → embedded baseline) and emits one row per known model id. `owned_by`
//! is the provider name as the catalogue records it
//! (`anthropic`, `openai`, `openrouter`, `gemini`, etc.).
//!
//! No network calls — pure local lookup. Clients that need fresh data
//! refresh the catalogue via `/models refresh` in the REPL/GUI.

use axum::response::Json;
use serde::Serialize;

use super::AuthOk;
use crate::model_catalogue::EffectiveCatalogue;

#[derive(Serialize)]
pub struct ModelListResponse {
    pub object: &'static str,
    pub data: Vec<ModelRow>,
}

#[derive(Serialize)]
pub struct ModelRow {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: String,
    /// dev-plan/24: optional context window the catalogue knows
    /// about. OpenAI's own /v1/models doesn't return this; it's
    /// the thClaws extension that LiteLLM-compatible clients
    /// already look for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// dev-plan/24: optional pricing block, populated when the
    /// catalogue entry has at least one *_per_mtok rate. Discovery
    /// surface — consumers compute cost themselves using these
    /// rates × usage tokens. thClaws doesn't emit a `cost_usd`
    /// field in chat responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<PricingBlock>,
}

/// USD-denominated rates, per million tokens. All sub-fields optional —
/// a row may have only some token types priced (e.g. providers that
/// don't publish a cache-tier discount). Currency hardcoded "USD" so
/// future multi-currency support is a non-breaking change (add
/// alternate currency entries instead of mutating this).
#[derive(Serialize)]
pub struct PricingBlock {
    pub currency: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_per_mtok: Option<f64>,
    /// `true` when this model is bundled in a subscription tier
    /// (ChatGPT Plus/Pro/Team for Codex via the chatgpt-codex/*
    /// route). Consumers should show "tier-billed" instead of a $
    /// amount.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier_billed: Option<bool>,
    /// `true` when the provider publishes this model as free (e.g.
    /// OpenRouter's free-tier models). Cost computation returns 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free: Option<bool>,
}

impl PricingBlock {
    /// Build from a catalogue [`ModelEntry`]. Returns `None` when the
    /// entry has no pricing signals at all — keeps the response JSON
    /// minimal for niche / un-priced models.
    fn from_entry(e: &crate::model_catalogue::ModelEntry) -> Option<Self> {
        let any_field = e.input_per_mtok.is_some()
            || e.output_per_mtok.is_some()
            || e.cached_input_per_mtok.is_some()
            || e.cache_creation_per_mtok.is_some()
            || e.reasoning_per_mtok.is_some()
            || e.tier_billed.is_some()
            || e.free.is_some();
        if !any_field {
            return None;
        }
        Some(PricingBlock {
            currency: "USD",
            input_per_mtok: e.input_per_mtok,
            output_per_mtok: e.output_per_mtok,
            cached_input_per_mtok: e.cached_input_per_mtok,
            cache_creation_per_mtok: e.cache_creation_per_mtok,
            reasoning_per_mtok: e.reasoning_per_mtok,
            tier_billed: e.tier_billed,
            free: e.free,
        })
    }
}

pub async fn list_models(_auth: AuthOk) -> Json<ModelListResponse> {
    let cat = EffectiveCatalogue::load();
    let created = chrono::Utc::now().timestamp();
    let mut rows: Vec<ModelRow> = Vec::new();

    // Walk cache first (preferred), then fall back to baseline for ids
    // only the baseline knows about. Dedupe by id.
    let mut seen = std::collections::HashSet::new();
    let layers = [cat.cache.as_ref(), Some(&cat.baseline)];
    for layer in layers.into_iter().flatten() {
        for (provider_name, provider_cat) in &layer.providers {
            for (model_id, entry) in &provider_cat.models {
                if !seen.insert(model_id.clone()) {
                    continue;
                }
                // Skip non-chat rows (embeddings, audio, image) so the
                // list matches what a chat client can actually call.
                if entry.chat == Some(false) {
                    continue;
                }
                rows.push(ModelRow {
                    id: model_id.clone(),
                    object: "model",
                    created,
                    owned_by: provider_name.clone(),
                    context_window: entry.context,
                    pricing: PricingBlock::from_entry(entry),
                });
            }
        }
    }

    // Sort by id so clients see a stable, alphabetical list — matches
    // OpenAI's own list ordering and makes diffs easy to spot.
    rows.sort_by(|a, b| a.id.cmp(&b.id));

    Json(ModelListResponse {
        object: "list",
        data: rows,
    })
}
