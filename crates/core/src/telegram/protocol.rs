//! Wire shapes for the Telegram Bot API.
//!
//! Unlike LINE (where a relay normalises everything into a small
//! `WsEnvelope`), Telegram is direct: we long-poll `getUpdates` and
//! deserialise raw Bot API `Update` objects, then serialise outbound
//! `sendMessage` / `answerCallbackQuery` / `editMessageText` request
//! bodies. These structs mirror the subset of
//! <https://core.telegram.org/bots/api> that Tier 1 needs:
//!
//! - Inbound: `Update` → `Message` (text DMs / groups), `CallbackQuery`
//!   (inline-keyboard taps for tool approval).
//! - Outbound: `SendMessage`, `AnswerCallbackQuery`, `EditMessageText`,
//!   each wrapping an `InlineKeyboardMarkup` for the approval UX.
//!
//! Everything is `#[serde(default)]` / `Option` where the Bot API may
//! omit a field, so a forward-compatible update variant (channel_post
//! in Tier 2, polls, etc.) deserialises without erroring — unknown
//! top-level keys are ignored by serde and surface as `None`.

use serde::{Deserialize, Serialize};

/// Generic Bot API envelope. Every method returns `{ "ok": bool,
/// "result": T }` on success or `{ "ok": false, "error_code": N,
/// "description": "…" }` on failure.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    // `Option` fields default to `None` when absent without an explicit
    // `#[serde(default)]` — and adding it here would force a spurious
    // `T: Default` bound on every `ApiResponse<T>`.
    pub result: Option<T>,
    pub description: Option<String>,
    pub error_code: Option<i64>,
}

/// One polled update. Exactly one of the optional payload fields is
/// populated per update; `update_id` is monotonically increasing and
/// drives the long-poll `offset` cursor (ack = `offset = max_seen + 1`).
#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
    /// User edited a prior message. Tier 1 treats edits as fresh
    /// inbound text (no diffing) — kept separate so a future tier can
    /// special-case them.
    #[serde(default)]
    pub edited_message: Option<Message>,
    /// Inline-keyboard tap. Tier 1's tool-approval buttons land here.
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
    /// Broadcast-channel post. Tier 2 wires a handler; Tier 1 just
    /// deserialises the field so the polling loop doesn't choke on a
    /// channel where the bot is an admin.
    #[serde(default)]
    pub channel_post: Option<Message>,
}

impl Update {
    /// The inbound message this update carries, if any — collapses the
    /// `message` / `edited_message` split since Tier 1 handles both the
    /// same way.
    pub fn incoming_message(&self) -> Option<&Message> {
        self.message.as_ref().or(self.edited_message.as_ref())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<User>,
    pub chat: Chat,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub text: Option<String>,
    /// Present on forum-topic messages (supergroups with topics on).
    /// Tier 2 routes per-topic on this.
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    /// Set on supergroup messages that belong to a forum topic (Tier 2).
    /// A message in the "General" topic has no `message_thread_id` but is
    /// still a topic message — this disambiguates.
    #[serde(default)]
    pub is_topic_message: Option<bool>,
    /// The chat on whose behalf the message was sent. For a comment on a
    /// channel post (auto-forwarded into the linked discussion group),
    /// `sender_chat` is the channel — lets us recognise the channel's
    /// own forwarded posts vs. real user replies (Tier 2).
    #[serde(default)]
    pub sender_chat: Option<Chat>,
    // ── forum-topic service messages (Tier 2) ──
    #[serde(default)]
    pub forum_topic_created: Option<ForumTopicCreated>,
    #[serde(default)]
    pub forum_topic_edited: Option<ForumTopicEdited>,
    #[serde(default)]
    pub forum_topic_closed: Option<ForumTopicClosed>,
    #[serde(default)]
    pub forum_topic_reopened: Option<ForumTopicReopened>,
}

/// Which forum-topic lifecycle service message a [`Message`] carries, if
/// any. Tier 2 uses this to keep an internal topic registry in sync
/// without the polling loop having to match each variant inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForumTopicEvent {
    Created,
    Edited,
    Closed,
    Reopened,
}

