# OpenAI Chat Completions provider

`OpenAIProvider` (`providers/openai.rs`, 1429 LOC) speaks the OpenAI Chat Completions SSE format. It's the workhorse: **16 of the 25 `ProviderKind` variants** route to this single impl with different URL/auth/prefix-strip configurations. The provider was deliberately built with a configuration knob design (`with_base_url` + `with_strip_model_prefix` + `with_api_key_header` + `with_list_models_url`) so adding a new OpenAI-compat aggregator is a `match` arm in `build_provider` rather than a new file.

The 16 variants: `OpenAI` (api.openai.com), `OpenRouter`, `TokenRouter`, `DashScope`, `QwenCloud`, `ZAi`, `LMStudio`, `OpenAICompat`, `DeepSeek`, `ThaiLLM`, `Nvidia`, `Minimax`, `Moonshot`, `XAi`, `Groq`, `OpenCodeGo`.

**Source:** `crates/core/src/providers/openai.rs`
**Constants:**
- `DEFAULT_API_URL = "https://api.openai.com/v1/chat/completions"`

**Cross-references:**
- [`providers.md`](providers.md) — `Provider` trait, `StreamRequest`, `ProviderEvent`
- [`provider-anthropic.md`](provider-anthropic.md) — wire-format contrast
- [`provider-responses.md`](provider-responses.md) — OpenAI's NEWER API (codex/o-series) with a different shape
- [`provider-gateway.md`](provider-gateway.md) — when EE gateway is active, the gateway also speaks this format

---

## 1. Wire format

