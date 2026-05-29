//! Telegram Bot API adapter — thClaws-side client (dev-plan/29).
//!
//! Architecture vs LINE: **no relay**. The LINE adapter needs a relay
//! (`line-server`) because LINE only delivers via outbound HTTPS webhook
//! to an endpoint we'd have to host. Telegram exposes `getUpdates`
//! long-polling, which works behind NAT, so a desktop client connects
//! directly to `api.telegram.org`. Webhook mode is Tier 3.
//!
//! Tier 1 scope (this milestone):
//! - [`client`]  — long-poll `getUpdates`, `sendMessage`,
//!   `answerCallbackQuery`, `editMessageText`; reconnect + stall guard.
//! - [`config`]  — `TelegramConfig` + on-disk runtime state; env-wins
//!   token resolution; DM/group policy.
//! - [`protocol`] — Bot API wire types (`Update` / `Message` / `Chat` /
//!   `CallbackQuery` / inline keyboard).
//! - [`filter`]  — HTML-escape + chunk; reuses the LINE ANSI / tool-
//!   narration stripper.
//! - [`session`] — chat registry (per-`chat_id` state + 24h idle GC) and
//!   the update-routing sink that drives the agent.
//! - [`approver`] — inline-keyboard tool-approval (callback-data state
//!   machine), the Telegram analogue of `LineApprover`.
//! - [`pairing`] — pairing-code mint + 1h expiry + GUI approve/reject.
//! - [`bootstrap`] — wires the session into the GUI worker
//!   (`shared_session`), `#[cfg(feature = "gui")]` like LINE's.

pub mod approver;
#[cfg(feature = "gui")]
pub mod bootstrap;
/// Broadcast-channel + linked-discussion-group plumbing (Tier 2).
pub mod channel;
pub mod client;
pub mod config;
pub mod filter;
/// Standalone agent loop for `thclaws --telegram` (no GUI feature). Not
/// gui-gated — it builds its own agent instead of the gui-only worker.
pub mod headless;
pub mod pairing;
pub mod protocol;
pub mod session;
/// Streaming preview edits — edit a message in place as the agent
/// generates, rate-limited (Tier 3.1).
pub mod stream;
/// Forum-topic routing: effective topic id, per-topic agent resolution,
/// the General-topic send quirk, per-topic session keys (Tier 2).
pub mod topic;

pub use approver::{ApprovalReply, TelegramApprover};
#[cfg(feature = "gui")]
pub use bootstrap::{TelegramSessionHandle, TelegramStatus};
pub use channel::ChannelError;
pub use client::{TelegramClient, TelegramClientError, TelegramUpdateSink};
pub use config::{DmPolicy, GroupPolicy, TelegramConfig, TelegramConfigError};
pub use config::{TelegramChannelConfig, TopicRoute};
pub use filter::{escape_html, format_for_telegram};
pub use pairing::{PairingManager, PendingPair};
pub use protocol::{CallbackQuery, Chat, ChatKind, ChatMember, Message, Update, User};
pub use session::{ChatRegistry, TelegramMessageHandler, TelegramSession};
pub use stream::{PreviewCoalescer, PreviewSink, TelegramPreview};
