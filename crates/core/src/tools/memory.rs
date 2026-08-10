//! Memory tools — the LLM-facing read/write/append surface for the
//! persistent memory directory.
//!
//! M6.26 BUG #1: pre-fix the LLM had NO memory tools at all. Memory was
//! auto-loaded into the system prompt every turn but was strictly
//! read-only — and even Write/Edit on `<project>/.thclaws/memory/`
//! was blocked by the sandbox. The user had to hand-edit `.md` files.
//!
//! Post-fix:
//! - `MemoryRead` — fetch a deferred entry (used when the system prompt
//!   marks an entry as `body deferred — call MemoryRead(...)`)
//! - `MemoryWrite` — create or replace an entry; auto-stamps frontmatter
//!   (`name`, `created`, `updated`); auto-updates `MEMORY.md`
//! - `MemoryAppend` — append to an entry; bumps `updated:`
//!
//! `MemoryWrite` and `MemoryAppend` deliberately bypass `Sandbox::check_write`
//! to land inside the resolved memory root. Path safety enforced via
//! `memory::writable_entry_path`. Same intentional carve-out pattern as
//! `TodoWrite` (`.thclaws/todos.md`) and `KmsWrite` (`.thclaws/kms/...`).

use super::{req_str, Tool};
use crate::error::{Error, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

fn resolve_store() -> Result<crate::memory::MemoryStore> {
    crate::memory::MemoryStore::default_path()
        .map(crate::memory::MemoryStore::new)
        .ok_or_else(|| Error::Tool("memory root unresolvable (missing $HOME)".into()))
}

/// Read one memory entry by name. Returns the full file contents
/// including any frontmatter — the model gets the same bytes the user
/// would see via `/memory read <name>`.
pub struct MemoryReadTool;

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &'static str {
        "MemoryRead"
    }

    fn description(&self) -> &'static str {
        "Read a single memory entry. Use when the system prompt marked \
         an entry as `body deferred` (the entry was elided from the prompt \
         to keep it under budget). Pass the entry name without `.md`."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Entry name (e.g. `user_role`); no `.md` extension"}
            },
            "required": ["name"]
        })
    }

    async fn call(&self, input: Value) -> Result<String> {
        let name = req_str(&input, "name")?;
        let store = resolve_store()?;
        match store.get(name) {
            Some(entry) => {
                // Reconstruct the on-disk shape so the model sees
                // frontmatter alongside body — matches what /memory
                // read shows.
                let mut out = String::new();
                if !entry.description.is_empty() || entry.memory_type.is_some() {
                    out.push_str("---\n");
                    if !entry.description.is_empty() {
                        out.push_str(&format!("description: {}\n", entry.description));
                    }
                    if let Some(ty) = &entry.memory_type {
                        out.push_str(&format!("type: {ty}\n"));
                    }
                    out.push_str("---\n");
                }
                out.push_str(&entry.body);
                Ok(out)
            }
            None => Err(Error::Tool(format!(
                "no memory entry named '{name}' (check the index in the system prompt)"
            ))),
        }
    }
}

/// Create or replace a memory entry. Content may begin with YAML
/// frontmatter (`---\n…\n---\n`); preserved on write. `created:` is
/// stamped on new entries; `updated:` is always stamped to today.
/// `MEMORY.md` index is auto-maintained.
pub struct MemoryWriteTool;

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &'static str {
        "MemoryWrite"
    }

    fn description(&self) -> &'static str {
        "Create or replace a persistent memory entry. Content may begin \
         with YAML frontmatter (---\\n description: ... \\n type: user|feedback|project|reference \\n category: ... \\n---\\n). \
         Use to file durable facts: user identity, feedback rules, project \
         context, references to external systems. Per the auto-memory \
         instructions, prefer one entry per discrete topic; use the \
         description field as a one-line hook the future-you will \
         recognize. The `MEMORY.md` index is auto-updated."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name":    {"type": "string", "description": "Entry name (file stem). [A-Za-z0-9_-]; no path separators or `..`."},
                "content": {"type": "string", "description": "Full entry content; may include YAML frontmatter."}
            },
            "required": ["name", "content"]
        })
    }

    fn requires_approval(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value) -> Result<String> {
        let name = req_str(&input, "name")?;
        let content = req_str(&input, "content")?;
        let store = resolve_store()?;
        let path = crate::memory::write_entry(&store, name, content)?;
        Ok(format!(
            "wrote memory `{name}` → {} ({} bytes)",
            path.display(),
            content.len()
        ))
    }
}

