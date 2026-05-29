//! Pre-packaged schedule templates for common KMS-maintenance cadences.
//!
//! Inspired by obsidian-second-brain's four scheduled agents — nightly
//! close, weekly review, contradiction sweep, vault-health check. Each
//! preset bundles a cron expression with a prompt template that uses
//! `{kms}` substitution. `add_from_preset` instantiates a [`Schedule`]
//! from a preset + a target KMS name and persists it to the store.
//!
//! Adding a new preset is two lines in [`presets()`] — no scheduler
//! plumbing changes needed; the resulting [`Schedule`] flows through
//! the existing in-process scheduler / daemon paths.
//!
//! See `dev-log/152-scheduling-stack-m6-37.md` for the underlying
//! scheduler architecture.

use crate::error::{Error, Result};
use crate::schedule::{Schedule, ScheduleStore};
use std::path::{Path, PathBuf};

/// One preset — a named cron + prompt template pairing.
pub struct SchedulePreset {
    pub id: &'static str,
    pub description: &'static str,
    pub cron: &'static str,
    pub prompt_template: &'static str,
}

/// Registry of known presets. Add new entries here; the slash command
/// surface picks them up automatically through [`find`] and [`list_ids`].
///
/// **Important:** prompt templates are **natural-language directives**, not
/// slash commands. The scheduler fires these via `thclaws --print` which
/// does not run slash-command rewrite or dispatch — so `/kms wrap-up …`
/// would arrive at the LLM as literal text. Instead, each prompt instructs
/// the agent to use the registered KMS tools (KmsRead/KmsSearch/KmsWrite/
/// KmsAppend) directly. The scheduled invocation needs at least one KMS
/// in `kms_active` (otherwise the tools are absent from the registry —
/// the user is responsible for activating the target KMS in the cwd's
/// `.thclaws/settings.json`).
pub fn presets() -> &'static [SchedulePreset] {
    &[
        SchedulePreset {
            id: "nightly-close",
            description: "Wrap up the day — lint + auto-fix + stale-marker review (KMS '{kms}')",
            cron: "0 23 * * *",
            prompt_template:
                "Run a nightly maintenance pass on KMS '{kms}'. Pass `kms: \"{kms}\"` to every tool call. \
                 Walk pages/ via KmsRead/KmsSearch and fix:\n\
                 1. Broken markdown page links — `[text](pages/<stem>.md)` where `<stem>.md` doesn't exist. \
                 KmsSearch for the intended target stem; if exactly one strong match exists, KmsWrite the \
                 source page with the corrected link. Otherwise leave alone and report.\n\
                 2. Pages on disk missing from index.md — KmsAppend a one-line bullet to index.md under \
                 the matching `category:` section.\n\
                 3. STALE markers (`> ⚠ STALE: source ` followed by the alias) — KmsRead the source page \
                 (alias is the page stem), KmsWrite a refreshed body that drops the STALE line.\n\
                 Hard rules: don't invent sources; never use KmsDelete; every new page must reference at \
                 least one existing page via markdown link. End with `**Fixed**` / `**Skipped**` blocks.",
        },
        SchedulePreset {
            id: "weekly-review",
            description: "Sunday-morning consolidation across KMS '{kms}'",
            cron: "0 9 * * SUN",
            prompt_template:
                "Run the weekly review pass on KMS '{kms}'. Pass `kms: \"{kms}\"` to every tool call.\n\
                 Phase 1 — Consolidate. KmsRead the index page to enumerate pages. Use KmsSearch to find \
                 pages that overlap heavily on the same topic. If two pages clearly cover the same subject, \
                 merge them: KmsWrite the canonical page with combined content (preserve every claim and \
                 source URL). Leave the duplicate page in place with a `> See [primary](pages/<stem>.md).` \
                 pointer at the top — do NOT KmsDelete.\n\
                 Phase 2 — Hygiene. Find broken markdown page links and pages missing from index.md \
                 (same procedure as nightly-close).\n\
                 Hard rules: don't invent sources; never KmsDelete; preserve all source URLs and recency \
                 markers (e.g. `(as of 2026-04, …)`); every new page must reference at least one existing \
                 page. End with `**Consolidated**` / `**Fixed**` / `**Skipped**` blocks.",
        },
        SchedulePreset {
            id: "contradiction-sweep",
            description: "Daily noon reconcile — auto-resolve clear-winner contradictions in '{kms}'",
            cron: "0 12 * * *",
            prompt_template:
                "Run the contradiction sweep on KMS '{kms}'. Pass `kms: \"{kms}\"` to every tool call. \
                 Four passes:\n\
                 1. Claims — KmsSearch for facts that may conflict (numbers, dates, definitions). \
                 KmsRead candidate pages; identify pairs that disagree.\n\
                 2. Entities — KmsRead pages under entities/people sections; flag drifted roles/companies/titles.\n\
                 3. Decisions — find decision pages contradicted by later pages without an explicit \
                 `supersedes:` link.\n\
                 4. Source-freshness — pages citing old sources when newer sources on the same topic \
                 exist in the KMS.\n\
                 Per finding: clear-winner (newer + more authoritative) → KmsWrite the outdated page \
                 appending a `## History` section that documents the change with reason and source dates. \
                 Ambiguous → KmsWrite a new `Conflict — <topic>.md` with `status: open` and both positions, \
                 link the original conflicting pages via markdown links `[<label>](pages/<stem>.md)`. \
                 Evolution (user changed mind, not contradiction) → update with a `## Timeline` section.\n\
                 Hard rules: never silently delete a claim; recency markers + source URLs intact across \
                 rewrites; never use KmsDelete; never invent dates or sources. End with \
                 `**Auto-resolved**` / `**Flagged**` / `**Stale pages updated**` blocks.",
        },
        SchedulePreset {
            id: "vault-health",
            description: "Morning lint summary at 06:00 for KMS '{kms}'",
            cron: "0 6 * * *",
            prompt_template:
                "Run a read-only health check on KMS '{kms}'. Pass `kms: \"{kms}\"` to every tool call. \
                 Walk pages/ via KmsRead/KmsSearch and report counts + samples for each category:\n\
                 - Broken markdown page links: `[text](pages/<stem>.md)` where `pages/<stem>.md` doesn't exist\n\
                 - Orphan pages: page on disk with no inbound markdown link from any other page\n\
                 - Pages missing from index.md (page stem on disk but not listed in index)\n\
                 - Pages with no YAML frontmatter `---` block\n\
                 - Pages carrying STALE markers (`> ⚠ STALE: …`) awaiting refresh\n\
                 Read-only — do NOT call KmsWrite, KmsAppend, or KmsDelete. End with a totals table.",
        },
    ]
}

