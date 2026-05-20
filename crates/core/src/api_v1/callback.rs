//! Webhook delivery for the `x_callback` extension on
//! `POST /v1/chat/completions`. See [`super::chat::XCallback`] for the
//! request side and `dev-plan/23-thclaws-async-callback.md` for the
//! end-to-end design.
//!
//! Lifecycle: chat handler validates the request, returns 202 ACK,
//! spawns the agent run on a detached task. The task awaits the
//! `AgentTurnOutcome` and calls [`deliver`] with the terminal payload.
//!
//! Best-effort delivery: 3 attempts with exponential backoff (~0s,
//! ~10s, ~60s), then drop with a structured log line. The caller is
//! responsible for reconciliation if all retries fail — thClaws will
//! not buffer indefinitely.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::Duration;

use crate::agent::AgentTurnOutcome;

const RETRY_DELAYS_MS: &[u64] = &[0, 10_000, 60_000];
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Validated webhook target. Construct via [`Self::from_request`].
#[derive(Clone, Debug)]
pub struct CallbackTarget {
    pub url: String,
    pub api_key: String,
    pub run_id: String,
    pub idempotency_key: String,
}

impl CallbackTarget {
    /// Validate the user-supplied envelope. Returns an error message
    /// suitable for surfacing in the 400 response body. Rules:
    ///
    /// - `url` must parse as http/https (other schemes refused — no
    ///   `file://` shenanigans even though reqwest would refuse them).
    /// - `api_key` must be non-empty (we don't inspect contents).
    /// - `run_id` must be non-empty (echoed in callback body).
    pub fn from_request(req: &super::chat::XCallback) -> Result<Self, String> {
        if req.url.trim().is_empty() {
            return Err("x_callback.url is required".into());
        }
        let parsed = url::Url::parse(&req.url)
            .map_err(|e| format!("x_callback.url is not a valid URL: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!(
                "x_callback.url must be http or https (got {})",
                parsed.scheme()
            ));
        }
        if req.api_key.trim().is_empty() {
            return Err("x_callback.api_key is required".into());
        }
        if req.run_id.trim().is_empty() {
            return Err("x_callback.run_id is required".into());
        }
        let idempotency_key = req
            .idempotency_key
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| req.run_id.clone());
        Ok(Self {
            url: req.url.clone(),
            api_key: req.api_key.clone(),
            run_id: req.run_id.clone(),
            idempotency_key,
        })
    }
}

/// Body POSTed to the callback URL. Stable wire shape — additive fields
/// are fine, but renames/removals are breaking changes for receivers.
#[derive(Serialize, Clone, Debug)]
pub struct CallbackPayload {
    pub run_id: String,
    /// `"succeeded"` | `"failed"` | `"cancelled"` (cancelled reserved
    /// for a future cancel endpoint).
    pub status: &'static str,
    /// OpenAI-normalized finish reason: `"stop"` | `"length"` |
    /// `"tool_calls"` | `"error"`. Mirrors what the sync path returns
    /// via [`super::chat::map_finish_reason`].
    pub finish_reason: String,
    pub model: String,
    /// Final assistant text. May be empty for tool-only outcomes.
    pub summary: String,
    pub usage: PayloadUsage,
    /// Names of tool calls executed during the run. Detailed events
    /// (input/output blobs) are an A2-iteration follow-up — v1 ships
    /// just the names so receivers can show a count + list.
    pub tool_calls: Vec<String>,
    pub tool_denials: Vec<String>,
    pub iterations: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PayloadError>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct PayloadUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    // dev-plan/24: extra token-type counts for downstream cost compute.
    // Receivers (paperclip-adapter, thcompany) multiply these by the
    // pricing rates from /v1/models. thClaws never emits cost_usd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u32>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PayloadError {
    pub code: &'static str,
    pub message: String,
}