/// Append a chunk to a memory entry. If frontmatter is present,
/// `updated:` bumps; if not, plain append. If the entry doesn't exist,
/// creates it with bare body (no frontmatter — call `MemoryWrite` next
/// to add metadata). `MEMORY.md` index gets a bullet for new entries.
pub struct MemoryAppendTool;

#[async_trait]
impl Tool for MemoryAppendTool {
    fn name(&self) -> &'static str {
        "MemoryAppend"
    }

    fn description(&self) -> &'static str {
        "Append a chunk to a persistent memory entry (creates the entry \
         if it doesn't exist). Useful for accumulating rolling notes — \
         feedback observations, project-event log entries, reference \
         updates. For a fresh entry with frontmatter, use MemoryWrite \
         instead and pass the full content."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name":    {"type": "string", "description": "Entry name (file stem)"},
                "content": {"type": "string", "description": "Chunk to append"}
            },
            "required": ["name", "content"]
        })
    }

    fn requires_approval(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value) -> Result<String> {
        let name = req_str(&input, "name")?;
        let content = req_str(&input, "content")?;
        let store = resolve_store()?;
        let path = crate::memory::append_to_entry(&store, name, content)?;
        Ok(format!(
            "appended {} bytes to memory `{name}` → {}",
            content.len(),
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;

    /// Run `body` with memory resolving inside a private tempdir.
    ///
    /// The previous guard swapped process cwd *and* HOME. Both are
    /// process-global, so these tests were hostage to any other thread that
    /// touched either — `read_returns_frontmatter_and_body` failed at random
    /// for exactly that reason, and removing one offender in v0.110.0 only
    /// made it rarer (~1 full-suite run in 5 still lost it).
    ///
    /// `default_path()` prefers the task-local working dir that dev-plan/42
    /// added so tenants don't race over one process cwd. That is the same
    /// problem at a smaller scale, so use the same answer: a task-local
    /// cannot be moved by another thread, which makes these tests immune
    /// rather than merely lucky. It also drops the env lock — nothing here
    /// touches process state any more, so there is nothing to serialise.
    async fn with_memory_root<F: std::future::Future<Output = T>, T>(body: F) -> T {
        let dir = tempfile::tempdir().unwrap();
        // `.thclaws/` present ⇒ default_path() takes the project-scoped
        // branch and returns before it ever consults HOME.
        std::fs::create_dir_all(dir.path().join(".thclaws")).unwrap();
        crate::workdir::scope_workdir(dir.path().to_path_buf(), body).await
    }

    #[tokio::test]
    async fn write_creates_entry_and_updates_index() {
        with_memory_root(async {
            let store = MemoryStore::default_path().map(MemoryStore::new).unwrap();
            let result = MemoryWriteTool
                .call(json!({
                    "name": "user_role",
                    "content": "---\ndescription: senior backend engineer\ntype: user\n---\nLikes Rust."
                }))
                .await
                .unwrap();
            assert!(result.contains("wrote memory `user_role`"));
            // MEMORY.md got a bullet pointing at the new entry.
            let index = std::fs::read_to_string(store.root.join("MEMORY.md")).unwrap();
            assert!(index.contains("[user_role](user_role.md)"));
            assert!(index.contains("senior backend engineer"));
            // Entry on disk has frontmatter with auto-stamped `created:` + `updated:`.
            let entry = std::fs::read_to_string(store.root.join("user_role.md")).unwrap();
            assert!(entry.contains("description: senior backend engineer"));
            assert!(entry.contains("created:"));
            assert!(entry.contains("updated:"));
            assert!(entry.contains("Likes Rust."));
        })
        .await;
    }

    #[tokio::test]
    async fn write_replace_dedupes_index_bullet() {
        with_memory_root(async {
            let store = MemoryStore::default_path().map(MemoryStore::new).unwrap();
            MemoryWriteTool
                .call(json!({"name": "topic", "content": "---\ndescription: v1\n---\nbody"}))
                .await
                .unwrap();
            MemoryWriteTool
                .call(json!({"name": "topic", "content": "---\ndescription: v2\n---\nbody2"}))
                .await
                .unwrap();
            let index = std::fs::read_to_string(store.root.join("MEMORY.md")).unwrap();
            assert_eq!(index.matches("(topic.md)").count(), 1, "{index}");
            assert!(index.contains("v2"));
            assert!(!index.contains("v1"));
        })
        .await;
    }

    #[tokio::test]
    async fn write_rejects_traversal_and_reserved() {
        with_memory_root(async {
            let err = MemoryWriteTool
                .call(json!({"name": "../escape", "content": "evil"}))
                .await
                .unwrap_err();
            assert!(
                format!("{err}").contains("invalid memory name") || format!("{err}").contains("..")
            );
            // MEMORY is reserved (would clobber the index).
            let err2 = MemoryWriteTool
                .call(json!({"name": "MEMORY", "content": "x"}))
                .await
                .unwrap_err();
            assert!(format!("{err2}").contains("reserved"));
        })
        .await;
    }

    #[tokio::test]
    async fn append_creates_then_extends() {
        with_memory_root(async {
            let store = MemoryStore::default_path().map(MemoryStore::new).unwrap();
            // First call creates with bare body (no frontmatter).
            MemoryAppendTool
                .call(json!({"name": "rolling", "content": "first\n"}))
                .await
                .unwrap();
            // Bring frontmatter in via Write, then append again.
            MemoryWriteTool
                .call(
                    json!({"name": "rolling", "content": "---\ndescription: log\n---\nseed line\n"}),
                )
                .await
                .unwrap();
            MemoryAppendTool
                .call(json!({"name": "rolling", "content": "second\n"}))
                .await
                .unwrap();
            let raw = std::fs::read_to_string(store.root.join("rolling.md")).unwrap();
            assert!(raw.contains("seed line"));
            assert!(raw.contains("second"));
            // Frontmatter is preserved + `updated:` was bumped.
            assert!(raw.contains("description: log"));
            assert!(raw.contains("updated:"));
        })
        .await;
    }

    #[tokio::test]
    async fn read_unknown_entry_errors() {
        with_memory_root(async {
            let err = MemoryReadTool
                .call(json!({"name": "nope"}))
                .await
                .unwrap_err();
            assert!(format!("{err}").contains("no memory entry"));
        })
        .await;
    }

    #[tokio::test]
    async fn read_returns_frontmatter_and_body() {
        with_memory_root(async {
            MemoryWriteTool
                .call(json!({
                    "name": "foo",
                    "content": "---\ndescription: hook\ntype: user\n---\nbody"
                }))
                .await
                .unwrap();
            let out = MemoryReadTool.call(json!({"name": "foo"})).await.unwrap();
            assert!(out.contains("description: hook"));
            assert!(out.contains("type: user"));
            assert!(out.contains("body"));
        })
        .await;
    }

    #[test]
    fn write_and_append_require_approval() {
        assert!(MemoryWriteTool.requires_approval(&json!({})));
        assert!(MemoryAppendTool.requires_approval(&json!({})));
        // Read is non-destructive, no approval needed.
        assert!(!MemoryReadTool.requires_approval(&json!({})));
    }
}
