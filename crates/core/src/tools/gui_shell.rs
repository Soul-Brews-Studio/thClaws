//! GUI Shell authoring tools.
//!
//! Replace free-form `Write` into `.thclaws/gui-shell/<id>/` with
//! validated, path-jailed tools. All four are **gated** behind the
//! `gui-shell` gate (`Tool::requires_gate`) — they stay hidden from the
//! model until the `gui-shell` skill opens the gate, so the authoring
//! surface costs zero tokens until a user actually asks to build a shell.
//!
//! The folder contract is unchanged (the loader still scans the same
//! dirs), so a human can still hand-author a shell; these tools are just
//! the validated writer the model uses. See `dev-plan/43`.

use super::Tool;
use crate::error::{Error, Result};
use crate::gui_shell::{project_shell_dir, user_shell_dir, ShellManifest};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Register all GUI-shell authoring tools into a registry.
pub fn register(reg: &mut super::ToolRegistry) {
    reg.register(std::sync::Arc::new(GuiShellCreateTool));
    reg.register(std::sync::Arc::new(GuiShellWriteFileTool));
    reg.register(std::sync::Arc::new(GuiShellListTool));
    reg.register(std::sync::Arc::new(GuiShellRemoveTool));
}

const GATE: &str = "gui-shell";

/// Resolve a `scope` ("project" | "user") to its base shell directory.
/// Defaults to project scope.
fn base_dir(scope: Option<&str>) -> Result<PathBuf> {
    match scope.unwrap_or("project") {
        "project" => Ok(project_shell_dir()),
        "user" => Ok(user_shell_dir()),
        other => Err(Error::Tool(format!(
            "invalid scope '{other}': expected 'project' or 'user'"
        ))),
    }
}

/// Validate `id` as a single safe path segment and return the shell root.
fn shell_root(scope: Option<&str>, id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(Error::Tool(format!(
            "invalid shell id '{id}': must be a single path segment with no '/', '\\', or '..'"
        )));
    }
    Ok(base_dir(scope)?.join(id))
}

/// Join `rel` under `root`, rejecting any path that escapes the shell
/// folder (absolute, leading slash, or `..` component).
fn jail_join(root: &std::path::Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        return Err(Error::Tool("relpath must not be empty".into()));
    }
    let p = std::path::Path::new(rel);
    if p.is_absolute() || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(Error::Tool(format!("relpath '{rel}' must be relative")));
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::Tool(format!(
            "relpath '{rel}' must not contain '..'"
        )));
    }
    Ok(root.join(p))
}

fn opt_str<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(Value::as_str)
}

// ── Create ──────────────────────────────────────────────────────────

pub struct GuiShellCreateTool;

