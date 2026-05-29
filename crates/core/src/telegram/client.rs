//! Telegram Bot API client — long-poll `getUpdates`, send text, answer
//! callback queries, edit messages. Reconnect with exponential backoff.
//!
//! No relay (unlike LINE): `api.telegram.org` is publicly reachable and
//! `getUpdates` long-polling works behind NAT, so a desktop client
//! connects directly. The public entry point is
//! [`TelegramClient::run`], which loops forever — polls updates, hands
//! each to a [`TelegramUpdateSink`], and reconnects on transport error.
//! Cancellation is the standard [`crate::cancel::CancelToken`].
//!
//! Stall handling (Risk #3 in dev-plan/29): the long-poll request
//! timeout ([`REQUEST_TIMEOUT_SECS`]) is set just above the server-side
//! poll timeout ([`LONG_POLL_TIMEOUT_SECS`]), so a hung connection that
//! never returns trips the client timeout → transport error → reconnect,
//! instead of leaving the bot silently alive-but-deaf.

use std::time::Duration;

use super::config::redact_token;
use super::protocol::{
    AnswerCallbackQuery, ApiResponse, ChatMember, EditMessageText, Message, SendMessage, Update,
    User,
};

/// Server-side long-poll hold. Telegram holds the `getUpdates`
/// connection open this long waiting for an update before returning an
/// empty array.
pub const LONG_POLL_TIMEOUT_SECS: u64 = 45;

/// Per-request client timeout for `getUpdates`. Must exceed
/// `LONG_POLL_TIMEOUT_SECS` so a normal empty long-poll doesn't trip it,
/// but bounds a hung connection (the stall detector).
pub const REQUEST_TIMEOUT_SECS: u64 = LONG_POLL_TIMEOUT_SECS + 10;

/// Timeout for the quick send/answer/edit calls.
pub const SEND_TIMEOUT_SECS: u64 = 15;

/// Default API root. Override with `THCLAWS_TELEGRAM_API` for dev / a
/// local mock server.
pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";

#[derive(Debug, thiserror::Error)]
pub enum TelegramClientError {
    #[error("http: {0}")]
    Http(String),
    #[error("telegram API error {code:?}: {description}")]
    Api {
        code: Option<i64>,
        description: String,
    },
    #[error("unauthorized — bot token rejected (401)")]
    Unauthorized,
    #[error("cancelled")]
    Cancelled,
}

/// Sink the session implements so the client can hand it updates. Kept
/// tiny so testing the client doesn't need an agent.
#[async_trait::async_trait]
pub trait TelegramUpdateSink: Send + Sync + 'static {
    async fn on_update(&self, update: Update);
}

pub struct TelegramClient {
    token: String,
    api_base: String,
    http: reqwest::Client,
    cancel: Option<crate::cancel::CancelToken>,
}

impl TelegramClient {
    pub fn new(token: impl Into<String>) -> Self {
        let api_base = std::env::var("THCLAWS_TELEGRAM_API")
            .ok()
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        Self {
            token: token.into(),
            api_base,
            http: reqwest::Client::builder()
                .user_agent(concat!("thclaws-core/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(SEND_TIMEOUT_SECS))
                .build()
                .expect("reqwest client build"),
            cancel: None,
        }
    }

    pub fn with_cancel(mut self, token: crate::cancel::CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Build `{base}/bot{token}/{method}`.
    pub fn method_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base, self.token, method)
    }

    /// `getMe` — confirms the token works and returns the bot identity
    /// (used for the GUI status pill + pairing prompt `@botname`).
    pub async fn get_me(&self) -> Result<User, TelegramClientError> {
        let url = self.method_url("getMe");
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(TelegramClientError::Unauthorized);
        }
        let body: ApiResponse<User> = resp
            .json()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        api_result(body)
    }

    pub async fn send_message(&self, msg: &SendMessage) -> Result<(), TelegramClientError> {
        self.post_unit("sendMessage", msg).await
    }