SSE chunks: `data: {chunk_json}\n\n` followed by a final `data: [DONE]` terminator. **No `event:` lines** (Anthropic has them; OpenAI doesn't).

```
data: {"id":"1","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}

data: {"id":"1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"}}]}

data: {"id":"1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":" world"}}]}

data: {"id":"1","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"1","model":"gpt-4o","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}

data: [DONE]
```

Three notable shapes:

1. **`finish_reason` lives on the last content chunk**, not as a separate event (Anthropic uses a dedicated `message_delta` event).
2. **Usage arrives in a tail frame** with `choices: []` and the real token counts. Triggered by sending `stream_options: {include_usage: true}` in the request body. Without this, the parser would report 0 in/0 out for every turn.
3. **Tool calls stream via `choices[0].delta.tool_calls[i].function.arguments`** — a new tool call is marked by a new `index` value. The first chunk for each index includes `id` + `function.name`; subsequent chunks for the same index carry only `arguments` deltas.

### Stateful parser

The wire format requires state across chunks because:
- A new `tool_call.index` arriving requires synthesizing a `ContentBlockStop` for the previously-active tool BEFORE opening the new one (Anthropic emits explicit `content_block_stop`; OpenAI doesn't).
- `MessageStart` must be emitted exactly once per stream; the parser tracks `seen_message_start`.
- The trailing usage frame (post-`finish_reason`) needs `emitted_message_stop` to know it's an additional `MessageStop` carrying the real usage, not a duplicate.

```rust
#[derive(Default, Debug)]
pub struct ParseState {
    pub seen_message_start: bool,
    pub active_tool_index: Option<i64>,
    pub emitted_message_stop: bool,
}

impl ParseState {
    fn flush_eof(&mut self) -> Vec<ProviderEvent> {
        // Synthesize a missing ContentBlockStop if the stream ended
        // mid-tool — happens with truncated provider responses.
        if self.active_tool_index.is_some() { ... }
    }
}
```

### Wire-element → `ProviderEvent` mapping

| Source | Mapped to |
|---|---|
| First chunk parsed (any) | `MessageStart { model: chunk.model }` (once, gated by `seen_message_start`) |
| `delta.content: "..."` (non-empty) | `TextDelta(content)` |
| `delta.reasoning_content: "..."` (non-empty) | `ThinkingDelta(reasoning)` (only echoed back to providers in `model_uses_reasoning_content` allowlist) |
| `delta.tool_calls[i]` with new `index` | (synthesize `ContentBlockStop` for prev) → `ToolUseStart { id, name }` |
| `delta.tool_calls[i].function.arguments: "..."` | `ToolUseDelta { partial_json: args }` |
| `choice.finish_reason: "..."` | (synthesize `ContentBlockStop` if tool active) → `MessageStop { stop_reason, usage: parsed_or_None }` |
| Trailing frame: `choices: []` + top-level `usage` | `MessageStop { stop_reason: "stop", usage: parsed }` (only after a prior MessageStop fired) |
| `data: {"error": {...}}` | `Err(Error::Provider("upstream error: {message}"))` |
| `data: [DONE]` | (ignored — final usage frame is what carries real data) |

The "upstream error inside SSE data frame" guard handles broken OpenAI-compat gateways that return HTTP 200 + a JSON error body — without this, the turn would silently complete with no output.

---

## 2. Struct + builder

```rust
pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    base_url: String,                       // defaults to DEFAULT_API_URL
    strip_model_prefix: Option<String>,     // e.g. "openrouter/" / "moonshot/" / "zai/"
    api_key_header: Option<String>,         // None → "authorization" with "Bearer {key}"
    list_models_url: Option<String>,        // override for non-derived /models path
    model_override: Option<String>,         // replace req.model before wire (openrouter/fusion+ → outer; §7)
    injected_tools: Vec<Value>,             // appended to tools[] (e.g. openrouter:fusion)
    tool_choice: Option<Value>,             // body tool_choice override (e.g. "required")
}

impl OpenAIProvider {
    pub fn new(api_key: impl Into<String>) -> Self;
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self;
    pub fn with_strip_model_prefix(mut self, prefix: impl Into<String>) -> Self;
    pub fn with_api_key_header(mut self, name: impl Into<String>) -> Self;
    pub fn with_list_models_url(mut self, url: impl Into<String>) -> Self;
    pub fn with_model_override(mut self, model: impl Into<String>) -> Self;  // §7 fusion+
    pub fn with_injected_tool(mut self, tool: Value) -> Self;                // §7 fusion+
    pub fn with_tool_choice(mut self, choice: Value) -> Self;                // §7 fusion+
}
```

### Auth

```rust
fn auth_header_name(&self) -> &str {
    self.api_key_header.as_deref().unwrap_or("authorization")
}
fn auth_header_value(&self) -> String {
    match &self.api_key_header {
        Some(_) => self.api_key.clone(),                   // raw key, no Bearer
        None => format!("Bearer {}", self.api_key),        // standard
    }
}
```

The `Some(_)` branch sends the raw key (no `Bearer ` prefix) — used for Azure-style `api-key:` headers. Default is `Authorization: Bearer <key>`.

### Model prefix strip

Applied at the start of `stream()`:
```rust
if let Some(prefix) = &self.strip_model_prefix {
    if let Some(rest) = req.model.strip_prefix(prefix.as_str()) {
        req.model = rest.to_string();
    }
}
```

Mutates `req.model` before serializing. As of v0.61.0 the actual call is `strip_wire_prefix(&req.model, …)`, which guards the OpenRouter vendor collision (`openrouter/fusion` / `openrouter/auto` keep their id — see §7). Configured per-variant:
- `OpenRouter` → `openrouter/`
- `ZAi` → `zai/`
- `LMStudio` → `lmstudio/`
- `OpenAICompat` → `oai/`
- `ThaiLLM` → `thaillm/`
- `OpenAI` / `DashScope` / `DeepSeek` → no strip (model ids passed verbatim)

---

## 3. Request body construction (`build_body`)

```rust
{
  "model": "<post-strip>",
  "max_completion_tokens": 1024,
  "messages": [...],
  "stream": true,
  "stream_options": {"include_usage": true},
  "tools": [{"type": "function", "function": {"name": ..., "description": ..., "parameters": ...}}, ...]
}
```

- `max_completion_tokens` (the newer name) is sent — older o-series models accept both `max_tokens` and `max_completion_tokens`; gpt-5+ series requires the new name.
- `stream_options.include_usage: true` requests the trailing usage frame.
- `tools` is omitted when empty. `build_body` is an **instance method**: it appends any `self.injected_tools` (e.g. the `openrouter:fusion` tool — see §7 `openrouter/fusion+`) after the agent's function tools, and sets `tool_choice` from `self.tool_choice` when present.

### Message conversion (`messages_to_openai`)

Most subtle part of the impl. Each canonical `Message { role, content: Vec<ContentBlock> }` is decomposed into one or more OpenAI-shape messages, because OpenAI splits things our `ContentBlock` keeps unified:

| `ContentBlock` | Becomes |
|---|---|
| `Text { text }` | Concatenated into `content_text` for this message |
| `Thinking { content }` | Concatenated into `reasoning_text`; **only emitted as `reasoning_content` field if** `model_uses_reasoning_content(req.model)` returns true (see §6) |
| `Image { source: Base64 { media_type, data } }` | Queued as `inline_user_images`; emitted as `image_url` blocks in OpenAI's array-form `content` |
| `ToolUse { id, name, input }` | Pushed to `tool_calls: [{id, type: "function", function: {name, arguments: JSON.stringify(input)}}]` on the assistant message |
| `ToolResult { tool_use_id, content, .. }` | Queued as `trailing_tool_results`; emitted as a separate `{role: "tool", tool_call_id, content}` message AFTER the assistant message |

Then the emission step builds:
1. The assistant/user/system message itself, with one of three content shapes:
   - `content: "string"` when only text
   - `content: null` + `tool_calls: [...]` when only tool calls
   - `content: [{"type":"text","text":...}, {"type":"image_url","image_url":{"url":"data:image/png;base64,..."}}]` when images are present
2. Then ALL tool messages back-to-back (`role: "tool"`, one per `tool_use_id`).
3. Then ONE combined synthetic user message carrying every image returned by any tool call:
   ```json
   {"role": "user", "content": [
       {"type": "text", "text": "(image attached from preceding tool_result: call_abc)"},
       {"type": "text", "text": "from call_abc:"},
       {"type": "image_url", "image_url": {"url": "data:image/png;base64,..."}}
   ]}
   ```

**Why the back-to-back tool messages then ONE combined image message?** OpenAI's contract: an assistant message with `tool_calls` MUST be followed by tool-role messages responding to every `tool_call_id`, with no other roles interleaved. An earlier (broken) version of this code emitted a synthetic user message after each individual tool message — fine for one tool call but a 400 from the server when the model batched N parallel calls (`tool_call_ids did not have response messages`). The combined-image-message-after-all-tools shape is the documented OpenAI pattern for getting tool-returned imagery in front of a vision-capable model.

### System prompt

Prepended as `messages[0] = {role: "system", content: sys}` if `req.system` is non-empty. Unlike Anthropic, OpenAI keeps system in the messages array (no top-level `system` field).

### Sample minimal body

```rust
StreamRequest {
    model: "gpt-4o".into(),
    system: Some("you are helpful".into()),
    messages: vec![Message::user("hi")],
    tools: vec![],
    max_tokens: 1024,
    thinking_budget: None,   // ignored by OpenAI Chat Completions
}
```
produces:
```json
{
    "model": "gpt-4o",
    "max_completion_tokens": 1024,
    "messages": [
        {"role": "system", "content": "you are helpful"},
        {"role": "user", "content": "hi"}
    ],
    "stream": true,
    "stream_options": {"include_usage": true}
}
```

---

## 4. Stream pipeline

```rust
async fn stream(&self, mut req: StreamRequest) -> Result<EventStream> {
    if let Some(m) = &self.model_override { req.model = m.clone(); }            // fusion+ → outer model
    req.model = strip_wire_prefix(&req.model, self.strip_model_prefix.as_deref());  // vendor-collision-safe (§7)
    let body = self.build_body(&req);   // instance method (appends injected_tools + tool_choice)
    let resp = self.client.post(&self.base_url)
        .header(self.auth_header_name(), self.auth_header_value())
        .header("content-type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() { return Err(...); }   // body redacted

    let byte_stream = resp.bytes_stream();
    let raw_dump = super::RawDump::new(format!("openai {}", req.model));

    Ok(Box::pin(try_stream! {
        let mut buffer = String::new();
        let mut state = ParseState::default();
        while let Some(chunk) = byte_stream.next().await {
            buffer.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(boundary) = buffer.find("\n\n") {
                let event_text: String = buffer.drain(..boundary + 2).collect();
                for event in parse_chunk(event_text.trim_end_matches('\n'), &mut state)? {
                    if let ProviderEvent::TextDelta(ref s) = event { raw.push(s); }
                    yield event;
                }
            }
        }
        for event in state.flush_eof() { yield event; }
        raw.flush();
    }))
}
```

`parse_chunk` is stateful (takes `&mut ParseState`) and returns `Vec<ProviderEvent>` per chunk because one chunk can produce multiple events (e.g. delta.content + tool_calls in same frame; finish_reason → ContentBlockStop + MessageStop).

`flush_eof()` — synthesizes a closing `ContentBlockStop` if the stream ended with an unclosed tool call (truncated provider response). Without this, the assembler would keep waiting for `ContentBlockStop` and never emit the partial tool_use as a complete `AssembledEvent::ToolUse`.

---

## 5. `list_models`

```rust
async fn list_models(&self) -> Result<Vec<ModelInfo>> {
    let models_url = self.list_models_url.clone().unwrap_or_else(|| {
        // Derive from base_url: /v1/chat/completions → /v1/models
        self.base_url.rsplit_once("/chat/completions")
            .map(|(base, _)| format!("{base}/models"))
            .unwrap_or_else(|| format!("{}/models", self.base_url.trim_end_matches('/')))
    });
    // GET → JSON {data: [{id, ...}]} → ModelInfo with prefix re-applied
}
```

URL transform: replaces `/chat/completions` with `/models`. Override via `with_list_models_url` for backends where this doesn't work.

**Prefix re-application:** when a `strip_model_prefix` is configured, the listing prepends it back so users can paste IDs straight into `/model`. E.g. Moonshot`s `/v1/models` returns `kimi-k2.6`; the listing surfaces `moonshot/kimi-k2.6` so `/model moonshot/kimi-k2.6` round-trips through `detect()` correctly.

---

## 6. `model_uses_reasoning_content` allowlist

```rust
pub fn model_uses_reasoning_content(model: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "deepseek/deepseek-v4",  "deepseek-v4",
        "openai/o1",  "openai/o3",  "openai/o4",
        "deepseek/deepseek-r1",  "deepseek-r1",
        "deepseek-reasoner",
    ];
    PATTERNS.iter().any(|p| model.to_lowercase().contains(p))
}
```

Conservative substring allowlist. Models in the allowlist:
- ECHO `reasoning_content` from `delta.reasoning_content` as `ThinkingDelta` events
- INCLUDE `reasoning_content` field on assistant messages in subsequent turn requests (the upstream rejects with 400 if prior reasoning is dropped)

Models NOT in the allowlist drop `Thinking` content blocks during request serialization — saves tokens and avoids surprising servers that don't recognize the field. Adding new thinking-model families means appending to `PATTERNS`.

Match is against the model id AFTER `strip_model_prefix` (so `openrouter/anthropic/claude-sonnet-4-6` becomes `anthropic/claude-sonnet-4-6` for matching — the bare id the upstream sees). Case-insensitive.

Direct OpenAI o-series calls go through `OpenAIResponsesProvider`, NOT this provider — the `openai/o1` etc. patterns only catch the OpenRouter proxy form.

---

## 7. The 9 sibling variants

All construct `OpenAIProvider` with a different URL/auth/prefix combination. Routing happens in `repl.rs::build_provider`.

### `OpenAI` — first-party

```rust
ProviderKind::OpenAI => Ok(Arc::new(OpenAIProvider::new(api_key)))
```

Default URL (api.openai.com), default auth (`Authorization: Bearer`), no strip prefix. Routing: `gpt-` / `o1-` / `o3-` / `o3` / `o4-`.

### `OpenRouter`

```rust
OpenAIProvider::new(api_key)
    .with_base_url("https://openrouter.ai/api/v1/chat/completions")
    .with_strip_model_prefix("openrouter/")
```

Models look like `openrouter/anthropic/claude-sonnet-4-6`. Strip yields `anthropic/claude-sonnet-4-6` which OpenRouter routes to upstream.

**Vendor-prefix collision (`strip_wire_prefix`).** OpenRouter's own router models — `openrouter/fusion`, `openrouter/auto` — have vendor `openrouter`, which collides with the `openrouter/` routing prefix. Naively stripping sent the vendor-less `fusion` upstream → `404 No endpoints found that support tool use`. `strip_wire_prefix(model, prefix)` keeps the id intact when stripping `openrouter/` would leave a **bare single segment** (a real OpenRouter id is always `vendor/model`, i.e. contains a `/`). Other providers' prefixes strip to a bare id by design and are unaffected.

```rust
fn strip_wire_prefix(model: &str, strip_prefix: Option<&str>) -> String {
    let Some(prefix) = strip_prefix else { return model.to_string(); };
    match model.strip_prefix(prefix) {
        Some(rest) if prefix == "openrouter/" && !rest.contains('/') => model.to_string(),
        Some(rest) => rest.to_string(),
        None => model.to_string(),
    }
}
```

### `openrouter/fusion+` — configurable Fusion (v0.61.0+)

`openrouter/fusion+` is a **thClaws pseudo-model**, not a real OpenRouter id. The bare `openrouter/fusion` uses OpenRouter's default deliberation panel; `fusion+` exposes the panel/judge/limit parameters via the `openrouter:fusion` tool. It's wired with three `OpenAIProvider` builders set in `build_provider`'s OpenRouter arm when `config.model == config::FUSION_PLUS_MODEL`:

```rust
let mut provider = OpenAIProvider::new(key)
    .with_base_url(base).with_strip_model_prefix("openrouter/");
if config.model == crate::config::FUSION_PLUS_MODEL {
    let f = &config.openrouter_fusion;                 // FusionConfig
    provider = provider
        .with_model_override(f.outer_model.clone())    // wire model = outer, NOT "fusion+"
        .with_injected_tool(f.tool_json());            // {type:"openrouter:fusion", parameters:{…}}
    if let Some(tc) = f.tool_choice_value() { provider = provider.with_tool_choice(tc); }
}
```

- **`model_override`** — `stream()` replaces `req.model` with the configured outer model *before* `strip_wire_prefix` runs (so `openrouter/openai/gpt-4.1` → `openai/gpt-4.1`). The user-facing model id stays `openrouter/fusion+`.
- **`injected_tools`** — appended to the request `tools` array after the agent's own function tools (so Fusion coexists with Bash/Edit/etc. under `tool_choice: auto`). `FusionConfig::tool_json()` emits `{"type":"openrouter:fusion","parameters":{…snake_case…}}`, omitting any unset field so empty config ⇒ OpenRouter defaults.
- **`tool_choice`** — `FusionConfig::tool_choice_value()` returns `Some(json!("required"))` only for the `required` setting; `auto` is OpenRouter's default and is omitted.

`FusionConfig` (`config.rs`, `settings.json` key `openrouterFusion`, camelCase fields: `outerModel`, `analysisModels[1–8]`, `judgeModel`, `maxToolCalls`, `maxCompletionTokens`, `temperature`, `reasoning`, `toolChoice`) lives on both `AppConfig` (always present, defaulted) and `ProjectConfig` (optional overlay). IPC round-trips it via `fusion_config_get` / `fusion_config_set` (`ipc.rs`), which the GUI's fusion config modal opens when the user picks `openrouter/fusion+`. The catalogue entry is pinned in `refresh-model-catalogue.py` (`PROVIDERS["openrouter"]["pin"]`) so auto-prune doesn't delete the pseudo-id (it has no upstream `/v1/models` row).

### `DashScope`

```rust
let base = std::env::var("DASHSCOPE_BASE_URL")
    .unwrap_or_else(|_| "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string());
let url = if base.ends_with("/chat/completions") { base }
          else { format!("{}/chat/completions", base.trim_end_matches('/')) };
OpenAIProvider::new(api_key).with_base_url(url)
```

Alibaba Qwen via OpenAI-compatible mode. **No prefix strip** — `qwen-max` is the bare id. Routing: `qwen` / `qwq-`.

### `ZAi`

```rust
OpenAIProvider::new(api_key)
    .with_base_url($ZAI_BASE_URL or "https://api.z.ai/api/coding/paas/v4")
    .with_strip_model_prefix("zai/")
```

Z.ai GLM Coding Plan. Models look like `zai/glm-4.6`. Power users with the general BigModel SKU (open.bigmodel.cn/api/paas/v4) can override via `ZAI_BASE_URL`.

### `LMStudio` — local, no auth

```rust
let base = std::env::var("LMSTUDIO_BASE_URL")
    .unwrap_or_else(|_| "http://localhost:1234/v1".to_string());
OpenAIProvider::new("lm-studio".to_string())     // dummy bearer
    .with_base_url(...)
    .with_strip_model_prefix("lmstudio/")
```

Built BEFORE the `api_key_from_env()` lookup in `build_provider`'s Stage B, so no real key is required. Bearer value is the literal `"lm-studio"` — LMStudio ignores Authorization but the OpenAI client always sends one.

### `OpenAICompat` — generic

```rust
OpenAIProvider::new(api_key)
    .with_base_url($OPENAI_COMPAT_BASE_URL or "http://localhost:8000/v1")
    .with_strip_model_prefix("oai/")
```

For SML Gateway, Portkey, Helicone, internal corporate proxies. Models look like `oai/gpt-4o-mini` (or any upstream model id the user picks). Auth via `OPENAI_COMPAT_API_KEY`. vLLM, llama.cpp and LiteLLM have their own kinds and no longer share this slot.

### `LiteLlm`

```rust
OpenAIProvider::new(api_key or "local-no-auth")
    .with_base_url($LITELLM_BASE_URL or "http://localhost:4000/v1")
    .with_strip_model_prefix("litellm/")
```

Self-hosted LiteLLM proxy. Built BEFORE `build_provider`'s Stage B key lookup, so `LITELLM_API_KEY` is optional (a proxy without `master_key` accepts any bearer). Models are the operator's `model_name` aliases, so ids may nest (`litellm/azure/prod-gpt4` → wire `azure/prod-gpt4`) and none of them are catalogued: `/models` unions the live `GET /models`, and a `/model` switch runs `probe_compat_limits(ProviderKind::LiteLlm, …)`, which reads `/model/info` (retried at the root when the base URL ends in `/v1`, since LiteLLM mounts admin routes there) before falling back to `/models`.

Not in `is_local()` nor `gateway::is_local_provider()` — it routes to cloud upstreams, so PII masking and org-policy gating stay on.

### `DeepSeek`

```rust
OpenAIProvider::new(api_key)
    .with_base_url($DEEPSEEK_BASE_URL or "https://api.deepseek.com/v1")
```

DeepSeek's hosted endpoint. Bare model ids (`deepseek-chat`, `deepseek-reasoner`, `deepseek-v4-flash`, `deepseek-v4-pro`) — no prefix to strip. `deepseek-reasoner` and `deepseek-v4-*` match `model_uses_reasoning_content` so reasoning_content round-trips.

### `ThaiLLM`

```rust
OpenAIProvider::new(api_key)
    .with_base_url($THAILLM_BASE_URL or "http://thaillm.or.th/api/v1")
    .with_strip_model_prefix("thaillm/")
```

NSTDA / สวทช Thai LLM aggregator. Hosts OpenThaiGPT, Typhoon-S, Pathumma, THaLLE. Models look like `thaillm/OpenThaiGPT-ThaiLLM-8B-Instruct-v7.2`.

---

## 8. Testing

`openai::tests` — extensive coverage of the parser (per-event cases) and end-to-end mock streams.

**Parser:**
- `parse_text_chunk_emits_message_start_and_text_delta` — basic happy path
- `final_empty_choices_chunk_emits_usage_stop` — DashScope/OpenAI `stream_options.include_usage` trailing frame
- `parse_tool_call_streams_and_flushes_stop_on_finish` — partial_json across 3 chunks, finish_reason synthesizes ContentBlockStop
- Multiple parallel tool calls (index switching synthesizes ContentBlockStop)
- Upstream error JSON inside `data:` frame surfaces as `Error::Provider`
- `[DONE]` is ignored

**Body construction:**
- `messages_to_openai` splits ToolResult into separate `tool` role messages
- Multiple parallel tool calls → all tool messages back-to-back, then ONE combined image message
- Inline user images → `content: [{type: text}, {type: image_url}]` array form
- System prompt prepended as `messages[0]`
- Empty system / tools omitted

**End-to-end (wiremock):**
- Full text turn replay
- Tool call with multi-chunk arguments combines correctly via `assemble`
- HTTP 401 propagates as `Error::Provider("http 401: ...")` with key redacted

**Reasoning:**
- `model_uses_reasoning_content` allowlist matches expected patterns, rejects others

---

## 9. Notable behaviors / gotchas

- **`max_completion_tokens` not `max_tokens`.** The newer field name; required for gpt-5+ and o-series. Older OpenAI-compat aggregators may need to translate. If you see "unknown field max_completion_tokens" from a provider, that provider is too old to be in the OpenAI-compat allowlist.
- **`stream_options.include_usage: true` is unconditional.** Sent on every request. Upstream providers that don't recognize it should ignore it; if one rejects (rare), surface as a provider-specific bug.
- **Tool call `arguments` are pre-stringified JSON** at request time (`serde_json::to_string(input)`). At response time, deltas concatenate into a buffer and the assembler parses to JSON at `ContentBlockStop`. Same flow as Anthropic's `partial_json`.
- **`role: "tool"` messages must have `tool_call_id` matching a prior assistant `tool_calls[].id`.** The combined-images-after-all-tools rule exists because of this contract.
- **Inline images are user-role.** ContentBlock::Image inside an assistant message would never be sent — assistant content goes through `text_parts` / `tool_calls` / `reasoning_text` paths but not `inline_user_images`. (Models don't emit images directly anyway.)
- **`reasoning_content` is dropped for non-allowlist models.** If you add a new thinking-capable model and forget to update `model_uses_reasoning_content`, the model's reasoning won't round-trip and the next turn will error 400 (server expects prior reasoning).
- **Implicit-thinking models** (Qwen3, DeepSeek-R1) that stream raw chain-of-thought as `text_delta` are handled by the assembler's `<think>` tag splitter, NOT by `reasoning_content`. That's an assembler concern; this provider just emits `TextDelta` and lets `assemble` route the bracketed content to `Thinking`.
- **No request retry inside the provider.** Same as Anthropic — agent loop owns retry.
- **No prompt caching.** OpenAI Chat Completions doesn't support cache_control; tokens are billed in full every turn. If your cost matters, prefer Anthropic for cacheable-prefix workloads.

---

## 10. What's NOT supported

- **No `tool_choice` field** — model decides when to call tools.
- **No `response_format` / structured outputs** — every response is unstructured text/tool-calls.
- **No batch API** (`/v1/batches`).
- **No image generation** — `gpt-image-*` and DALL-E endpoints aren't routed here.
- **No `developer` role** (the newer GPT-4.1+ system-message variant). System messages stay `role: "system"`.
- **No Azure direct support** — Azure-hosted OpenAI uses a different URL shape (`/openai/deployments/{id}/chat/completions?api-version=...`) and `api-key:` header. Could be wired with `with_api_key_header("api-key")` + custom `with_base_url`, but isn't a pre-built variant. (`AzureAIFoundry` exposes Anthropic models, not OpenAI models — it routes through `AnthropicProvider`.)