pub fn find(id: &str) -> Option<&'static SchedulePreset> {
    presets().iter().find(|p| p.id == id)
}

pub fn list_ids() -> Vec<&'static str> {
    presets().iter().map(|p| p.id).collect()
}

/// Substitute template variables. Currently only `{kms}` is supported.
pub fn render_prompt(preset: &SchedulePreset, kms: &str) -> String {
    preset.prompt_template.replace("{kms}", kms)
}

pub fn render_description(preset: &SchedulePreset, kms: &str) -> String {
    preset.description.replace("{kms}", kms)
}

/// Render the list of registered presets as the user-facing table
/// emitted by `/schedule preset list`. Three columns: ID / CRON /
/// DESCRIPTION (description shows the template's `{kms}` literally
/// since no KMS is bound at list time). Lives here, not in
/// `shell_dispatch`, because the CLI binary is built without the
/// `gui` feature gate. (M6.38.3 audit fix.)
pub fn format_preset_list() -> String {
    let presets = presets();
    if presets.is_empty() {
        return "no schedule presets registered".into();
    }
    let mut out = String::from("schedule presets:\n");
    out.push_str("  ID                     CRON           DESCRIPTION\n");
    for preset in presets {
        out.push_str(&format!(
            "  {:<22} {:<14} {}\n",
            preset.id, preset.cron, preset.description
        ));
    }
    out.push_str("\nadd via: /schedule preset add <id> --kms <name> [--cwd <path>]\n");
    out
}

