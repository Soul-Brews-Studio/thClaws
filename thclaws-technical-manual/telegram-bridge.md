# Telegram Bridge (dev-plan/29)

Telegram Bot API ↔ thClaws desktop adapter: a user chats with their thClaws session over Telegram, the agent runs on their local machine, and thClaws talks to `api.telegram.org` **directly** — no relay.

| Layer | Lives at | Role |
|---|---|---|
| Adapter engine | `crates/core/src/telegram/` | long-poll client + session routing + `TelegramApprover` + pairing + config + filter |
| GUI worker integration | `crates/core/src/shared_session.rs` `ShellInput::Telegram*` arms | drives `Agent::run_turn` per inbound message; mode swap to `TelegramGated` |
| GUI bootstrap | `crates/core/src/telegram/bootstrap.rs` (`#[cfg(feature = "gui")]`) | forwards messages into the worker via `ShellInput::TelegramMessage` |
| Headless loop | `crates/core/src/telegram/headless.rs` | standalone agent loop for `thclaws --telegram` (no GUI) |
| IPC | `crates/core/src/ipc.rs` `telegram_*` arms | connect / disconnect / status / pairing approve+reject |
| Frontend modal | `frontend/src/components/TelegramConnectModal.tsx` | paste token → connect; pending-pairing Approve/Reject list |
| Sidebar pill | `frontend/src/components/Sidebar.tsx` | `@botname` + chat/approval/pairing counts |

## Why no relay (vs LINE)

LINE only delivers via outbound HTTPS webhook, so a desktop client can't receive without a server to host the webhook → `crates/line-server/` is unavoidable. Telegram exposes `getUpdates` long-polling, which works behind NAT, so the desktop connects straight to `api.telegram.org`. Webhook mode (for users running `thclaws --serve` on a public host) is deferred to Tier 3. There is **no `telegram-server` crate** and no third party in the message path.

## Module layout

```
crates/core/src/telegram/
├── mod.rs        public exports; feature-gate notes
├── protocol.rs   Bot API wire types (Update/Message/Chat/User/CallbackQuery/InlineKeyboard*) + outbound bodies
├── config.rs     TelegramConfig + ~/.config/thclaws/telegram.json; token resolution + validation; DM/group policy
├── client.rs     long-poll getUpdates loop; sendMessage / answerCallbackQuery / editMessageText; backoff + stall guard
├── filter.rs     HTML-escape + chunk (chunk-then-escape); reuses line::filter::clean_for_stream
├── session.rs    ChatRegistry (per-chat state + 24h GC) + SessionSink (auth → pairing → approver → handler)
├── approver.rs   TelegramApprover: inline-keyboard approval, callback_data state machine
├── pairing.rs    PairingManager: 6-digit code mint + 1h expiry (in-memory)
├── bootstrap.rs  GUI worker wiring (#[cfg(feature = "gui")])
└── headless.rs   standalone agent loop for --telegram
```

## Wire protocol

All calls hit `{api_base}/bot{token}/{method}` (`TelegramClient::method_url`). `api_base` defaults to `https://api.telegram.org`, overridable with `THCLAWS_TELEGRAM_API` (dev / mock server). Every method returns the generic envelope `ApiResponse<T> { ok, result?, description?, error_code? }` (`protocol.rs`); `api_result()` (`client.rs`) unwraps it, mapping `error_code == 401` to `TelegramClientError::Unauthorized` (fatal — bad token).

### `getUpdates` (long-poll)

`TelegramClient::run` (`client.rs`) is the loop:

1. **Backlog drain** — one `getUpdates(offset=0, timeout=0)` whose results are discarded; the offset advances past them. So messages sent *before* launch don't replay. Logged at startup.
2. **Poll** — `getUpdates(offset, timeout=LONG_POLL_TIMEOUT_SECS=45)` in a `tokio::select!` against the cancel token. The per-request HTTP timeout is `REQUEST_TIMEOUT_SECS = 55` (`= 45 + 10`), set *above* the server hold so a normal empty long-poll doesn't trip it, but a hung connection does → transport error → reconnect. **This is the stall guard** (dev-plan/29 Risk #3); no separate watchdog.
3. **Offset cursor** — `next_offset(current, &updates)` returns `max(update_id)+1`, never regressing below `current` (`client.rs`). Telegram acks all updates `< offset` on the next call.
4. **Backoff** — exponential 1s→60s on transport error; reset to 1s on a clean poll. `Unauthorized` stops the loop (surfaced so the caller disconnects).