    /// `sendMessage`, returning the new message's id — needed by the
    /// streaming preview (Tier 3.1) so subsequent deltas can edit it in
    /// place via `editMessageText`.
    pub async fn send_message_returning_id(
        &self,
        msg: &SendMessage,
    ) -> Result<i64, TelegramClientError> {
        let url = self.method_url("sendMessage");
        let resp = self
            .http
            .post(&url)
            .json(msg)
            .send()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(TelegramClientError::Unauthorized);
        }
        let parsed: ApiResponse<Message> = resp
            .json()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        api_result(parsed).map(|m| m.message_id)
    }

    /// Convenience: plain HTML text to a chat.
    pub async fn send_text(
        &self,
        chat_id: i64,
        text: impl Into<String>,
    ) -> Result<(), TelegramClientError> {
        self.send_message(&SendMessage::text(chat_id, text)).await
    }

    pub async fn answer_callback_query(
        &self,
        answer: &AnswerCallbackQuery,
    ) -> Result<(), TelegramClientError> {
        self.post_unit("answerCallbackQuery", answer).await
    }

    pub async fn edit_message_text(
        &self,
        edit: &EditMessageText,
    ) -> Result<(), TelegramClientError> {
        self.post_unit("editMessageText", edit).await
    }

    /// `getChatMember` — the bot's membership/role in a chat. Tier 2's
    /// channel admin-rights probe calls this with the bot's own user id
    /// to confirm it can post (see [`super::channel`]).
    pub async fn get_chat_member(
        &self,
        chat_id: i64,
        user_id: i64,
    ) -> Result<ChatMember, TelegramClientError> {
        let url = self.method_url("getChatMember");
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("chat_id", chat_id.to_string()),
                ("user_id", user_id.to_string()),
            ])
            .send()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(TelegramClientError::Unauthorized);
        }
        let body: ApiResponse<ChatMember> = resp
            .json()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        api_result(body)
    }

    /// POST a JSON body to `method`, discarding the `result` payload.
    async fn post_unit<B: serde::Serialize>(
        &self,
        method: &str,
        body: &B,
    ) -> Result<(), TelegramClientError> {
        let url = self.method_url(method);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(TelegramClientError::Unauthorized);
        }
        let parsed: ApiResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        api_result(parsed).map(|_| ())
    }

    /// Run until cancelled. Drains any boot-time backlog (so old DMs
    /// don't replay on startup), then long-polls forever, dispatching
    /// updates to `sink` and reconnecting with exponential backoff on
    /// transport error. A 401 is fatal (bad token) — surfaced so the
    /// caller can disconnect rather than spin.
    pub async fn run<S: TelegramUpdateSink>(&self, sink: S) -> Result<(), TelegramClientError> {
        eprintln!(
            "[telegram] client starting (bot {})",
            redact_token(&self.token)
        );

        // Drain backlog: advance the offset past anything pending at
        // boot without dispatching it. Best-effort — a transport error
        // here just means we start from offset 0 and the first real
        // poll handles the backlog instead.
        let mut offset = match self.drain_backlog().await {
            Ok(o) => o,
            Err(TelegramClientError::Cancelled) => return Err(TelegramClientError::Cancelled),
            Err(TelegramClientError::Unauthorized) => {
                return Err(TelegramClientError::Unauthorized)
            }
            Err(e) => {
                eprintln!("[telegram] backlog drain failed (continuing): {e}");
                0
            }
        };

        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);
        loop {
            if self.is_cancelled() {
                return Err(TelegramClientError::Cancelled);
            }
            match self.poll_once(offset, &sink).await {
                Ok(new_offset) => {
                    offset = new_offset;
                    backoff = Duration::from_secs(1);
                }
                Err(TelegramClientError::Cancelled) => return Err(TelegramClientError::Cancelled),
                Err(TelegramClientError::Unauthorized) => {
                    eprintln!("[telegram] token rejected (401); stopping client");
                    return Err(TelegramClientError::Unauthorized);
                }
                Err(e) => {
                    eprintln!("[telegram] poll failed: {e}; backoff {backoff:?}");
                    if self.sleep_with_cancel(backoff).await {
                        return Err(TelegramClientError::Cancelled);
                    }
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }

    /// One long-poll cycle. Returns the next `offset` (max update_id + 1,
    /// or the unchanged offset if the batch was empty).
    async fn poll_once<S: TelegramUpdateSink>(
        &self,
        offset: i64,
        sink: &S,
    ) -> Result<i64, TelegramClientError> {
        let updates = tokio::select! {
            _ = self.cancelled() => return Err(TelegramClientError::Cancelled),
            res = self.get_updates(offset, LONG_POLL_TIMEOUT_SECS) => res?,
        };
        let mut next = offset;
        for update in updates {
            next = next.max(update.update_id + 1);
            sink.on_update(update).await;
        }
        Ok(next)
    }

    /// Fetch pending updates with a zero long-poll (immediate return),
    /// discard them, and return the offset to start serving from. Logs
    /// the discard count so an operator can see why a pre-launch DM
    /// didn't get a reply.
    async fn drain_backlog(&self) -> Result<i64, TelegramClientError> {
        let updates = tokio::select! {
            _ = self.cancelled() => return Err(TelegramClientError::Cancelled),
            res = self.get_updates(0, 0) => res?,
        };
        let next = next_offset(0, &updates);
        if !updates.is_empty() {
            eprintln!(
                "[telegram] discarded {} backlog update(s) at startup",
                updates.len()
            );
        }
        Ok(next)
    }

    /// Raw `getUpdates` call with explicit offset + server-side timeout.
    async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u64,
    ) -> Result<Vec<Update>, TelegramClientError> {
        let url = self.method_url("getUpdates");
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", timeout_secs.to_string()),
            ])
            .send()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(TelegramClientError::Unauthorized);
        }
        let body: ApiResponse<Vec<Update>> = resp
            .json()
            .await
            .map_err(|e| TelegramClientError::Http(e.to_string()))?;
        api_result(body)
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|t| t.is_cancelled())
    }

    async fn cancelled(&self) {
        if let Some(t) = self.cancel.as_ref() {
            t.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    }

    async fn sleep_with_cancel(&self, dur: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(dur) => false,
            _ = self.cancelled() => true,
        }
    }
}