#[async_trait]
impl Tool for GuiShellCreateTool {
    fn name(&self) -> &'static str {
        "GuiShellCreate"
    }

    fn description(&self) -> &'static str {
        "Scaffold a new GUI Shell (a sandboxed HTML frontend that talks to \
         the agent via the `window.thclaws.*` bridge) into the shell folder, \
         with a VALIDATED manifest. Use this instead of writing manifest.json \
         + index.html by hand — it checks the id, permissions, and required \
         fields, then writes the folder atomically. After it returns, the user \
         opens the shell via '+ New Tab' → 'GUI Shell' → 'Refresh shells'. \
         Iterate on the HTML/CSS/JS afterwards with GuiShellWriteFile."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Lowercase kebab-case id, used as the folder name and bridge identity (e.g. 'image-gallery')." },
                "scope": { "type": "string", "enum": ["project", "user"], "description": "'project' → ./.thclaws/gui-shell/ (this repo only); 'user' → ~/.config/thclaws/gui-shell/ (all projects). Default 'project'." },
                "name": { "type": "string", "description": "Display name shown in the new-tab tile." },
                "description": { "type": "string", "description": "One-line summary." },
                "entry_html": { "type": "string", "description": "Full contents of the entry HTML file (index.html)." },
                "entry": { "type": "string", "description": "Entry filename. Default 'index.html'." },
                "permissions": { "type": "array", "items": { "type": "string" }, "description": "Bridge permissions: agent.run, session.read, session.list, fs.shell-scoped, tools.invoke:<tool>, network.outbound:<host>." },
                "files": { "type": "object", "description": "Optional extra files keyed by relative path → content (style.css, main.js, icon.svg, AGENTS.md)." },
                "version": { "type": "string", "description": "Semver. Default '0.1.0'." },
                "icon": { "type": "string", "description": "Optional icon filename (e.g. 'icon.svg'); include it in `files`." }
            },
            "required": ["id", "name", "description", "entry_html"]
        })
    }

    fn requires_gate(&self) -> Option<&'static str> {
        Some(GATE)
    }

    fn requires_approval(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value) -> Result<String> {
        let id = super::req_str(&input, "id")?.to_string();
        let scope = opt_str(&input, "scope");
        let entry = opt_str(&input, "entry").unwrap_or("index.html").to_string();
        let entry_html = super::req_str(&input, "entry_html")?.to_string();

        let permissions = input
            .get("permissions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let manifest = ShellManifest {
            id: id.clone(),
            name: super::req_str(&input, "name")?.to_string(),
            version: opt_str(&input, "version").unwrap_or("0.1.0").to_string(),
            description: super::req_str(&input, "description")?.to_string(),
            entry: entry.clone(),
            icon: opt_str(&input, "icon").map(String::from),
            min_bridge_version: "1".to_string(),
            permissions,
        };
        manifest.validate().map_err(Error::Tool)?;

        let root = shell_root(scope, &id)?;
        if root.exists() {
            return Err(Error::Tool(format!(
                "shell '{id}' already exists at {}. Edit it with GuiShellWriteFile, or GuiShellRemove first.",
                root.display()
            )));
        }
        std::fs::create_dir_all(&root)
            .map_err(|e| Error::Tool(format!("cannot create {}: {e}", root.display())))?;

        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| Error::Tool(format!("serialize manifest: {e}")))?;
        std::fs::write(root.join("manifest.json"), manifest_json)
            .map_err(|e| Error::Tool(format!("write manifest.json: {e}")))?;
        std::fs::write(root.join(&entry), entry_html)
            .map_err(|e| Error::Tool(format!("write {entry}: {e}")))?;

        let mut written = vec!["manifest.json".to_string(), entry.clone()];
        if let Some(files) = input.get("files").and_then(Value::as_object) {
            for (rel, content) in files {
                let dst = jail_join(&root, rel)?;
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        Error::Tool(format!("cannot create {}: {e}", parent.display()))
                    })?;
                }
                let body = content
                    .as_str()
                    .ok_or_else(|| Error::Tool(format!("files['{rel}'] must be a string")))?;
                std::fs::write(&dst, body).map_err(|e| Error::Tool(format!("write {rel}: {e}")))?;
                written.push(rel.clone());
            }
        }

        Ok(format!(
            "Created GUI Shell '{id}' at {}\nFiles: {}\nOpen it: '+ New Tab' → 'GUI Shell' → 'Refresh shells' → click the '{}' tile. Iterate with GuiShellWriteFile.",
            root.display(),
            written.join(", "),
            manifest.name,
        ))
    }
}

// ── WriteFile ───────────────────────────────────────────────────────

pub struct GuiShellWriteFileTool;

#[async_trait]
impl Tool for GuiShellWriteFileTool {
    fn name(&self) -> &'static str {
        "GuiShellWriteFile"
    }

    fn description(&self) -> &'static str {
        "Write or overwrite one file inside an existing GUI Shell's folder \
         (path-jailed — cannot escape the shell). Use to iterate on \
         index.html / style.css / main.js / AGENTS.md after GuiShellCreate. \
         To change the manifest, use GuiShellCreate (it validates); this tool \
         refuses manifest.json."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The shell id." },
                "scope": { "type": "string", "enum": ["project", "user"], "description": "Default 'project'." },
                "relpath": { "type": "string", "description": "File path relative to the shell folder (e.g. 'main.js', 'assets/app.css')." },
                "content": { "type": "string", "description": "Full file contents." }
            },
            "required": ["id", "relpath", "content"]
        })
    }

    fn requires_gate(&self) -> Option<&'static str> {
        Some(GATE)
    }

    fn requires_approval(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value) -> Result<String> {
        let id = super::req_str(&input, "id")?;
        let scope = opt_str(&input, "scope");
        let relpath = super::req_str(&input, "relpath")?;
        let content = super::req_str(&input, "content")?;

        if relpath == "manifest.json" {
            return Err(Error::Tool(
                "refusing to write manifest.json directly — use GuiShellCreate so the manifest is validated".into(),
            ));
        }

        let root = shell_root(scope, id)?;
        if !root.exists() {
            return Err(Error::Tool(format!(
                "shell '{id}' not found at {}. Create it with GuiShellCreate first.",
                root.display()
            )));
        }
        let dst = jail_join(&root, relpath)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Tool(format!("cannot create {}: {e}", parent.display())))?;
        }
        std::fs::write(&dst, content).map_err(|e| Error::Tool(format!("write {relpath}: {e}")))?;
        Ok(format!("Wrote {} ({} bytes)", dst.display(), content.len()))
    }
}