impl Message {
    /// True for any non-text service message (forum-topic lifecycle,
    /// etc.) that Tier 2 should account for but never feed to the agent.
    pub fn forum_topic_event(&self) -> Option<ForumTopicEvent> {
        if self.forum_topic_created.is_some() {
            Some(ForumTopicEvent::Created)
        } else if self.forum_topic_edited.is_some() {
            Some(ForumTopicEvent::Edited)
        } else if self.forum_topic_closed.is_some() {
            Some(ForumTopicEvent::Closed)
        } else if self.forum_topic_reopened.is_some() {
            Some(ForumTopicEvent::Reopened)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ForumTopicCreated {
    pub name: String,
    #[serde(default)]
    pub icon_color: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_custom_emoji_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForumTopicEdited {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon_custom_emoji_id: Option<String>,
}

/// Both `forum_topic_closed` and `_reopened` are empty objects on the
/// wire — we only need their presence, captured by the `Option` field.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForumTopicClosed {}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForumTopicReopened {}

/// Subset of `getChatMember`'s result — enough to answer "is the bot an
/// admin that can post here?" for the Tier 2 channel admin-rights probe.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatMember {
    /// `creator` | `administrator` | `member` | `restricted` | `left` |
    /// `kicked`.
    pub status: String,
    #[serde(default)]
    pub can_post_messages: Option<bool>,
    #[serde(default)]
    pub can_delete_messages: Option<bool>,
}

impl ChatMember {
    /// True when this member may post to a channel: the creator (all
    /// rights implied) or an administrator with `can_post_messages`.
    pub fn can_post_to_channel(&self) -> bool {
        match self.status.as_str() {
            "creator" => true,
            "administrator" => self.can_post_messages.unwrap_or(false),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub first_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_code: Option<String>,
}

impl User {
    /// Best-effort display label for logs / pairing prompts.
    pub fn display(&self) -> String {
        if let Some(u) = &self.username {
            return format!("@{u}");
        }
        match &self.last_name {
            Some(last) if !last.is_empty() => format!("{} {}", self.first_name, last),
            _ => self.first_name.clone(),
        }
    }
}

/// The four chat types Tier 1 may see. `Channel` is broadcast-only
/// (Tier 2 territory) but we still classify it so the session layer can
/// route by kind without re-parsing the raw string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: ChatKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
}

impl Chat {
    pub fn is_private(&self) -> bool {
        self.kind == ChatKind::Private
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    /// The message the inline keyboard was attached to. We need its
    /// `chat.id` + `message_id` to `editMessageText` after the tap.
    #[serde(default)]
    pub message: Option<Message>,
    /// The button's `callback_data` — Tier 1 shapes this as
    /// `tool:<verb>:<request_id>` for the approver (see
    /// [`super::approver`]).
    #[serde(default)]
    pub data: Option<String>,
}

// ───────────────────────── outbound ─────────────────────────

/// Telegram supports `HTML` and `MarkdownV2`. Tier 1 uses HTML: only
/// `< > &` need escaping (vs MarkdownV2's ~18-char escape table), so
/// the filter has a far smaller foot-gun surface.
pub const PARSE_MODE_HTML: &str = "HTML";

/// Body of `POST /sendMessage`.
#[derive(Debug, Clone, Serialize)]
pub struct SendMessage {
    pub chat_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
    /// Forum-topic target (Tier 2). `Some(1)` (the "general" topic) is
    /// special-cased by Telegram to mean "omit" — callers handle that;
    /// we serialise whatever's set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_thread_id: Option<i64>,
}

impl SendMessage {
    /// Plain HTML-parsed text with no keyboard.
    pub fn text(chat_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            text: text.into(),
            parse_mode: Some(PARSE_MODE_HTML.to_string()),
            reply_markup: None,
            message_thread_id: None,
        }
    }

    pub fn with_keyboard(mut self, kb: InlineKeyboardMarkup) -> Self {
        self.reply_markup = Some(kb);
        self
    }
}

/// Body of `POST /editMessageText`. Used to rewrite the approval prompt
/// in place after the user taps Allow / Deny so the buttons disappear
/// and the chosen verdict is shown.
#[derive(Debug, Clone, Serialize)]
pub struct EditMessageText {
    pub chat_id: i64,
    pub message_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
    /// Pass `Some(empty)` to strip the keyboard. We model "strip" as
    /// `None` here and rely on Telegram defaulting to no keyboard when
    /// the field is omitted on edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

impl EditMessageText {
    pub fn new(chat_id: i64, message_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            message_id,
            text: text.into(),
            parse_mode: Some(PARSE_MODE_HTML.to_string()),
            reply_markup: None,
        }
    }
}

/// Body of `POST /answerCallbackQuery`. Telegram shows a spinner on the
/// tapped button until this is sent; firing it immediately (before the
/// slower edit) keeps the UI responsive.
#[derive(Debug, Clone, Serialize)]
pub struct AnswerCallbackQuery {
    pub callback_query_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_alert: Option<bool>,
}

impl AnswerCallbackQuery {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            callback_query_id: id.into(),
            text: None,
            show_alert: None,
        }
    }

    pub fn with_toast(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            callback_query_id: id.into(),
            text: Some(text.into()),
            show_alert: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

impl InlineKeyboardMarkup {
    /// Single row of buttons — the only layout Tier 1 needs.
    pub fn one_row(buttons: Vec<InlineKeyboardButton>) -> Self {
        Self {
            inline_keyboard: vec![buttons],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    /// 1–64 bytes per the Bot API. Tier 1 keeps these short
    /// (`tool:allow:<uuid>` ≈ 47 bytes).
    pub callback_data: String,
}

impl InlineKeyboardButton {
    pub fn new(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: callback_data.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_with_text_message_decodes() {
        let json = r#"{
            "update_id": 42,
            "message": {
                "message_id": 7,
                "from": {"id": 111, "is_bot": false, "first_name": "Jimmy", "username": "mozeal"},
                "chat": {"id": 111, "type": "private", "first_name": "Jimmy"},
                "date": 1716500000,
                "text": "hi"
            }
        }"#;
        let u: Update = serde_json::from_str(json).unwrap();
        assert_eq!(u.update_id, 42);
        let m = u.incoming_message().expect("message");
        assert_eq!(m.text.as_deref(), Some("hi"));
        assert!(m.chat.is_private());
        assert_eq!(m.from.as_ref().unwrap().display(), "@mozeal");
    }

    #[test]
    fn edited_message_collapses_into_incoming() {
        let json = r#"{
            "update_id": 43,
            "edited_message": {
                "message_id": 8,
                "chat": {"id": 111, "type": "private"},
                "text": "fixed typo"
            }
        }"#;
        let u: Update = serde_json::from_str(json).unwrap();
        assert_eq!(
            u.incoming_message().unwrap().text.as_deref(),
            Some("fixed typo")
        );
    }

    #[test]
    fn callback_query_decodes_with_data_and_source_message() {
        let json = r#"{
            "update_id": 44,
            "callback_query": {
                "id": "cbq-1",
                "from": {"id": 111, "is_bot": false, "first_name": "Jimmy"},
                "message": {
                    "message_id": 9,
                    "chat": {"id": 111, "type": "private"},
                    "text": "approve?"
                },
                "data": "tool:allow:abc"
            }
        }"#;
        let u: Update = serde_json::from_str(json).unwrap();
        let cbq = u.callback_query.expect("callback_query");
        assert_eq!(cbq.data.as_deref(), Some("tool:allow:abc"));
        assert_eq!(cbq.message.unwrap().chat.id, 111);
    }

    #[test]
    fn unknown_update_kind_does_not_error() {
        // A poll / shipping_query / future update type — all the
        // payload fields are absent, but the update still parses so
        // the long-poll loop advances its offset instead of stalling.
        let json = r#"{"update_id": 45, "poll": {"id": "p1"}}"#;
        let u: Update = serde_json::from_str(json).unwrap();
        assert_eq!(u.update_id, 45);
        assert!(u.incoming_message().is_none());
        assert!(u.callback_query.is_none());
    }

    #[test]
    fn chat_kinds_round_trip() {
        for (s, k) in [
            ("private", ChatKind::Private),
            ("group", ChatKind::Group),
            ("supergroup", ChatKind::Supergroup),
            ("channel", ChatKind::Channel),
        ] {
            let json = format!(r#"{{"id": -100, "type": "{s}"}}"#);
            let c: Chat = serde_json::from_str(&json).unwrap();
            assert_eq!(c.kind, k);
        }
    }

    #[test]
    fn api_response_error_shape_decodes() {
        let json = r#"{"ok": false, "error_code": 403, "description": "Forbidden: bot was blocked by the user"}"#;
        let r: ApiResponse<Vec<Update>> = serde_json::from_str(json).unwrap();
        assert!(!r.ok);
        assert_eq!(r.error_code, Some(403));
        assert!(r.result.is_none());
        assert!(r.description.unwrap().contains("blocked"));
    }

    #[test]
    fn api_response_getme_decodes() {
        let json = r#"{"ok": true, "result": {"id": 999, "is_bot": true, "first_name": "thClaws Bot", "username": "thclaws_bot"}}"#;
        let r: ApiResponse<User> = serde_json::from_str(json).unwrap();
        assert!(r.ok);
        let me = r.result.unwrap();
        assert!(me.is_bot);
        assert_eq!(me.username.as_deref(), Some("thclaws_bot"));
    }

    #[test]
    fn send_message_serialises_with_html_parse_mode() {
        let m = SendMessage::text(111, "<b>hi</b>");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["chat_id"], 111);
        assert_eq!(v["parse_mode"], "HTML");
        // No keyboard / thread => omitted from the wire.
        assert!(v.get("reply_markup").is_none());
        assert!(v.get("message_thread_id").is_none());
    }

    #[test]
    fn send_message_with_keyboard_serialises_nested_rows() {
        let kb = InlineKeyboardMarkup::one_row(vec![
            InlineKeyboardButton::new("✅ Allow", "tool:allow:abc"),
            InlineKeyboardButton::new("🚫 Deny", "tool:deny:abc"),
        ]);
        let m = SendMessage::text(111, "approve?").with_keyboard(kb);
        let v = serde_json::to_value(&m).unwrap();
        let rows = &v["reply_markup"]["inline_keyboard"];
        assert!(rows.is_array());
        assert_eq!(rows[0][0]["text"], "✅ Allow");
        assert_eq!(rows[0][0]["callback_data"], "tool:allow:abc");
        assert_eq!(rows[0][1]["callback_data"], "tool:deny:abc");
    }

    #[test]
    fn answer_callback_query_omits_empty_fields() {
        let a = AnswerCallbackQuery::new("cbq-1");
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("callback_query_id"));
        assert!(!json.contains("text"));
        assert!(!json.contains("show_alert"));
    }
}