### Outbound

| Method | Body (`protocol.rs`) | Used by |
|---|---|---|
| `sendMessage` | `SendMessage { chat_id, text, parse_mode: "HTML", reply_markup?, message_thread_id? }` | replies, pairing prompts, approval prompts |
| `answerCallbackQuery` | `AnswerCallbackQuery { callback_query_id, text?, show_alert? }` | stop the button spinner + toast the verdict |
| `editMessageText` | `EditMessageText { chat_id, message_id, text, parse_mode, reply_markup? }` | rewrite the approval prompt in place after a tap |
| `getMe` | — | token validation + `@username` for the status pill |

### Inbound `Update`

`Update` (`protocol.rs`) carries `message` / `edited_message` / `callback_query` / `channel_post`, all `Option` + `#[serde(default)]` so an unknown update type (poll, shipping_query, …) deserialises without erroring and the loop advances. `incoming_message()` collapses `message`/`edited_message` (Tier 1 treats edits as fresh text). `Chat::kind` is a `ChatKind` enum (`private`/`group`/`supergroup`/`channel`). `channel_post` is deserialised but unhandled in Tier 1.

## Config & token resolution

`TelegramConfig` (`config.rs`) is the canonical struct, embedded both in `ProjectConfig.telegram` (`config.rs`, `.thclaws/settings.json`) and persisted standalone at `~/.config/thclaws/telegram.json` (atomic write via `.tmp` + rename). Fields: `enabled`, `bot_token?`, `dm_policy` (`DmPolicy::{Pairing,Allowlist}`, default Pairing), `allow_from: Vec<String>`, `group_policy` (`GroupPolicy::{Allowlist,Open}`, default Allowlist), `groups`, `channels` (Tier 2), `output_ceiling` (default 4000). camelCase on the wire.