impl CallbackPayload {
    /// Build a "the runner died" payload after a panic. Used by the
    /// watcher task in [`super::chat::chat_completions_async`] so the
    /// receiver gets a terminal failure instead of waiting forever.
    /// Model is unknown (we don't have access to the original request),
    /// so we leave it empty.
    pub fn panic_payload(run_id: &str, started_at: DateTime<Utc>) -> Self {
        Self {
            run_id: run_id.into(),
            status: "failed",
            finish_reason: "error".into(),
            model: String::new(),
            summary: String::new(),
            usage: PayloadUsage::default(),
            tool_calls: vec![],
            tool_denials: vec![],
            iterations: 0,
            error: Some(PayloadError {
                code: "task_panicked",
                message: "thClaws agent task panicked during async run".into(),
            }),
            started_at,
            completed_at: Utc::now(),
        }
    }

    pub fn from_outcome(
        run_id: &str,
        model: &str,
        started_at: DateTime<Utc>,
        outcome: crate::error::Result<AgentTurnOutcome>,
    ) -> Self {
        let completed_at = Utc::now();
        match outcome {
            Ok(o) => {
                let finish_reason = map_finish_reason_local(o.stop_reason.as_deref());
                let usage = o
                    .usage
                    .map(|u| PayloadUsage {
                        prompt_tokens: u.input_tokens,
                        completion_tokens: u.output_tokens,
                        total_tokens: u.input_tokens + u.output_tokens,
                        cached_input_tokens: u.cache_read_input_tokens,
                        cache_creation_input_tokens: u.cache_creation_input_tokens,
                        reasoning_output_tokens: u.reasoning_output_tokens,
                    })
                    .unwrap_or_default();
                Self {
                    run_id: run_id.into(),
                    status: if finish_reason == "error" {
                        "failed"
                    } else {
                        "succeeded"
                    },
                    finish_reason,
                    model: model.into(),
                    summary: o.text,
                    usage,
                    tool_calls: o.tool_calls,
                    tool_denials: o.tool_denials,
                    iterations: o.iterations,
                    error: None,
                    started_at,
                    completed_at,
                }
            }
            Err(e) => Self {
                run_id: run_id.into(),
                status: "failed",
                finish_reason: "error".into(),
                model: model.into(),
                summary: String::new(),
                usage: PayloadUsage::default(),
                tool_calls: vec![],
                tool_denials: vec![],
                iterations: 0,
                error: Some(PayloadError {
                    code: "agent_error",
                    message: e.to_string(),
                }),
                started_at,
                completed_at,
            },
        }
    }
}

// Mirrors `super::chat::map_finish_reason` — kept local so this module
// doesn't take a dependency on chat::'s private function. If a stop
// reason maps to OpenAI's "error" we propagate it as the payload status.
fn map_finish_reason_local(stop: Option<&str>) -> String {
    match stop {
        Some("end_turn") | Some("stop") | Some("stop_sequence") | None => "stop".into(),
        Some("max_tokens") | Some("length") => "length".into(),
        Some("tool_use") | Some("tool_calls") => "tool_calls".into(),
        Some("error") => "error".into(),
        Some(other) => other.into(),
    }
}