// ── List ────────────────────────────────────────────────────────────

pub struct GuiShellListTool;

#[async_trait]
impl Tool for GuiShellListTool {
    fn name(&self) -> &'static str {
        "GuiShellList"
    }

    fn description(&self) -> &'static str {
        "List installed GUI Shells (project + user scope) with their id, \
         name, and folder path."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn requires_gate(&self) -> Option<&'static str> {
        Some(GATE)
    }

    async fn call(&self, _input: Value) -> Result<String> {
        let mut out = String::new();
        for (scope, dir) in [("project", project_shell_dir()), ("user", user_shell_dir())] {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let manifest_path = path.join("manifest.json");
                if !manifest_path.exists() {
                    continue;
                }
                let label = std::fs::read_to_string(&manifest_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<ShellManifest>(&raw).ok())
                    .map(|m| format!("{} (id: {})", m.name, m.id))
                    .unwrap_or_else(|| {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("?")
                            .to_string()
                    });
                out.push_str(&format!("- [{scope}] {label} — {}\n", path.display()));
            }
        }
        if out.is_empty() {
            return Ok("No GUI Shells installed.".into());
        }
        Ok(out)
    }
}

// ── Remove ──────────────────────────────────────────────────────────

pub struct GuiShellRemoveTool;

#[async_trait]
impl Tool for GuiShellRemoveTool {
    fn name(&self) -> &'static str {
        "GuiShellRemove"
    }

    fn description(&self) -> &'static str {
        "Delete an installed GUI Shell's folder. Irreversible — requires \
         approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The shell id." },
                "scope": { "type": "string", "enum": ["project", "user"], "description": "Default 'project'." }
            },
            "required": ["id"]
        })
    }

    fn requires_gate(&self) -> Option<&'static str> {
        Some(GATE)
    }

    fn requires_approval(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value) -> Result<String> {
        let id = super::req_str(&input, "id")?;
        let scope = opt_str(&input, "scope");
        let root = shell_root(scope, id)?;
        if !root.exists() {
            return Err(Error::Tool(format!(
                "shell '{id}' not found at {}",
                root.display()
            )));
        }
        std::fs::remove_dir_all(&root)
            .map_err(|e| Error::Tool(format!("remove {}: {e}", root.display())))?;
        Ok(format!("Removed GUI Shell '{id}' ({})", root.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jail_join_rejects_escape() {
        let root = std::path::Path::new("/tmp/shell");
        assert!(jail_join(root, "../evil").is_err());
        assert!(jail_join(root, "/etc/passwd").is_err());
        assert!(jail_join(root, "a/../../b").is_err());
        assert!(jail_join(root, "sub/app.js").is_ok());
    }

    #[test]
    fn shell_root_rejects_bad_id() {
        assert!(shell_root(Some("project"), "../x").is_err());
        assert!(shell_root(Some("project"), "a/b").is_err());
        assert!(shell_root(Some("project"), "good-id").is_ok());
    }

    #[tokio::test]
    async fn create_then_list_then_remove() {
        // Isolate scope to a temp project dir via current_dir swap. cwd is
        // PROCESS-WIDE, so hold the suite's env lock across the swap — without
        // it this test moved the ground under any other test resolving a
        // relative path, which is how `tools::memory` (which does take the
        // lock) still failed at random.
        let _env = crate::kms::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("guishell-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let create = GuiShellCreateTool;
        let res = create
            .call(json!({
                "id": "demo-shell",
                "name": "Demo",
                "description": "test",
                "entry_html": "<h1>hi</h1>",
                "permissions": ["agent.run"]
            }))
            .await;
        assert!(res.is_ok(), "{res:?}");
        assert!(tmp
            .join(".thclaws/gui-shell/demo-shell/manifest.json")
            .exists());

        // Bad permission rejected.
        let bad = create
            .call(json!({
                "id": "bad-shell", "name": "x", "description": "x",
                "entry_html": "x", "permissions": ["filesystem.root"]
            }))
            .await;
        assert!(bad.is_err());

        std::env::set_current_dir(&prev).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
