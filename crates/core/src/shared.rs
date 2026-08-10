//! Shared-agent mode (cloud-only) — see dev-plan/41.
//!
//! When `THCLAWS_SHARED_AGENT_DIR` points at a mounted, read-only
//! "company brain", the engine runs a **shared agent**: instructions,
//! KMS, skills/commands, and MCP *config* come from that directory and
//! are locked; BYOK is refused (gateway-only); the member's private
//! working dir keeps sessions, files, MCP credentials, and their own
//! additive KMS/MCP.
//!
//! The security boundary lives here in code, not in filesystem
//! permissions — settings/instruction *layering* would otherwise let a
//! member smuggle overrides in via user scope. So the loaders for
//! instructions (`context.rs`), config (`config.rs`), and KMS
//! (`kms.rs`) all consult these helpers and hard-ignore member-scope
//! sources when shared mode is active.

use std::path::PathBuf;

const SHARED_DIR_ENV: &str = "THCLAWS_SHARED_AGENT_DIR";
const STRICT_ENV: &str = "THCLAWS_SHARED_STRICT";
const MODEL_LOCKED_ENV: &str = "THCLAWS_SHARED_MODEL_LOCKED";

/// Provider segments forced through the gateway in shared mode. Mirrors
/// the gateway-routable set in `providers::thclaws_gateway`. Anything not
/// listed (ollama, lmstudio, …) has no gateway route and is simply
/// unavailable to a shared agent — by design, shared agents are
/// gateway-billed only.
///
/// These are exactly the ten `ProviderTier::Featured` providers: what the
/// gateway sells is what it proxies. qwen-cloud / thaillm / groq were
/// dropped 2026-08-10 — see `thclaws_gateway::provider_segment` for why.
/// They remain fully usable with the user's own key.
pub const GATEWAY_ALL_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "google",
    "openrouter",
    "dashscope",
    "zai",
    "deepseek",
    "minimax",
    "xai",
    "moonshot",
];

/// The shared-brain directory when shared mode is active, else `None`.
/// Read fresh each call (cheap); the env var is set once at pod start.
pub fn shared_agent_dir() -> Option<PathBuf> {
    match std::env::var(SHARED_DIR_ENV) {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => None,
    }
}

/// True when shared-agent mode is active.
pub fn is_active() -> bool {
    shared_agent_dir().is_some()
}

fn flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref().map(str::trim),
        Some("1") | Some("true")
    )
}

/// Strict mode: members can't even *add* private skills/MCP/KMS on top of
/// the shared brain (fully deterministic agent). Only meaningful when
/// shared mode is active.
pub fn is_strict() -> bool {
    is_active() && flag(STRICT_ENV)
}

/// Model is pinned by the company — members can't switch.
pub fn is_model_locked() -> bool {
    is_active() && flag(MODEL_LOCKED_ENV)
}

/// Locked instructions file inside the shared brain (`$SHARED/AGENTS.md`).
pub fn shared_agents_md() -> Option<PathBuf> {
    shared_agent_dir().map(|d| d.join("AGENTS.md"))
}

/// Read-only company settings base (`$SHARED/settings.json`).
pub fn shared_settings_json() -> Option<PathBuf> {
    shared_agent_dir().map(|d| d.join("settings.json"))
}

/// Read-only MCP config (`$SHARED/mcp.json`); credentials stay per-member.
pub fn shared_mcp_json() -> Option<PathBuf> {
    shared_agent_dir().map(|d| d.join("mcp.json"))
}

/// Shared KMS root (`$SHARED/kms`, read-only).
pub fn shared_kms_root() -> Option<PathBuf> {
    shared_agent_dir().map(|d| d.join("kms"))
}

/// Shared skills dir (`$SHARED/skills`, read-only).
pub fn shared_skills_dir() -> Option<PathBuf> {
    shared_agent_dir().map(|d| d.join("skills"))
}

/// Shared slash-commands dir (`$SHARED/commands`, read-only).
pub fn shared_commands_dir() -> Option<PathBuf> {
    shared_agent_dir().map(|d| d.join("commands"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reuse the crate-wide env lock so shared-mode tests serialise with
    /// the kms/context tests that also mutate `THCLAWS_SHARED_AGENT_DIR`
    /// (and HOME/cwd) — a separate lock would race against those.
    use crate::kms::test_env_lock as env_lock;

    #[test]
    fn inactive_by_default() {
        let _g = env_lock();
        std::env::remove_var(SHARED_DIR_ENV);
        assert!(!is_active());
        assert!(shared_agent_dir().is_none());
        assert!(!is_strict());
        assert!(!is_model_locked());
    }

    #[test]
    fn active_with_dir_and_flags() {
        let _g = env_lock();
        std::env::set_var(SHARED_DIR_ENV, "/shared-agent");
        std::env::set_var(STRICT_ENV, "1");
        std::env::set_var(MODEL_LOCKED_ENV, "true");
        assert!(is_active());
        assert_eq!(shared_agent_dir(), Some(PathBuf::from("/shared-agent")));
        assert_eq!(
            shared_agents_md(),
            Some(PathBuf::from("/shared-agent/AGENTS.md"))
        );
        assert_eq!(shared_kms_root(), Some(PathBuf::from("/shared-agent/kms")));
        assert!(is_strict());
        assert!(is_model_locked());
        // Flags are inert when shared mode is off.
        std::env::remove_var(SHARED_DIR_ENV);
        assert!(!is_strict());
        assert!(!is_model_locked());
        std::env::remove_var(STRICT_ENV);
        std::env::remove_var(MODEL_LOCKED_ENV);
    }

    #[test]
    fn empty_dir_is_inactive() {
        let _g = env_lock();
        std::env::set_var(SHARED_DIR_ENV, "   ");
        assert!(!is_active());
        std::env::remove_var(SHARED_DIR_ENV);
    }
}