/// Deliver the payload with retries. Never panics, never throws —
/// failure modes are logged and dropped. Receiver-side correctness
/// (idempotency, dedup) is enforced via the `Idempotency-Key` header.
pub async fn deliver(target: &CallbackTarget, payload: &CallbackPayload) {
    let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[api_v1] callback_failed run_id={} reason=client_build error=\"{e}\"",
                target.run_id
            );
            return;
        }
    };

    for (attempt, delay_ms) in RETRY_DELAYS_MS.iter().copied().enumerate() {
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        let req = client
            .post(&target.url)
            .bearer_auth(&target.api_key)
            .header("Idempotency-Key", &target.idempotency_key)
            .header("User-Agent", concat!("thclaws/", env!("CARGO_PKG_VERSION")))
            .json(payload);
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                eprintln!(
                    "[api_v1] callback_delivered run_id={} status={} attempt={}",
                    target.run_id,
                    resp.status().as_u16(),
                    attempt + 1
                );
                return;
            }
            Ok(resp) => {
                let status = resp.status();
                eprintln!(
                    "[api_v1] callback_retried run_id={} status={} attempt={}",
                    target.run_id,
                    status.as_u16(),
                    attempt + 1
                );
                // 4xx (except 429) gives up — receiver said no, retrying
                // won't help. 5xx + 429 + network errors fall through.
                if status.is_client_error() && status.as_u16() != 429 {
                    break;
                }
            }
            Err(e) => {
                eprintln!(
                    "[api_v1] callback_retried run_id={} reason=network attempt={} error=\"{e}\"",
                    target.run_id,
                    attempt + 1
                );
            }
        }
    }

    eprintln!(
        "[api_v1] callback_failed run_id={} reason=retries_exhausted",
        target.run_id
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str, api_key: &str, run_id: &str) -> super::super::chat::XCallback {
        super::super::chat::XCallback {
            url: url.into(),
            api_key: api_key.into(),
            run_id: run_id.into(),
            idempotency_key: None,
        }
    }

    #[test]
    fn rejects_missing_url() {
        assert!(CallbackTarget::from_request(&req("", "k", "r")).is_err());
    }

    #[test]
    fn rejects_non_http_scheme() {
        let err = CallbackTarget::from_request(&req("file:///tmp/x", "k", "r")).unwrap_err();
        assert!(err.contains("http or https"));
    }

    #[test]
    fn rejects_malformed_url() {
        let err = CallbackTarget::from_request(&req("not a url", "k", "r")).unwrap_err();
        assert!(err.contains("not a valid URL"));
    }

    #[test]
    fn rejects_empty_api_key() {
        assert!(CallbackTarget::from_request(&req("https://x.test/cb", "", "r")).is_err());
    }

    #[test]
    fn rejects_empty_run_id() {
        assert!(CallbackTarget::from_request(&req("https://x.test/cb", "k", "")).is_err());
    }

    #[test]
    fn idempotency_key_defaults_to_run_id() {
        let t = CallbackTarget::from_request(&req("https://x.test/cb", "k", "run-42")).unwrap();
        assert_eq!(t.idempotency_key, "run-42");
        assert_eq!(t.run_id, "run-42");
    }

    #[test]
    fn idempotency_key_override_wins() {
        let mut r = req("https://x.test/cb", "k", "run-42");
        r.idempotency_key = Some("k-explicit".into());
        let t = CallbackTarget::from_request(&r).unwrap();
        assert_eq!(t.idempotency_key, "k-explicit");
    }

    #[test]
    fn empty_idempotency_override_falls_back_to_run_id() {
        let mut r = req("https://x.test/cb", "k", "run-42");
        r.idempotency_key = Some("   ".into());
        let t = CallbackTarget::from_request(&r).unwrap();
        assert_eq!(t.idempotency_key, "run-42");
    }

    #[test]
    fn payload_from_ok_outcome_marks_succeeded() {
        let outcome = Ok(AgentTurnOutcome {
            text: "hello".into(),
            tool_calls: vec!["Read".into()],
            tool_denials: vec![],
            stop_reason: Some("end_turn".into()),
            usage: None,
            iterations: 2,
        });
        let p = CallbackPayload::from_outcome("r1", "claude-haiku-4-5", Utc::now(), outcome);
        assert_eq!(p.status, "succeeded");
        assert_eq!(p.finish_reason, "stop");
        assert_eq!(p.summary, "hello");
        assert_eq!(p.tool_calls, vec!["Read".to_string()]);
        assert_eq!(p.iterations, 2);
        assert!(p.error.is_none());
    }

    #[test]
    fn payload_from_err_outcome_marks_failed() {
        let outcome: crate::error::Result<AgentTurnOutcome> =
            Err(crate::error::Error::Tool("boom".into()));
        let p = CallbackPayload::from_outcome("r1", "claude-haiku-4-5", Utc::now(), outcome);
        assert_eq!(p.status, "failed");
        assert_eq!(p.finish_reason, "error");
        let err = p.error.expect("error field populated");
        assert_eq!(err.code, "agent_error");
        assert!(err.message.contains("boom"));
    }

    // ── integration: deliver() against a real mock HTTP receiver ──

    #[tokio::test]
    async fn deliver_posts_to_receiver_with_bearer_and_idempotency_key() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .and(header("authorization", "Bearer test-secret"))
            .and(header("idempotency-key", "run-42"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let target = CallbackTarget {
            url: format!("{}/cb", server.uri()),
            api_key: "test-secret".into(),
            run_id: "run-42".into(),
            idempotency_key: "run-42".into(),
        };
        let payload = CallbackPayload {
            run_id: "run-42".into(),
            status: "succeeded",
            finish_reason: "stop".into(),
            model: "claude-haiku-4-5".into(),
            summary: "hello".into(),
            usage: PayloadUsage::default(),
            tool_calls: vec![],
            tool_denials: vec![],
            iterations: 1,
            error: None,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        };

        deliver(&target, &payload).await;
        // MockServer verifies on drop that exactly one request matched
        // the matchers above — that's the test assertion.
    }

    #[tokio::test]
    async fn deliver_gives_up_on_4xx_other_than_429() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1) // 4xx other than 429 ⇒ single attempt, no retries
            .mount(&server)
            .await;

        let target = CallbackTarget {
            url: format!("{}/cb", server.uri()),
            api_key: "k".into(),
            run_id: "r1".into(),
            idempotency_key: "r1".into(),
        };
        let payload = CallbackPayload::panic_payload("r1", Utc::now());
        deliver(&target, &payload).await;
    }

    #[tokio::test]
    async fn deliver_serializes_payload_shape_correctly() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Partial body match: confirm the load-bearing fields are
        // present + carry the expected values. Other optional fields
        // (started_at, completed_at — runtime timestamps) are unmatched.
        Mock::given(method("POST"))
            .and(path("/cb"))
            .and(body_partial_json(serde_json::json!({
                "run_id": "r1",
                "status": "succeeded",
                "finish_reason": "stop",
                "model": "claude-haiku-4-5",
                "summary": "ok",
                "tool_calls": ["Read", "Bash"],
                "iterations": 3,
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let target = CallbackTarget {
            url: format!("{}/cb", server.uri()),
            api_key: "k".into(),
            run_id: "r1".into(),
            idempotency_key: "r1".into(),
        };
        let outcome = Ok(AgentTurnOutcome {
            text: "ok".into(),
            tool_calls: vec!["Read".into(), "Bash".into()],
            tool_denials: vec![],
            stop_reason: Some("end_turn".into()),
            usage: None,
            iterations: 3,
        });
        let payload = CallbackPayload::from_outcome("r1", "claude-haiku-4-5", Utc::now(), outcome);
        deliver(&target, &payload).await;
    }

    #[tokio::test]
    async fn deliver_does_not_panic_when_url_unreachable() {
        // Random unused loopback port — connection refused on the first
        // attempt + each retry. Test passes if deliver() returns rather
        // than hanging / panicking.
        let target = CallbackTarget {
            url: "http://127.0.0.1:1/cb".into(),
            api_key: "k".into(),
            run_id: "r-unreachable".into(),
            idempotency_key: "r-unreachable".into(),
        };
        let payload = CallbackPayload::panic_payload("r-unreachable", Utc::now());
        // We can't wait the full 70s retry window in a unit test, so
        // race against a short timer — anything finite is fine because
        // the property under test is "doesn't hang/panic", not "fails
        // fast". The function will eventually exit via the retry loop
        // exhaustion path.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            deliver(&target, &payload),
        )
        .await;
        // It's OK if it didn't finish within 5s (would hit retry delays)
        // — we just verify it didn't panic out by reaching this line.
        let _ = result;
    }

    #[test]
    fn payload_serializes_idempotency_friendly() {
        let p = CallbackPayload {
            run_id: "r1".into(),
            status: "succeeded",
            finish_reason: "stop".into(),
            model: "claude-haiku-4-5".into(),
            summary: "hi".into(),
            usage: PayloadUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_input_tokens: None,
                cache_creation_input_tokens: None,
                reasoning_output_tokens: None,
            },
            tool_calls: vec!["Read".into()],
            tool_denials: vec![],
            iterations: 1,
            error: None,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        };
        let body = serde_json::to_value(&p).expect("serialize");
        assert_eq!(body["run_id"], "r1");
        assert_eq!(body["status"], "succeeded");
        assert_eq!(body["usage"]["total_tokens"], 15);
        assert!(body["error"].is_null() || body.get("error").is_none());
    }
}