/// Unwrap an `ApiResponse<T>` into `Ok(result)` or a structured error.
fn api_result<T>(resp: ApiResponse<T>) -> Result<T, TelegramClientError> {
    if resp.ok {
        resp.result.ok_or_else(|| TelegramClientError::Api {
            code: None,
            description: "ok=true but result missing".into(),
        })
    } else if resp.error_code == Some(401) {
        Err(TelegramClientError::Unauthorized)
    } else {
        Err(TelegramClientError::Api {
            code: resp.error_code,
            description: resp.description.unwrap_or_default(),
        })
    }
}

/// Next poll offset: one past the highest `update_id` in the batch, or
/// the current offset when the batch is empty.
pub fn next_offset(current: i64, updates: &[Update]) -> i64 {
    updates
        .iter()
        .map(|u| u.update_id + 1)
        .max()
        .unwrap_or(current)
        .max(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> TelegramClient {
        TelegramClient::new("123456789:AAFmKQk_abcdefghijklmnopqrstuvwxyz")
    }

    #[test]
    fn method_url_embeds_token_and_method() {
        std::env::remove_var("THCLAWS_TELEGRAM_API");
        let c = client();
        assert_eq!(
            c.method_url("getUpdates"),
            "https://api.telegram.org/bot123456789:AAFmKQk_abcdefghijklmnopqrstuvwxyz/getUpdates"
        );
    }

    #[test]
    fn api_base_override_is_honored() {
        std::env::set_var("THCLAWS_TELEGRAM_API", "http://localhost:8088/");
        let c = TelegramClient::new("1:aaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(
            c.method_url("sendMessage"),
            "http://localhost:8088/bot1:aaaaaaaaaaaaaaaaaaaaaa/sendMessage"
        );
        std::env::remove_var("THCLAWS_TELEGRAM_API");
    }

    #[test]
    fn request_timeout_exceeds_long_poll_timeout() {
        // The stall detector contract: the client must wait longer than
        // the server holds the connection, or every empty long-poll
        // would spuriously time out.
        assert!(REQUEST_TIMEOUT_SECS > LONG_POLL_TIMEOUT_SECS);
    }

    fn update(id: i64) -> Update {
        serde_json::from_str(&format!(
            r#"{{"update_id":{id},"message":{{"message_id":1,"chat":{{"id":5,"type":"private"}},"text":"hi"}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn next_offset_advances_past_max_id() {
        let ups = vec![update(10), update(12), update(11)];
        assert_eq!(next_offset(0, &ups), 13);
    }

    #[test]
    fn next_offset_keeps_current_when_empty() {
        assert_eq!(next_offset(7, &[]), 7);
    }

    #[test]
    fn next_offset_never_regresses() {
        // A stale/duplicate batch below the current cursor mustn't pull
        // the offset backwards.
        let ups = vec![update(3)];
        assert_eq!(next_offset(100, &ups), 100);
    }

    #[test]
    fn api_result_ok_unwraps() {
        let r: ApiResponse<i32> = ApiResponse {
            ok: true,
            result: Some(42),
            description: None,
            error_code: None,
        };
        assert_eq!(api_result(r).unwrap(), 42);
    }

    #[test]
    fn api_result_401_maps_to_unauthorized() {
        let r: ApiResponse<i32> = ApiResponse {
            ok: false,
            result: None,
            description: Some("Unauthorized".into()),
            error_code: Some(401),
        };
        assert!(matches!(
            api_result(r),
            Err(TelegramClientError::Unauthorized)
        ));
    }

    #[test]
    fn api_result_other_error_carries_code_and_description() {
        let r: ApiResponse<i32> = ApiResponse {
            ok: false,
            result: None,
            description: Some("Bad Request: chat not found".into()),
            error_code: Some(400),
        };
        match api_result(r) {
            Err(TelegramClientError::Api { code, description }) => {
                assert_eq!(code, Some(400));
                assert!(description.contains("chat not found"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
