//! Boot a [`TelegramSession`] from a [`TelegramConfig`] and own its
//! lifetime for the GUI worker. Mirrors [`crate::line::bootstrap`].
//!
//! `WorkerForwardHandler` routes each inbound Telegram message into the
//! worker's `ShellInput::TelegramMessage` channel; the worker drives
//! `Agent::run_turn`, captures the final assistant text, and answers via
//! a `oneshot::Sender`, which this handler returns so the session sink
//! posts the Telegram reply.
//!
//! [`TelegramSessionHandle`] is what the worker stashes — the cancel
//! token (for disconnect), a status snapshot, and the shared
//! approver / pairing-manager / config so the IPC layer can resolve
//! tool approvals and pairing requests against the same live state.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

use super::approver::TelegramApprover;
use super::client::TelegramClient;
use super::config::TelegramConfig;
use super::pairing::PairingManager;
use super::session::{ChatRegistry, TelegramMessageHandler, TelegramSession};
use crate::cancel::CancelToken;

/// Forward Telegram text to the worker, wait for the captured agent
/// reply, return it. On worker-channel closure return a fallback so the
/// Telegram user sees *something* rather than dead silence.
struct WorkerForwardHandler {
    input_tx: mpsc::Sender<crate::shared_session::ShellInput>,
}

#[async_trait]
impl TelegramMessageHandler for WorkerForwardHandler {
    async fn handle_message(
        &self,
        text: String,
        agent_id: Option<String>,
        _preview: Option<Arc<dyn super::stream::PreviewSink>>,
    ) -> Option<String> {
        // Tier 2: the GUI worker runs a single shared session, so a
        // per-topic `agent_id` can't yet route to a different agent here
        // (that needs worker-side multi-agent spawning — a follow-up).
        // The headless path (`telegram::headless`) honours it fully.
        // Tier 3.1: streaming preview is headless-only too; the GUI worker
        // ignores `_preview` and the sink sends the final reply once.
        if let Some(a) = &agent_id {
            eprintln!(
                "[telegram] topic routed to agent '{a}' (GUI worker runs the shared session)"
            );
        }
        let (tx, rx) = oneshot::channel();
        if self
            .input_tx
            .send(crate::shared_session::ShellInput::TelegramMessage { text, respond: tx })
            .is_err()
        {
            return Some("⚠️ thClaws worker is unavailable; restart thClaws and try again.".into());
        }
        match rx.await {
            Ok(s) if !s.trim().is_empty() => Some(s),
            _ => Some("(thClaws agent finished the turn without a text reply.)".into()),
        }
    }
}

/// Live Telegram-bridge handle stored on the worker. Dropping it alone
/// won't stop the session — fire `cancel.cancel()` first (the IPC
/// `telegram_disconnect` arm does this).
pub struct TelegramSessionHandle {
    pub cancel: CancelToken,
    pub status: TelegramStatus,
    pub join: tokio::task::JoinHandle<()>,
    /// Shared approver — the agent's `ApprovalSink` swaps to this while
    /// Telegram is connected, so inline-keyboard taps resolve the same
    /// pending decisions the agent loop is awaiting.
    pub approver: Arc<TelegramApprover>,
    /// Shared API client (used by the worker for status / restart paths).
    pub client: Arc<TelegramClient>,
    /// Pending pairing requests — the IPC `telegram_pairing_*` arms
    /// approve/reject against this.
    pub pairing: Arc<PairingManager>,
    /// Shared config — pairing approval appends to `allow_from` here and
    /// the session sink reads it for authorization.
    pub config: Arc<Mutex<TelegramConfig>>,
    /// Per-chat registry for the status pill's counts.
    pub registry: Arc<ChatRegistry>,
}

/// Snapshot of the bridge state, serialised into the `telegram_status`
/// IPC payload for the GUI sidebar / connect modal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TelegramStatus {
    pub state: &'static str,
    /// `@botname` from `getMe`, when known.
    pub bot_username: Option<String>,
    pub pending_approvals: usize,
    pub pending_pairings: usize,
    pub active_chats: usize,
}

impl TelegramStatus {
    pub fn disconnected() -> Self {
        Self {
            state: "disconnected",
            bot_username: None,
            pending_approvals: 0,
            pending_pairings: 0,
            active_chats: 0,
        }
    }

    pub fn connected(bot_username: Option<String>) -> Self {
        Self {
            state: "connected",
            bot_username,
            pending_approvals: 0,
            pending_pairings: 0,
            active_chats: 0,
        }
    }
}

/// Spawn a `TelegramSession` on the tokio runtime with the worker-
/// forwarding handler + `TelegramApprover`. The caller (worker) has
/// already validated the token via `getMe` and passes the resolved
/// `bot_username` for the status pill.
///
/// `input_tx` is the worker's `ShellInput` channel — every inbound
/// Telegram message arrives as `ShellInput::TelegramMessage`, runs
/// through the agent loop, and the captured assistant text is shipped
/// back over Telegram.
pub fn spawn(
    config: TelegramConfig,
    bot_username: Option<String>,
    input_tx: mpsc::Sender<crate::shared_session::ShellInput>,
) -> TelegramSessionHandle {
    let cancel = CancelToken::new();
    let token = config.resolved_token().unwrap_or_default();

    let client = Arc::new(TelegramClient::new(token).with_cancel(cancel.clone()));
    let approver = Arc::new(TelegramApprover::new(client.clone()));
    let pairing = Arc::new(PairingManager::new());
    let shared_config = Arc::new(Mutex::new(config));
    let handler: Arc<dyn TelegramMessageHandler> = Arc::new(WorkerForwardHandler { input_tx });

    let session = Arc::new(
        TelegramSession::new(
            client.clone(),
            handler,
            shared_config.clone(),
            pairing.clone(),
        )
        .with_approver(approver.clone()),
    );
    let registry = session.registry();

    let cancel_for_task = cancel.clone();
    let join = tokio::spawn(async move {
        if let Err(e) = session.run().await {
            eprintln!("[telegram] session ended: {e}");
        }
        cancel_for_task.cancel();
    });

    TelegramSessionHandle {
        cancel,
        status: TelegramStatus::connected(bot_username),
        join,
        approver,
        client,
        pairing,
        config: shared_config,
        registry,
    }
}