/// Instantiate a preset for a specific KMS, persist it to the default
/// store (`~/.config/thclaws/schedules.json`), and return the resulting
/// `Schedule`. The schedule id is `<preset.id>-<kms>` so the same preset
/// can target multiple KMSes without collision.
///
/// Refuses with a clear error if a schedule with the same id already
/// exists — `ScheduleStore::add` enforces unique ids and the caller
/// should `rm` the existing entry first or pick a different KMS name.
pub fn add_from_preset(preset_id: &str, kms: &str, cwd: PathBuf) -> Result<Schedule> {
    add_from_preset_with_store(preset_id, kms, cwd, None)
}

/// Same as [`add_from_preset`] but allows passing an explicit `store_path`
/// for tests. `None` falls back to the default store path. Mirrors the
/// `Option<&Path>` test-isolation pattern from `schedule::run_once_with`.
pub fn add_from_preset_with_store(
    preset_id: &str,
    kms: &str,
    cwd: PathBuf,
    store_path: Option<&Path>,
) -> Result<Schedule> {
    let preset = find(preset_id).ok_or_else(|| {
        Error::Tool(format!(
            "unknown preset '{preset_id}' — try one of: {}",
            list_ids().join(", ")
        ))
    })?;
    if kms.is_empty() {
        return Err(Error::Tool(
            "preset requires a KMS name — preset prompts substitute {kms}".into(),
        ));
    }
    let schedule = Schedule {
        id: format!("{}-{kms}", preset.id),
        cron: preset.cron.into(),
        run_at: None,
        cwd,
        prompt: render_prompt(preset, kms),
        model: None,
        max_iterations: None,
        timeout_secs: None,
        enabled: true,
        watch_workspace: false,
        last_run: None,
        last_exit: None,
    };
    let mut store = match store_path {
        Some(p) => ScheduleStore::load_from(p)?,
        None => ScheduleStore::load()?,
    };
    store.add(schedule.clone())?;
    match store_path {
        Some(p) => store.save_to(p)?,
        None => store.save()?,
    }
    Ok(schedule)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_have_validatable_cron() {
        for preset in presets() {
            assert!(
                crate::schedule::validate_cron(preset.cron).is_ok(),
                "preset '{}' has invalid cron '{}'",
                preset.id,
                preset.cron
            );
        }
    }

    #[test]
    fn render_prompt_substitutes_kms() {
        let p = find("nightly-close").unwrap();
        let rendered = render_prompt(p, "mynotes");
        // M6.38.1 — preset prompts are natural-language directives now.
        // The KMS name appears in the `kms: "..."` tool-arg directive
        // and in the human-readable "KMS '...'" reference.
        assert!(rendered.contains("KMS 'mynotes'"));
        assert!(rendered.contains("kms: \"mynotes\""));
        assert!(!rendered.contains("{kms}"));
    }

    #[test]
    fn render_prompt_substitutes_multiple_occurrences() {
        // weekly-review references {kms} in multiple sentences;
        // ensure all occurrences substitute.
        let p = find("weekly-review").unwrap();
        let rendered = render_prompt(p, "notes");
        assert!(rendered.contains("KMS 'notes'"));
        assert!(rendered.contains("kms: \"notes\""));
        assert!(!rendered.contains("{kms}"));
    }

    #[test]
    fn presets_are_natural_language_not_slash_commands() {
        // Bug #1 regression guard: the scheduler fires `thclaws --print`
        // which does NOT process slash commands. Prompts must instruct
        // the agent via natural language + tool directives, not /kms or
        // /dream prefixes.
        for preset in presets() {
            let rendered = render_prompt(preset, "k");
            // None of the prompt should be a slash-command line.
            for line in rendered.lines() {
                let trimmed = line.trim_start();
                assert!(
                    !trimmed.starts_with("/kms ") && !trimmed.starts_with("/dream"),
                    "preset '{}' contains a slash-command line: {trimmed:?}",
                    preset.id
                );
            }
            // Every preset references `kms: "<name>"` (the tool-arg
            // hint that's our convention).
            assert!(
                rendered.contains("kms: \"k\""),
                "preset '{}' missing `kms: \"<name>\"` directive",
                preset.id
            );
        }
    }

    #[test]
    fn find_returns_none_for_unknown_id() {
        assert!(find("does-not-exist").is_none());
    }

    #[test]
    fn list_ids_includes_all_presets() {
        let ids = list_ids();
        assert!(ids.contains(&"nightly-close"));
        assert!(ids.contains(&"weekly-review"));
        assert!(ids.contains(&"contradiction-sweep"));
        assert!(ids.contains(&"vault-health"));
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn add_from_preset_rejects_unknown() {
        let err = add_from_preset("nope", "k", std::env::temp_dir()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown preset"));
        assert!(msg.contains("nightly-close")); // hint lists the real ones
    }

    #[test]
    fn add_from_preset_rejects_empty_kms() {
        let err = add_from_preset("nightly-close", "", std::env::temp_dir()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("KMS name"));
    }

    #[test]
    fn add_from_preset_with_store_round_trips_to_disk() {
        // Happy-path test using a tempdir-backed store so the user's
        // real ~/.config/thclaws/schedules.json is untouched.
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("schedules.json");
        let cwd = dir.path().to_path_buf();

        let schedule =
            add_from_preset_with_store("nightly-close", "mynotes", cwd.clone(), Some(&store_path))
                .unwrap();

        // ID format is `<preset.id>-<kms>`.
        assert_eq!(schedule.id, "nightly-close-mynotes");
        assert_eq!(schedule.cron, "0 23 * * *");
        assert_eq!(schedule.cwd, cwd);
        assert!(schedule.enabled);
        // Prompt has been rendered (no template variable left).
        assert!(schedule.prompt.contains("KMS 'mynotes'"));
        assert!(!schedule.prompt.contains("{kms}"));

        // Store was actually written and can be re-read.
        let store = ScheduleStore::load_from(&store_path).unwrap();
        assert_eq!(store.schedules.len(), 1);
        assert_eq!(store.schedules[0].id, "nightly-close-mynotes");
    }

    #[test]
    fn add_from_preset_with_store_assigns_unique_ids_per_kms() {
        // Same preset for two different KMSes must produce distinct ids.
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("schedules.json");
        let cwd = dir.path().to_path_buf();

        add_from_preset_with_store("vault-health", "notes-a", cwd.clone(), Some(&store_path))
            .unwrap();
        add_from_preset_with_store("vault-health", "notes-b", cwd.clone(), Some(&store_path))
            .unwrap();

        let store = ScheduleStore::load_from(&store_path).unwrap();
        let ids: Vec<&str> = store.schedules.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"vault-health-notes-a"));
        assert!(ids.contains(&"vault-health-notes-b"));
        assert_eq!(store.schedules.len(), 2);
    }

    #[test]
    fn add_from_preset_with_store_rejects_duplicate_id() {
        // Adding the same preset+KMS twice should fail on the second
        // call (ScheduleStore::add enforces unique ids).
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("schedules.json");
        let cwd = dir.path().to_path_buf();

        add_from_preset_with_store("nightly-close", "notes", cwd.clone(), Some(&store_path))
            .unwrap();
        let err =
            add_from_preset_with_store("nightly-close", "notes", cwd.clone(), Some(&store_path))
                .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("nightly-close-notes") || msg.contains("already exists"),
            "expected duplicate-id error, got: {msg}"
        );
    }
}