**Token precedence — `resolved_token()`:** `TELEGRAM_BOT_TOKEN` env (non-empty) → file `bot_token` → `None`. Env-wins is deliberate (12-factor; dev-plan/29 acceptance test #8) — note the plan *prose* said file-first, but the test and `resolved_token` are env-first. `validate_token()` checks the `<digits>:<secret≥20 token-chars>` shape (lenient — catches a pasted username / empty, not a typo inside a well-formed token). `redact_token()` keeps the bot id, masks the secret.

Authorization helpers: `allows_dm(user_id)` (membership in `allow_from`), `allows_group(chat_id)` (policy-aware), `add_allowed_user(user_id)` (idempotent append, used by pairing approve).

## Session routing

`ChatRegistry` (`session.rs`): `chat_id → ChatState { kind, last_active: Instant, message_count }` with lazy 24h idle GC (`IDLE_GC`) on every `touch`/`active_count`. Drives the status pill counts. **Tier 1 has no per-chat agent isolation** — all authorized chats forward into the single shared session; pairing gates *who* reaches it, so a deployment is effectively single-owner. Per-chat/forum-topic routing is Tier 2.

`SessionSink::on_update` dispatch:
- **callback_query** → `handle_callback`: resolve the pending approval synchronously (unblocks the waiting turn), then *spawn* `answerCallbackQuery` + `editMessageText` (using the `CallbackQuery.message` payload — no stored message id needed) so `on_update` returns promptly.
- **message** → `handle_message`: `registry.touch`; text-only (non-text ignored in Tier 1). Private: `allows_dm` → serve; else `Pairing` → mint code + DM; `Allowlist` → silent ignore. Group/Supergroup: `allows_group` → serve. Channel: ignored. Authorized text either resolves a pending approval via free-text fallback or `spawn_turn` (sets approver `active_chat`, spawns `handler.handle_message`, chunks the reply via `format_for_telegram`).

`TelegramMessageHandler` is the pluggable turn-runner: `WorkerForwardHandler` (GUI, `bootstrap.rs`) forwards to the worker; `HeadlessAgentHandler` (`headless.rs`) drives an in-process agent.

## Pairing

`PairingManager` (`pairing.rs`): in-memory `code → PendingPair { code, user_id, chat_id, display, minted_at }`. `mint` is idempotent per user (reuses a live code so repeated messages don't flood the GUI), GCs expired entries, generates a 6-digit code via `getrandom` (CSPRNG — a pairing code is a shared secret). `approve(code)` / `reject(code)` remove + return the entry; `PAIRING_EXPIRY = 1h`. `is_expired()` is a pure fn (testable without sleeping; clock-skew-safe). **Pending codes don't survive restart** (Tier 1; dev-plan/29 Risk #2). GUI approval: `ShellInput::TelegramPairingApprove` → `pairing.approve` → `config.add_allowed_user` → `cfg.save()` → DM the user → re-broadcast status. Headless can't GUI-approve → `TELEGRAM_OWNER_ID` auto-allowlists at startup.

## Approver

`TelegramApprover` (`approver.rs`) is the Telegram analogue of `LineApprover` (the plan lifts both into a shared `adapter::approver` once Discord arrives). On `ApprovalSink::approve`:

1. mint a `request_id`, register a `oneshot::Sender` in `Pending { by_id, order }`,
2. `sendMessage` to `active_chat` with an `InlineKeyboardMarkup` of three buttons carrying `callback_data = tool:{verb}:{request_id}` (verbs `allow`/`always`/`deny`),
3. await the tap with `tokio::time::timeout(DEFAULT_TIMEOUT = 60s)`; auto-deny + notice on timeout.

`active_chat` is set by the session sink before each turn (`set_active_chat`) — last-writer-wins, correct for single-owner Tier 1. Resolution: `record_decision_from_callback(data)` parses `tool:<verb>:<req_id>`, resolves the matching oneshot, returns the verdict so the sink edits the prompt; `record_decision_from_text` is the typed-`approve`/`deny` fallback. `ApprovalReply::{Allow,AllowAlways,Deny}` map to `ApprovalDecision::{Allow,AllowForSession,Deny}`. The prompt HTML-escapes tool name + input preview (else a tool input containing `<>&` 400s the prompt).

## Permission posture

A new mode `PermissionMode::TelegramGated` (`permissions.rs`) — parallel to `LineGated`, both opt into `asks_for_approval()`. The plan generalises them to `BotGated` in Tier 2. On connect the worker stashes the agent's pre-connect mode + approver, sets the global mode + agent mode to `TelegramGated`, and swaps `state.approver` to the `TelegramApprover` (the agent loop consults `current_mode()` per gate, so the global must be set, not just `agent.permission_mode`). On disconnect it restores both. Same C3 fix the LINE path documents (restore on the agent's mode, not just the global).

## GUI integration

`shared_session.rs` (all `#[cfg(feature = "gui")]`):
- `ShellInput::{TelegramConnect(cfg), TelegramDisconnect, TelegramMessage{text,respond}, TelegramPairingApprove{code}, TelegramPairingReject{code}, TelegramStatusRequest}`.
- `ViewEvent::TelegramStatus(String)` — pre-built JSON `{ type, state, bot_username, pending_approvals, pending_pairings, active_chats, pairings: [...] }`; `event_render.rs` passes it through to the chat frontend.
- **Connect arm**: `resolved_token()` → `getMe` validate (errors broadcast a disconnected status with `error`) → cancel any prior session → `bootstrap::spawn` → mode swap → `rebuild_agent`.
- **Boot auto-reconnect**: on worker start, if `telegram.json` loads with `enabled && resolved_token().is_some()`, send `TelegramConnect` (mirrors the LINE block).
- `telegram_status_payload(handle)` builds the live snapshot from the in-memory approver/pairing/registry; pending pairings live in the polling task (not the worker loop), so the GUI **polls** `telegram_status` (modal `setInterval` 3s) → `TelegramStatusRequest` → worker re-broadcasts.

`bootstrap::spawn` builds the shared `Arc<TelegramClient>` (one client for poller + approver + sink), `TelegramApprover`, `PairingManager`, `Arc<Mutex<TelegramConfig>>`, and a `TelegramSession`, returning a `TelegramSessionHandle { cancel, status, join, approver, client, pairing, config, registry }`.

IPC (`ipc.rs`): `telegram_status` (forward `TelegramStatusRequest`), `telegram_connect` (validate token shape → merge onto existing config → `cfg.save()` → `TelegramConnect`), `telegram_disconnect`, `telegram_pairing_approve`/`_reject`.

## Headless integration

`telegram::headless::run(AppConfig)` (`headless.rs`, **not** gui-gated — the GUI worker is) builds its own agent the way `repl::run_print_mode` does: `ProjectContext` system prompt + memory + KMS section, `ToolRegistry::with_builtins` + KMS/memory tools, `repl::build_provider`. Then it sets the agent to `TelegramGated` + `TelegramApprover`, sets the global mode, and runs the session. `HeadlessAgentHandler` holds an `Arc<Agent>` and serialises turns with a `tokio::Mutex` turn-lock (the agent's history is shared mutable state). `TELEGRAM_OWNER_ID` (numeric) is auto-added to `allow_from` at startup. Ctrl-C → cancel → clean exit (a `Cancelled` result is success, not error). Tier 1 headless limits: no MCP servers, no session persistence (history is in-memory for the process), single shared session.

Wired in `bin/app.rs`: `--telegram` flag joins `use_cli` (skips GUI dispatch) and routes to `headless::run` before the print/REPL branch. The `telegram` subcommand (`TelegramCmd::{Status, Pair}`) prints config / setup help.

## Output filter

`format_for_telegram(body, ceiling)` (`filter.rs`): `clean_for_stream` (ANSI + tool-narration strip, shared with LINE) → trim → `chunk_plain` (split on line boundaries ≤ ceiling, hard-split an oversized line on a char boundary) → `render_html` per chunk. **Chunk-then-escape** is deliberate: escaping the plain text *after* chunking means an HTML entity never spans a chunk boundary (`&amp;` can't be cut into `&am`+`p;`), and a fenced code block split across chunks fails the balance check (`split("```")` segment-count parity) and falls back to plain escaping rather than emitting a dangling `<pre>`. HTML mode escapes only `< > &` (vs MarkdownV2's ~18-char table). `TELEGRAM_MAX_CHARS = 4096` is the hard cap; `output_ceiling` (4000) the configurable target.

## Tier roadmap

- **Tier 1 (this doc):** DM + basic group + plain text + pairing + inline-keyboard approvals; GUI + headless.
- **Tier 2:** broadcast channels + linked discussion groups + forum-topic per-agent routing (`channel.rs`, `topic.rs`); `channel_post` handler; generalise `LineGated`/`TelegramGated` → `BotGated`; lift the shared `adapter::{Outbound, Inbound}` traits.
- **Tier 3:** streaming preview edits, media up/download, voice transcription, sticker vision, webhook mode (mounts on `--serve`), multi-account, proxy, keychain token storage, `telegram doctor`.

## Test surface

~59 unit tests across the module (`cargo test --lib telegram::`): protocol round-trips (incl. unknown-update tolerance + camelCase config), token precedence + validation, filter (escape correctness, chunk boundaries, fenced-block + UTF-8 safety, unbalanced-fence fallback), client URL/offset/`api_result` helpers, pairing mint/approve/expiry, approver callback/text/timeout/concurrent-resolve, registry counts + GC. The poll→turn→reply loop and the inline-keyboard tap against a *real* bot token are not in the automated suite (need a provisioned bot); the transport is verified by a live `getMe`→401 round-trip.

## Workspace-only

Nothing here is workspace-only — unlike `line-server`, the whole adapter ships in the public crate (no relay to keep private).
