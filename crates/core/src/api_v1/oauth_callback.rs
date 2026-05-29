//! Pod-side OAuth callback for headless MCP re-auth (dev-plan/29).
//!
//! On a laptop the OAuth flow binds a loopback port and the
//! provider's redirect lands directly on that local listener. On a
//! `--serve` pod the owner consents in their **laptop's** browser, so
//! the redirect must target a URL their laptop can reach — i.e. the
//! pod's public hostname. This module hosts the pod-side endpoint
//! plus the in-memory pending-auth store that links a state nonce
//! (sent in the auth URL) back to the code-verifier + token-endpoint
//! context needed to complete the exchange.
//!
//! Flow:
//!   1. `/mcp reauth <name>` (slash command) calls `oauth::authorize_pod`,
//!      which generates PKCE + state, registers a DCR client against
//!      `<public_base>/v1/oauth/callback`, stashes the context here
//!      via [`insert_pending`], and returns the auth URL.
//!   2. Owner clicks the auth URL on their laptop, consents.
//!   3. Provider redirects to `<public_base>/v1/oauth/callback?code=…&state=…`.
//!   4. [`oauth_callback`] looks up the state via [`take_pending`],
//!      exchanges code for tokens via [`crate::oauth::exchange_code_for_token`],
//!      writes the result to the pod's `TokenStore`, returns an HTML
//!      success page.
//!
//! State entries are TTL-pruned at 10 minutes — enough to consent +
//! click but short enough that a leaked state nonce can't be replayed
//! into a long-lived hijack window.

use axum::extract::RawQuery;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const PENDING_TTL_SECS: u64 = 600;

/// Context an inflight pod-mode OAuth flow needs to complete a code
/// exchange when the provider's redirect lands on
/// `/v1/oauth/callback`. All sensitive fields stay in-memory only.
pub struct PendingAuth {
    pub code_verifier: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scope: String,
    pub token_endpoint: String,
    pub authorization_server_origin: String,
    /// MCP server URL — the [`crate::oauth::TokenStore`] key the new
    /// `TokenEntry` lands under after a successful exchange.
    pub server_url: String,
    pub expires_at: u64,
}

static PENDING: OnceLock<Mutex<HashMap<String, PendingAuth>>> = OnceLock::new();

fn pending_map() -> &'static Mutex<HashMap<String, PendingAuth>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Store a pending OAuth context keyed by its `state` nonce. Caller
/// generates `state` via the same PKCE-friendly RNG the laptop flow
/// uses (no constraints here — just opaque bytes).
pub fn insert_pending(state: String, mut p: PendingAuth) {
    let now = now_secs();
    p.expires_at = now + PENDING_TTL_SECS;
    let mut map = pending_map().lock().expect("pending mutex poisoned");
    map.retain(|_, v| v.expires_at > now);
    map.insert(state, p);
}

/// Atomically remove the pending context for `state`. Returns `None`
/// if the state is unknown or expired (the callback handler treats
/// both as "no such flow" — same 400 response either way).
pub fn take_pending(state: &str) -> Option<PendingAuth> {
    let now = now_secs();
    let mut map = pending_map().lock().expect("pending mutex poisoned");
    map.retain(|_, v| v.expires_at > now);
    map.remove(state).filter(|p| p.expires_at > now)
}

/// Parse a `&`-joined URL query string into name → value pairs. URL-
/// decode each side; later occurrences of a duplicate key win (mirrors
/// the conventional `Query` extractor's behaviour). Returns an empty
/// map when the query string is missing or empty.
fn parse_query(q: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(q) = q else { return out };
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let k = urlencoding::decode(k)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| k.to_string());
        let v = urlencoding::decode(v)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| v.to_string());
        out.insert(k, v);
    }
    out
}

/// `GET /v1/oauth/callback?code=…&state=…`
///
/// Public endpoint — no Bearer check (the provider's redirect isn't
/// going to carry our API token). Security comes from the `state`
/// nonce: only states the slash command stashed via [`insert_pending`]
/// match. Anything else is rejected.
pub async fn oauth_callback(RawQuery(raw): RawQuery) -> Response {
    let p = parse_query(raw.as_deref());
    if let Some(err) = p.get("error") {
        let desc = p.get("error_description").map(String::as_str).unwrap_or("");
        return failure_page(&format!("Authorization denied: {err} {desc}"));
    }
    let (Some(code), Some(state)) = (p.get("code").cloned(), p.get("state").cloned()) else {
        return failure_page("Callback missing 'code' or 'state' query parameter.");
    };
    let Some(pending) = take_pending(&state) else {
        return failure_page(
            "No matching pending authorization (state unknown or expired). \
             Restart with `/mcp reauth <name>` and click the fresh link.",
        );
    };

    let client = reqwest::Client::new();
    let entry = match crate::oauth::exchange_code_for_token(
        &client,
        &pending.token_endpoint,
        &code,
        &pending.redirect_uri,
        &pending.client_id,
        pending.client_secret.as_deref(),
        &pending.code_verifier,
        &pending.scope,
        &pending.authorization_server_origin,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            return failure_page(&format!(
                "Token exchange failed: {e}. The authorization code may be reused or expired — start over with `/mcp reauth`."
            ));
        }
    };

    let mut store = crate::oauth::TokenStore::load();
    store.set(&pending.server_url, entry);
    store.save();

    success_page(&pending.server_url)
}

fn success_page(server_url: &str) -> Response {
    let body = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>thClaws — auth complete</title>
<style>
  body {{ font: 16px system-ui, sans-serif; max-width: 540px; margin: 12vh auto; padding: 0 20px; color: #1f2937; }}
  h1 {{ font-size: 22px; margin-bottom: 6px; }}
  .url {{ color: #6b7280; word-break: break-all; font-family: ui-monospace, Menlo, monospace; font-size: 13px; }}
  .hint {{ margin-top: 18px; color: #4b5563; font-size: 14px; }}
</style>
</head>
<body>
<h1>✅ Authorization complete</h1>
<div class="url">{server_url}</div>
<div class="hint">You can close this tab and return to thClaws — the agent has the new token.</div>
</body></html>"#,
        server_url = htmlescape(server_url),
    );
    (StatusCode::OK, Html(body)).into_response()
}

fn failure_page(message: &str) -> Response {
    let body = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>thClaws — auth failed</title>
<style>
  body {{ font: 16px system-ui, sans-serif; max-width: 540px; margin: 12vh auto; padding: 0 20px; color: #1f2937; }}
  h1 {{ font-size: 22px; margin-bottom: 6px; color: #b91c1c; }}
  .msg {{ color: #374151; font-size: 14px; line-height: 1.5; }}
</style>
</head>
<body>
<h1>❌ Authorization failed</h1>
<div class="msg">{message}</div>
</body></html>"#,
        message = htmlescape(message),
    );
    (StatusCode::BAD_REQUEST, Html(body)).into_response()
}

fn htmlescape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
