//! Forum-topic routing (dev-plan/29 Tier 2).
//!
//! Supergroups with "Topics" on split messages into threads identified
//! by `message_thread_id`. A channel's linked discussion group can be a
//! forum, so a comment on a channel post lands in a specific topic. This
//! module is the **pure** routing layer:
//!
//! - [`effective_topic_id`] — collapse Telegram's `message_thread_id` +
//!   `is_topic_message` into one optional topic id (the "General" topic
//!   has no `message_thread_id` but is still a topic).
//! - [`resolve_agent`] — map a `(chat_id, topic)` to the configured
//!   `agentId` (per-topic override → channel default → none).
//! - [`thread_id_for_send`] — the General-topic send quirk: Telegram
//!   **rejects** `message_thread_id = 1`, so posting to General must omit
//!   the field.
//! - [`session_key`] — per-topic session isolation key so two topics in
//!   the same group keep separate conversation context.

use super::config::TelegramConfig;

/// Telegram's reserved thread id for the "General" forum topic. Inbound
/// General-topic messages carry no `message_thread_id`; outbound sends to
/// General must omit it (passing `1` is an API error).
pub const GENERAL_TOPIC_ID: i64 = 1;

/// Collapse `message_thread_id` + `is_topic_message` into a single topic
/// id. `None` means "not a forum topic" (a plain group/DM message).
pub fn effective_topic_id(message_thread_id: Option<i64>, is_topic_message: bool) -> Option<i64> {
    match message_thread_id {
        Some(tid) => Some(tid),
        // No thread id but flagged as a topic message ⇒ the General topic.
        None if is_topic_message => Some(GENERAL_TOPIC_ID),
        None => None,
    }
}

/// Resolve the agent that should handle a message in `chat_id` / `topic`.
/// `chat_id` may be a configured channel **or** its linked discussion
/// group (comments route the same way). Precedence: per-topic override →
/// channel default `agentId` → `None` (caller uses the main agent).
pub fn resolve_agent(config: &TelegramConfig, chat_id: i64, topic: Option<i64>) -> Option<String> {
    let channel = config
        .channel(chat_id)
        .or_else(|| config.channel_for_discussion_group(chat_id))?;
    if let Some(tid) = topic {
        if let Some(route) = channel.topic_routing.get(&tid.to_string()) {
            if let Some(agent) = route.agent_id.as_ref() {
                return Some(agent.clone());
            }
        }
    }
    channel.agent_id.clone()
}

/// The `message_thread_id` to put on an outbound send. Topic id `1`
/// (General) maps to `None` because Telegram rejects an explicit
/// `message_thread_id = 1`; everything else passes through.
pub fn thread_id_for_send(topic: Option<i64>) -> Option<i64> {
    match topic {
        Some(GENERAL_TOPIC_ID) => None,
        other => other,
    }
}

/// Per-topic session-isolation key. Topics in the same group get
/// distinct keys so their conversation context doesn't bleed together.
pub fn session_key(chat_id: i64, topic: Option<i64>) -> String {
    match topic {
        Some(tid) => format!("{chat_id}:t{tid}"),
        None => chat_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::config::{TelegramChannelConfig, TelegramConfig, TopicRoute};
    use std::collections::HashMap;

    fn cfg_with_channel() -> TelegramConfig {
        // Channel -100123 with linked discussion group -100987, default
        // agent "research", topic 1 → research, topic 42 → coder.
        let mut topic_routing = HashMap::new();
        topic_routing.insert(
            "1".to_string(),
            TopicRoute {
                agent_id: Some("research".into()),
            },
        );
        topic_routing.insert(
            "42".to_string(),
            TopicRoute {
                agent_id: Some("coder".into()),
            },
        );
        let mut channels = HashMap::new();
        channels.insert(
            "-100123".to_string(),
            TelegramChannelConfig {
                linked_discussion_group: Some("-100987".into()),
                agent_id: Some("research".into()),
                topic_routing,
            },
        );
        TelegramConfig {
            channels,
            ..Default::default()
        }
    }

    #[test]
    fn effective_topic_id_rules() {
        assert_eq!(effective_topic_id(Some(42), false), Some(42));
        assert_eq!(effective_topic_id(Some(42), true), Some(42));
        // General topic: no thread id but flagged as topic message.
        assert_eq!(effective_topic_id(None, true), Some(GENERAL_TOPIC_ID));
        // Plain group message: neither.
        assert_eq!(effective_topic_id(None, false), None);
    }

    #[test]
    fn resolve_agent_topic_overrides_then_default() {
        let c = cfg_with_channel();
        // Acceptance #3: topic 1 → research, topic 42 → coder.
        assert_eq!(
            resolve_agent(&c, -100123, Some(1)).as_deref(),
            Some("research")
        );
        assert_eq!(
            resolve_agent(&c, -100123, Some(42)).as_deref(),
            Some("coder")
        );
        // Unconfigured topic falls back to the channel default agent.
        assert_eq!(
            resolve_agent(&c, -100123, Some(99)).as_deref(),
            Some("research")
        );
        // No topic ⇒ channel default.
        assert_eq!(
            resolve_agent(&c, -100123, None).as_deref(),
            Some("research")
        );
    }

    #[test]
    fn resolve_agent_via_linked_discussion_group() {
        let c = cfg_with_channel();
        // A comment in the linked discussion group routes by the channel's
        // config just like the channel itself.
        assert_eq!(
            resolve_agent(&c, -100987, Some(42)).as_deref(),
            Some("coder")
        );
        assert_eq!(
            resolve_agent(&c, -100987, Some(1)).as_deref(),
            Some("research")
        );
    }

    #[test]
    fn resolve_agent_unknown_chat_is_none() {
        let c = cfg_with_channel();
        assert_eq!(resolve_agent(&c, -999, Some(42)), None);
    }

    #[test]
    fn thread_id_for_send_omits_general() {
        // Acceptance #4: threadId=1 (General) must be omitted on send.
        assert_eq!(thread_id_for_send(Some(GENERAL_TOPIC_ID)), None);
        assert_eq!(thread_id_for_send(Some(42)), Some(42));
        assert_eq!(thread_id_for_send(None), None);
    }

    #[test]
    fn session_keys_are_per_topic() {
        assert_eq!(session_key(-100987, Some(42)), "-100987:t42");
        assert_eq!(session_key(-100987, Some(1)), "-100987:t1");
        assert_eq!(session_key(-100987, None), "-100987");
        assert_ne!(
            session_key(-100987, Some(42)),
            session_key(-100987, Some(1))
        );
    }
}
