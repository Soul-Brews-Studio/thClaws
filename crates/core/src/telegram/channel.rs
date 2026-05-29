//! Broadcast-channel + linked-discussion-group plumbing (dev-plan/29
//! Tier 2).
//!
//! The Telegram channel pattern: the agent posts status to a **channel**
//! (`sendMessage(chat_id = channel_id)`), and comments on those posts
//! arrive in the channel's **linked discussion group** as ordinary
//! `message` updates the session already handles. This module covers the
//! two channel-specific needs:
//!
//! - [`probe_admin`] — before relying on a channel, confirm the bot is an
//!   admin that can post, returning a *clear* error instead of letting a
//!   later `sendMessage` 403 silently (Tier 2 acceptance: "channel-
//!   without-admin probe returns clear error, not silent fail").
//! - [`build_topic_messages`] / [`send_to_topic`] — chunked outbound that
//!   targets a forum topic, applying the General-topic send quirk via
//!   [`super::topic::thread_id_for_send`].

use super::client::{TelegramClient, TelegramClientError};
use super::filter::format_for_telegram;
use super::protocol::{ChatMember, SendMessage};
use super::topic::thread_id_for_send;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("bot is not an admin of chat {chat_id} (membership: {status}) — add it as an admin with post rights")]
    NotAdmin { chat_id: i64, status: String },
    #[error("bot is an admin of chat {chat_id} but lacks the 'post messages' right")]
    CannotPost { chat_id: i64 },
    #[error(transparent)]
    Api(#[from] TelegramClientError),
}

/// Interpret a `getChatMember` result for `chat_id` as "can the bot post
/// here?". Pure so the status→error mapping is unit-testable without a
/// live API.
pub fn classify_membership(member: &ChatMember, chat_id: i64) -> Result<(), ChannelError> {
    match member.status.as_str() {
        "creator" => Ok(()),
        "administrator" => {
            if member.can_post_messages.unwrap_or(false) {
                Ok(())
            } else {
                Err(ChannelError::CannotPost { chat_id })
            }
        }
        other => Err(ChannelError::NotAdmin {
            chat_id,
            status: other.to_string(),
        }),
    }
}

/// Confirm the bot can post to `channel_id`. `bot_user_id` is the bot's
/// own id (from `getMe`). Returns a clear [`ChannelError`] on failure.
pub async fn probe_admin(
    client: &TelegramClient,
    channel_id: i64,
    bot_user_id: i64,
) -> Result<(), ChannelError> {
    let member = client.get_chat_member(channel_id, bot_user_id).await?;
    classify_membership(&member, channel_id)
}

/// Build the chunked outbound messages for a reply to `chat_id` within
/// `topic`. Pure: applies the General-topic quirk (topic `1` ⇒ no
/// `message_thread_id`) and HTML-escapes + chunks the body.
pub fn build_topic_messages(
    chat_id: i64,
    topic: Option<i64>,
    body: &str,
    ceiling: u32,
) -> Vec<SendMessage> {
    let thread = thread_id_for_send(topic);
    format_for_telegram(body, ceiling)
        .into_iter()
        .map(|chunk| {
            let mut m = SendMessage::text(chat_id, chunk);
            m.message_thread_id = thread;
            m
        })
        .collect()
}

/// Send a (possibly chunked) reply to `chat_id` within `topic`.
pub async fn send_to_topic(
    client: &TelegramClient,
    chat_id: i64,
    topic: Option<i64>,
    body: &str,
    ceiling: u32,
) -> Result<(), TelegramClientError> {
    for msg in build_topic_messages(chat_id, topic, body, ceiling) {
        client.send_message(&msg).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::topic::GENERAL_TOPIC_ID;
    use super::*;

    fn member(status: &str, can_post: Option<bool>) -> ChatMember {
        ChatMember {
            status: status.into(),
            can_post_messages: can_post,
            can_delete_messages: None,
        }
    }

    #[test]
    fn creator_can_always_post() {
        assert!(classify_membership(&member("creator", None), -100).is_ok());
    }

    #[test]
    fn admin_needs_post_right() {
        assert!(classify_membership(&member("administrator", Some(true)), -100).is_ok());
        assert!(matches!(
            classify_membership(&member("administrator", Some(false)), -100),
            Err(ChannelError::CannotPost { .. })
        ));
        // can_post_messages absent ⇒ treated as no.
        assert!(matches!(
            classify_membership(&member("administrator", None), -100),
            Err(ChannelError::CannotPost { .. })
        ));
    }

    #[test]
    fn non_admin_is_clear_error() {
        match classify_membership(&member("member", None), -100123) {
            Err(ChannelError::NotAdmin { chat_id, status }) => {
                assert_eq!(chat_id, -100123);
                assert_eq!(status, "member");
            }
            other => panic!("expected NotAdmin, got {other:?}"),
        }
        // left / kicked also map to NotAdmin.
        assert!(matches!(
            classify_membership(&member("left", None), -1),
            Err(ChannelError::NotAdmin { .. })
        ));
    }

    #[test]
    fn build_topic_messages_sets_thread_except_general() {
        // Regular topic → message_thread_id set.
        let msgs = build_topic_messages(-100987, Some(42), "hello", 4000);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message_thread_id, Some(42));
        assert_eq!(msgs[0].chat_id, -100987);

        // General topic → message_thread_id omitted (Tier 2 acceptance #5).
        let general = build_topic_messages(-100987, Some(GENERAL_TOPIC_ID), "hi", 4000);
        assert_eq!(general[0].message_thread_id, None);

        // No topic → none.
        let plain = build_topic_messages(-100987, None, "hi", 4000);
        assert_eq!(plain[0].message_thread_id, None);
    }

    #[test]
    fn build_topic_messages_chunks_long_body() {
        let body = "x".repeat(1200);
        let msgs = build_topic_messages(-100987, Some(7), &body, 500);
        assert!(msgs.len() > 1);
        // Every chunk keeps the same thread target.
        assert!(msgs.iter().all(|m| m.message_thread_id == Some(7)));
    }
}
