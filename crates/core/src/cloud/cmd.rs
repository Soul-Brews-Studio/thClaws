//! CLI subcommand handlers — wired from `bin/app.rs`.

use std::path::{Path, PathBuf};

use crate::cloud::{client::Client, pack, resolve_cloud_url, wssync, CloudConfig};

pub async fn login(
    cloud_url: Option<&str>,
    token: Option<String>,
    cloud_cfg: Option<&CloudConfig>,
) -> Result<(), String> {
    let url = resolve_cloud_url(cloud_url, cloud_cfg);
    let token = match token {
        Some(t) => t.trim().to_string(),
        None => prompt_token()?,
    };
    if !token.starts_with("thc_") {
        return Err(
            "expected token to start with 'thc_' — get one from the dashboard at /dashboard".into(),
        );
    }
    let client = Client::new(&url, Some(token.clone()));
    let me = client.me().await?;
    crate::cloud::set_token(&token).map_err(|e| format!("save token: {}", e))?;
    // Persist the URL too so subsequent CLI calls don't need --cloud-url.
    // Same field the GUI Settings → Cloud section writes.
    let mut project = crate::config::ProjectConfig::load().unwrap_or_default();
    project.set_cloud_url(Some(&url));
    if let Err(e) = project.save() {
        eprintln!("  warning: couldn't persist URL to settings.json: {}", e);
    }
    eprintln!("✓ Signed in to {} as {}", url, me.email);
    if me.can_publish {
        eprintln!("  Publishing enabled.");
    } else {
        eprintln!("  Publishing not enabled for this account.");
    }
    Ok(())
}

fn prompt_token() -> Result<String, String> {
    use std::io::{BufRead, Write};
    eprint!("Paste CLI token (from /dashboard): ");
    std::io::stderr().flush().ok();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("stdin: {}", e))?;
    let t = line.trim().to_string();
    if t.is_empty() {
        return Err("no token entered".into());
    }
    Ok(t)
}

pub fn logout() -> Result<(), String> {
    crate::cloud::clear_token().map_err(|e| format!("clear token: {}", e))?;
    eprintln!("✓ Signed out");
    Ok(())
}

/// Print where the CLI currently thinks the catalog lives and whether
/// it has credentials. Mirrors the Settings → Cloud panel so users can
/// confirm CLI-side state without opening the GUI.
pub fn status(cloud_url: Option<&str>, cloud_cfg: Option<&CloudConfig>) -> Result<(), String> {
    for line in status_lines(cloud_url, cloud_cfg) {
        eprintln!("{line}");
    }
    Ok(())
}

/// Return the same lines `status()` prints, but as a `Vec<String>` so
/// the REPL / GUI slash-command dispatchers can route them through
/// their own output channels (`println!` vs `ViewEvent::SlashOutput`).
pub fn status_lines(cloud_url: Option<&str>, cloud_cfg: Option<&CloudConfig>) -> Vec<String> {
    let url = resolve_cloud_url(cloud_url, cloud_cfg);
    let has_token = crate::cloud::token().is_some();
    let agent = crate::config::ProjectConfig::load().and_then(|c| c.agent.clone());
    let mut lines = vec![
        format!("Cloud URL: {url}"),
        format!("Token:     {}", if has_token { "set" } else { "(none)" }),
    ];
    match agent {
        Some(a) => {
            lines.push(format!(
                "Agent:     {} ({})",
                a.name.as_deref().unwrap_or("(unnamed)"),
                a.id.as_deref().unwrap_or("?")
            ));
            lines.push(format!(
                "UUID:      {}",
                a.uuid
                    .as_deref()
                    .map(|u| format!("{u} (bound)"))
                    .unwrap_or_else(|| "(unbound — next publish creates new entry)".to_string())
            ));
        }
        None => {
            lines.push("Agent:     (no settings.json::agent block in this folder)".to_string());
        }
    }
    lines
}

/// Hit the catalog and return the lines the slash dispatchers (REPL +
/// GUI) print. Errors surface as a single line so both surfaces render
/// identically.
pub async fn list_lines(
    mine: bool,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
) -> Vec<String> {
    let url = resolve_cloud_url(cloud_url, cloud_cfg);
    let token = crate::cloud::token();
    let client = Client::new(&url, token);
    match client.list_agents(mine).await {
        Ok(agents) if agents.is_empty() => vec!["(no agents in catalog)".to_string()],
        Ok(agents) => agents
            .into_iter()
            .map(|a| {
                format!(
                    "{:30}  v{:<10}  {}",
                    a.slug,
                    a.current_version.unwrap_or_default(),
                    a.name
                )
            })
            .collect(),
        Err(e) => vec![format!("/cloud list: {e}")],
    }
}

/// `/cloud publish` from inside a session — packs cwd + uploads.
/// Returns ordered progress lines (including any error). Mirrors
/// [`get_into_cwd_lines`].
pub async fn publish_cwd_lines(
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
) -> Vec<String> {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return vec![format!("/cloud publish: can't read cwd: {e}")],
    };
    let mut lines: Vec<String> = Vec::new();
    if let Err(e) = publish_inner(cwd, cloud_url, false, cloud_cfg, &mut lines).await {
        lines.push(format!("/cloud publish: {e}"));
    }
    lines
}

pub async fn publish(
    path: PathBuf,
    cloud_url: Option<&str>,
    dry_run: bool,
    cloud_cfg: Option<&CloudConfig>,
) -> Result<(), String> {
    // CLI-facing thin wrapper: mirror the slash-friendly inner impl
    // and dump its lines to stderr so terminal output matches the old
    // eprintln shape exactly.
    let mut lines = Vec::new();
    let result = publish_inner(path, cloud_url, dry_run, cloud_cfg, &mut lines).await;
    for ln in &lines {
        eprintln!("{ln}");
    }
    result
}

async fn publish_inner(
    path: PathBuf,
    cloud_url: Option<&str>,
    dry_run: bool,
    cloud_cfg: Option<&CloudConfig>,
    log: &mut Vec<String>,
) -> Result<(), String> {
    let url = resolve_cloud_url(cloud_url, cloud_cfg);
    let token = crate::cloud::token();

    // Load this folder's project settings so we can read agent identity
    // + write the UUID back after the server assigns one. We deliberately
    // chdir-style here: ProjectConfig::load reads from cwd, so we cd
    // into the agent folder for the duration of the call. (We restore
    // cwd at the end so the caller's environment is unchanged.)
    let prior_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(&path).map_err(|e| format!("entering {}: {}", path.display(), e))?;
    let _restore = scopeguard_chdir(prior_cwd);

    let mut project = crate::config::ProjectConfig::load().unwrap_or_default();
    let agent = ensure_agent_identity(&mut project, &path)?;

    let fused =
        crate::cloud::manifest::Manifest::fuse_for_publish(&agent, &path.join("manifest.json"))?;
    let fused_json =
        serde_json::to_vec_pretty(&fused).map_err(|e| format!("serialize fused manifest: {e}"))?;

    log.push(format!("Packing {} …", path.display()));
    let result = pack::pack(&path, Some(&fused_json))?;
    log.push(format!(
        "  Included {} file(s), stripped {} file(s), {:.1} KB",
        result.included.len(),
        result.stripped.len(),
        result.bytes.len() as f64 / 1024.0
    ));
    if !result.stripped.is_empty() {
        log.push("  Stripped (showing first 10):".to_string());
        for s in result.stripped.iter().take(10) {
            log.push(format!("    - {}", s));
        }
    }
    if agent.uuid.is_some() {
        log.push(format!(
            "  Publishing as existing agent (uuid: {}…)",
            &agent
                .uuid
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(8)
                .collect::<String>()
        ));
    } else {
        log.push("  First publish — server will assign a UUID.".to_string());
    }
    if dry_run {
        log.push("Dry run — not uploading.".to_string());
        return Ok(());
    }

    if token.is_none() {
        return Err(
            "not logged in — paste your CLI token in Settings → thClaws.cloud (mint one at /dashboard)".into()
        );
    }

    log.push(format!("Uploading to {} …", url));
    let client = Client::new(&url, token);
    let resp = client.publish(result.bytes).await?;
    log.push(format!(
        "✓ Published {} v{} ({} bytes)",
        resp.slug, resp.version, resp.size_bytes
    ));
    log.push(format!("  {}", resp.url));

    // Write the assigned UUID back to settings.json so re-publish from
    // this folder targets the same catalog entry.
    if agent.uuid.as_deref() != Some(resp.uuid.as_str()) {
        project.merge_agent(crate::config::AgentConfig {
            uuid: Some(resp.uuid.clone()),
            ..Default::default()
        });
        project
            .save()
            .map_err(|e| format!("write settings.json: {e}"))?;
        log.push(format!(
            "  settings.json::agent.uuid updated → {}…",
            resp.uuid.chars().take(8).collect::<String>()
        ));
    }
    Ok(())
}

/// Settings-or-manifest helper: pull the agent identity to use for
/// `cloud publish`. If `settings.json::agent` is populated, use it. If
/// it's missing but the legacy `manifest.json` carries id/name/
/// description (pre-Option-A folders), auto-migrate to settings.json
/// and emit a one-line notice. Returns the resolved identity.
fn ensure_agent_identity(
    project: &mut crate::config::ProjectConfig,
    folder: &Path,
) -> Result<crate::config::AgentConfig, String> {
    if let Some(existing) = project.agent.as_ref() {
        if existing.id.is_some() && existing.name.is_some() && existing.description.is_some() {
            return Ok(existing.clone());
        }
    }

    // Try to read identity from legacy manifest.json.
    let manifest_path = folder.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "no settings.json::agent block and can't read manifest.json: {e}\n\
             — add an [agent] section to ./.thclaws/settings.json with id/name/description"
        )
    })?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("manifest.json: {e}"))?;
    let id = v.get("id").and_then(|x| x.as_str()).map(String::from);
    let name = v.get("name").and_then(|x| x.as_str()).map(String::from);
    let description = v
        .get("description")
        .and_then(|x| x.as_str())
        .map(String::from);
    if id.is_none() || name.is_none() || description.is_none() {
        return Err(
            "settings.json::agent.{id,name,description} required for publish (none of these \
             could be derived from manifest.json either)"
                .into(),
        );
    }
    eprintln!("  Migrating identity from manifest.json → settings.json::agent");
    project.merge_agent(crate::config::AgentConfig {
        id,
        name,
        description,
        uuid: None,
    });
    project
        .save()
        .map_err(|e| format!("write settings.json: {e}"))?;
    Ok(project.agent.clone().unwrap())
}

/// Restore cwd on drop. Used by `publish` so the caller's environment
/// is unchanged after the publish call returns.
fn scopeguard_chdir(prior: Option<PathBuf>) -> impl Drop {
    struct Guard(Option<PathBuf>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(p) = self.0.take() {
                let _ = std::env::set_current_dir(p);
            }
        }
    }
    Guard(prior)
}

pub fn unbind() -> Result<(), String> {
    for ln in unbind_lines() {
        eprintln!("{ln}");
    }
    Ok(())
}

/// `/cloud unbind` from inside a session. Same logic as [`unbind`]
/// but returns lines for the SlashOutput stream instead of eprintln.
/// `/cloud unbind` — blank the folder's bound agent UUID. This is the single
/// "detach" operation and it serves both intents: afterwards a DIFFERENT agent
/// can be `/cloud get`'d over this folder (the get guard treats an unbound
/// folder as free — see the uuid check in `get_lines`), AND a `/cloud publish`
/// with no bound uuid registers a NEW catalog entry (the backend mints a fresh
/// uuid and writes it back) instead of trying to update the original — i.e. a
/// fork. No separate `/cloud fork` needed; unbind + publish is the fork flow.
pub fn unbind_lines() -> Vec<String> {
    let mut project = crate::config::ProjectConfig::load().unwrap_or_default();
    let prior = project
        .agent
        .as_ref()
        .and_then(|a| a.uuid.clone())
        .unwrap_or_default();
    if prior.is_empty() {
        return vec![
            "Already unbound — `/cloud get <slug>` can replace this folder, or edit + `/cloud publish` for a new entry.".to_string(),
        ];
    }
    project.clear_agent_uuid();
    if let Err(e) = project.save() {
        return vec![format!("/cloud unbind: write settings.json: {e}")];
    }
    vec![format!(
        "✓ Detached agent {}… — folder is now unbound. `/cloud get <slug>` can replace it \
         with a different agent, or edit + `/cloud publish` to fork it into a new catalog entry.",
        prior.chars().take(8).collect::<String>()
    )]
}

pub async fn get(
    slug: String,
    target: PathBuf,
    version: Option<String>,
    force: bool,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
) -> Result<(), String> {
    for line in get_lines(slug, target, version, force, cloud_url, cloud_cfg).await {
        eprintln!("{line}");
    }
    Ok(())
}

/// `cloud get <slug>` into the caller's cwd with the safety check the
/// slash command needs:
///   - empty cwd → extract fresh
///   - non-empty cwd + matching agent UUID → extract over (safe update)
///   - non-empty cwd + UUID mismatch or no UUID → abort
///
/// No `--force` — for that, use the CLI's `cloud get <slug> <target> --force`.
pub async fn get_into_cwd_lines(
    slug: String,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
) -> Vec<String> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return vec![format!("/cloud get: can't read cwd: {e}")],
    };
    get_lines(slug, cwd, None, /*force=*/ false, cloud_url, cloud_cfg).await
}

/// Underlying get-and-report. Errors come back as a single line so
/// both surfaces (CLI eprintln, GUI/REPL SlashOutput) render identically.
/// `force` bypasses the UUID-match safety check on non-empty targets.
async fn get_lines(
    slug: String,
    target: PathBuf,
    version: Option<String>,
    force: bool,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
) -> Vec<String> {
    let url = resolve_cloud_url(cloud_url, cloud_cfg);
    let token = crate::cloud::token();
    if token.is_none() {
        return vec![
            "/cloud get: not logged in — paste your CLI token in Settings → thClaws.cloud (mint one at /dashboard)"
                .to_string(),
        ];
    }

    let mut lines = Vec::new();
    // "Has agent content" — not just "is non-empty". REPL startup
    // auto-bootstraps a placeholder .thclaws/settings.json in cwd
    // (via ProjectConfig::ensure_default_exists), which would make
    // a genuinely-fresh folder look non-empty. The real signal that
    // an agent already lives here is AGENTS.md or manifest.json.
    let has_agent_content =
        target.join("AGENTS.md").exists() || target.join("manifest.json").exists();

    lines.push(format!("Downloading {} …", slug));
    let client = Client::new(&url, token);
    let dl = match client.download(&slug, version.as_deref()).await {
        Ok(d) => d,
        Err(e) => {
            lines.push(format!("/cloud get: {e}"));
            return lines;
        }
    };
    lines.push(format!(
        "  v{} ({:.1} KB, sha256 {}…)",
        dl.version,
        dl.bytes.len() as f64 / 1024.0,
        &dl.sha256.chars().take(12).collect::<String>()
    ));

    if !dl.sha256.is_empty() {
        if let Err(e) = pack::verify_sha256(&dl.bytes, &dl.sha256) {
            lines.push(format!("/cloud get: {e}"));
            return lines;
        }
    }

    // Safety check on folders that already hold an agent: refuse unless
    // the bound agent UUID matches what we just downloaded. `--force`
    // (CLI-only) bypasses.
    if has_agent_content && !force {
        let server_uuid = match dl.uuid.as_deref() {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => {
                lines.push(
                    "/cloud get: server didn't return an X-Agent-UUID header — refusing to \
                     overwrite an existing agent folder. (Catalog backend probably needs an update.)"
                        .into(),
                );
                return lines;
            }
        };
        let local_uuid = load_local_agent_uuid(&target);
        match local_uuid.as_deref() {
            Some(local) if local == server_uuid => {
                lines.push(format!(
                    "  Folder matches agent UUID {}… — overwriting in-place.",
                    server_uuid.chars().take(8).collect::<String>()
                ));
            }
            Some(local) => {
                lines.push(format!(
                    "/cloud get: refusing to overwrite. This folder is bound to agent {}…, but \
                     the downloaded agent is {}…. To replace this folder with the downloaded \
                     agent, run /cloud unbind first OR cd to an empty directory.",
                    local.chars().take(8).collect::<String>(),
                    server_uuid.chars().take(8).collect::<String>()
                ));
                return lines;
            }
            None => {
                // No bound UUID = no published agent to protect. The folder was
                // either explicitly detached (`/cloud unbind`, which blanks the
                // uuid) or hand-assembled; either way an explicit `/cloud get`
                // is a clear intent to install here, so replace it. (The guard
                // exists to stop clobbering a DIFFERENT bound agent — case 2 —
                // not an unbound folder.)
                lines.push(
                    "  Folder is unbound (no agent UUID) — replacing it with the downloaded agent."
                        .into(),
                );
            }
        }
    }

    // Snapshot installer-owned settings before the overwrite. `unpack`
    // (force=true) replaces the agent's `.thclaws/settings.json` wholesale,
    // which would wipe local session/account config the agent has no
    // business carrying — notably `gatewayProxy`. Losing it drops the user
    // off the gateway, and the next agent rebuild then fails with a
    // misleading "no API key found for provider 'anthropic'". These keys
    // are carried forward after extraction (see `restore_installer_settings`).
    let prior_settings = std::fs::read(target.join(".thclaws").join("settings.json")).ok();

    lines.push(format!("Extracting to {} …", target.display()));
    // After the UUID match (or empty target, or --force) we always
    // allow overwrite — pack::unpack's per-file refusal is bypassed
    // because the safety gate already lives above.
    let files = match pack::unpack(&dl.bytes, &target, /*force=*/ true) {
        Ok(f) => f,
        Err(e) => {
            lines.push(format!("/cloud get: {e}"));
            return lines;
        }
    };
    lines.push(format!("✓ Extracted {} file(s)", files.len()));

    let manifest_path = target.join("manifest.json");
    if let Ok(m) = crate::cloud::manifest::Manifest::from_path(&manifest_path) {
        if let Err(e) = split_unified_manifest(&target, &m, dl.uuid.as_deref()) {
            lines.push(format!(
                "  warning: couldn't split manifest into settings.json: {e}"
            ));
        }
        for line in post_install_hint_lines(&m, &target) {
            lines.push(line);
        }
    }

    // Carry installer-owned keys (gatewayProxy, …) back over the extracted
    // settings.json so the install doesn't knock the user off the gateway.
    if let Some(prior) = prior_settings {
        if let Err(e) = restore_installer_settings(&target, &prior) {
            lines.push(format!(
                "  warning: couldn't preserve local settings ({e}) — \
                 re-enable the gateway in Settings → thClaws.cloud if needed"
            ));
        }
    }
    lines
}

/// Keys in `.thclaws/settings.json` that belong to the installing user's
/// session/account, not to the agent being installed. `/cloud get`
/// overwrites the whole file from the tarball, so these are snapshotted
/// before extraction and carried forward after — without this, an install
/// silently wipes the user's gateway routing (`gatewayProxy`) and cloud URL,
/// which surfaces as a misleading "no API key found for provider 'anthropic'"
/// on the next agent rebuild.
///
/// `model` is here too: `/model` persists the user's pick to the project
/// settings, and losing it on install meant the post-get `/reload` silently
/// fell back to the global default. A bundle that explicitly ships `model`
/// (a publisher pin) still wins — restore only fills keys the bundle left
/// unset.
const INSTALLER_OWNED_SETTINGS_KEYS: &[&str] =
    &["gatewayProxy", "gateway_use_for", "cloudUrl", "model"];

/// Restore installer-owned keys from `prior_raw` onto the freshly-extracted
/// `.thclaws/settings.json`. Only fills keys the agent's bundle did NOT set,
/// so a publisher that legitimately ships one of these still wins.
fn restore_installer_settings(target: &Path, prior_raw: &[u8]) -> Result<(), String> {
    let prior: serde_json::Value =
        serde_json::from_slice(prior_raw).map_err(|e| format!("parse prior settings.json: {e}"))?;
    let Some(prior_obj) = prior.as_object() else {
        return Ok(());
    };

    let settings_path = target.join(".thclaws").join("settings.json");
    let mut cur: serde_json::Value = match std::fs::read(&settings_path) {
        Ok(raw) => serde_json::from_slice(&raw).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };
    let Some(cur_obj) = cur.as_object_mut() else {
        return Ok(());
    };

    let mut restored = false;
    for k in INSTALLER_OWNED_SETTINGS_KEYS {
        if !cur_obj.contains_key(*k) {
            if let Some(v) = prior_obj.get(*k) {
                cur_obj.insert((*k).to_string(), v.clone());
                restored = true;
            }
        }
    }
    if !restored {
        return Ok(());
    }
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&cur).map_err(|e| format!("serialize settings.json: {e}"))?,
    )
    .map_err(|e| format!("write settings.json: {e}"))
}

/// Read just `<target>/.thclaws/settings.json::agent.uuid` without
/// touching the rest of project config.
fn load_local_agent_uuid(target: &Path) -> Option<String> {
    let prior = std::env::current_dir().ok();
    if std::env::set_current_dir(target).is_err() {
        return None;
    }
    let uuid = crate::config::ProjectConfig::load()
        .and_then(|c| c.agent)
        .and_then(|a| a.uuid);
    if let Some(p) = prior {
        let _ = std::env::set_current_dir(p);
    }
    uuid
}

fn split_unified_manifest(
    target: &Path,
    manifest: &crate::cloud::manifest::Manifest,
    server_uuid: Option<&str>,
) -> Result<(), String> {
    let prior_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(target)
        .map_err(|e| format!("entering {}: {}", target.display(), e))?;
    let _restore = scopeguard_chdir(prior_cwd);

    // Identity AND UUID travel with the package. Preserving the UUID
    // makes "re-get into the same folder" act as an update (same agent
    // → CLI overwrites in place). Fork-safety is enforced server-side:
    // if the recipient tries to `cloud publish`, the server checks
    // UUID ownership and 403s with a clear "run cloud unbind to fork".
    // UUID precedence: server X-Agent-UUID header (authoritative) >
    // manifest.uuid inside the tarball (may be stale or absent).
    let resolved_uuid = server_uuid
        .map(|s| s.to_string())
        .or_else(|| manifest.uuid.clone());
    let agent_block = serde_json::json!({
        "id": manifest.id,
        "name": manifest.name,
        "description": manifest.description,
        "uuid": resolved_uuid,
    });
    // Direct JSON-level merge — set just the `agent` key, preserve
    // everything else the tarball shipped (guiShell, model, etc.).
    // Going through ProjectConfig::save() would also write every
    // Option<bool> default-false field (shellTabEnabled, teamEnabled,
    // …), bloating the installer's settings.json with noise.
    let settings_path = std::path::Path::new(".thclaws").join("settings.json");
    let mut existing: serde_json::Value = match std::fs::read(&settings_path) {
        Ok(raw) => serde_json::from_slice(&raw).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };
    if let Some(obj) = existing.as_object_mut() {
        obj.insert("agent".to_string(), agent_block);
    }
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&existing)
            .map_err(|e| format!("serialize settings.json: {e}"))?,
    )
    .map_err(|e| format!("write settings.json: {e}"))?;

    // Strip identity fields from the on-disk manifest.json so the local
    // source of truth is unambiguous (settings.json::agent).
    let manifest_path = target.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
    if let Some(obj) = v.as_object_mut() {
        for k in ["id", "name", "description", "uuid", "author"] {
            obj.remove(k);
        }
    }
    let slim =
        serde_json::to_string_pretty(&v).map_err(|e| format!("serialize slim manifest: {e}"))?;
    std::fs::write(&manifest_path, slim)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;
    Ok(())
}

pub async fn list(
    mine: bool,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
) -> Result<(), String> {
    let url = resolve_cloud_url(cloud_url, cloud_cfg);
    let token = crate::cloud::token();
    let client = Client::new(&url, token);
    let agents = client.list_agents(mine).await?;
    if agents.is_empty() {
        eprintln!("(no agents)");
        return Ok(());
    }
    for a in agents {
        println!(
            "{:30}  v{:<10}  {}",
            a.slug,
            a.current_version.unwrap_or_default(),
            a.name
        );
    }
    Ok(())
}

fn post_install_hint_lines(m: &crate::cloud::manifest::Manifest, target: &Path) -> Vec<String> {
    let mut lines = vec![
        String::new(),
        format!("Installed: {} v{}", m.name, m.version),
        format!("  cd {}", target.display()),
    ];
    if !m.requires.provider_keys.is_empty() {
        lines.push(String::new());
        lines.push("This agent expects these provider keys in .env:".to_string());
        for k in &m.requires.provider_keys {
            let mark = if k.required { "*" } else { " " };
            let purpose = k.purpose.as_deref().unwrap_or("");
            lines.push(format!(
                "  {} {}={}",
                mark,
                k.name,
                if purpose.is_empty() {
                    "<your-key>"
                } else {
                    purpose
                }
            ));
        }
    }
    if !m.requires.mcp_servers.is_empty() {
        lines.push(String::new());
        lines.push("Declared MCP servers (configured in .thclaws/mcp.json):".to_string());
        for s in &m.requires.mcp_servers {
            lines.push(format!("  - {s}"));
        }
    }
    lines.push(String::new());
    lines.push("Next: `thclaws` (CLI) or `thclaws --gui` (desktop).".to_string());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_settings(dir: &Path, json: &str) {
        let p = dir.join(".thclaws");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("settings.json"), json).unwrap();
    }

    fn read_settings(dir: &Path) -> serde_json::Value {
        let raw = std::fs::read(dir.join(".thclaws").join("settings.json")).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }

    #[test]
    fn restore_carries_gateway_proxy_over_an_agent_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        // The user's pre-install settings: on the gateway.
        let prior = br#"{"gatewayProxy": true, "model": "claude-x", "cloudUrl": "https://c"}"#;
        // What the agent's tarball extracted (no gateway/model keys).
        write_settings(
            dir.path(),
            r#"{"agent": {"id": "image-generator"}, "imageToolsEnabled": true}"#,
        );

        restore_installer_settings(dir.path(), prior).unwrap();

        let s = read_settings(dir.path());
        assert_eq!(
            s["gatewayProxy"],
            serde_json::json!(true),
            "gateway preserved"
        );
        assert_eq!(
            s["cloudUrl"],
            serde_json::json!("https://c"),
            "cloud url preserved"
        );
        // Agent-owned keys survive untouched.
        assert_eq!(s["imageToolsEnabled"], serde_json::json!(true));
        assert_eq!(s["agent"]["id"], serde_json::json!("image-generator"));
        // The user's model pick survives the overwrite — pre-fix the
        // post-get `/reload` reverted to the global default model.
        assert_eq!(
            s["model"],
            serde_json::json!("claude-x"),
            "user's model preserved"
        );
    }

    #[test]
    fn restore_keeps_a_publisher_pinned_model() {
        let dir = tempfile::tempdir().unwrap();
        let prior = br#"{"model": "user-choice"}"#;
        // Bundle explicitly pins a model → the pin wins over the prior.
        write_settings(dir.path(), r#"{"model": "publisher-pin"}"#);

        restore_installer_settings(dir.path(), prior).unwrap();

        assert_eq!(
            read_settings(dir.path())["model"],
            serde_json::json!("publisher-pin")
        );
    }

    #[test]
    fn restore_does_not_clobber_a_key_the_agent_set() {
        let dir = tempfile::tempdir().unwrap();
        let prior = br#"{"gatewayProxy": true}"#;
        // Unusual, but if the agent bundle explicitly ships gatewayProxy=false,
        // that wins — we only fill keys the agent left unset.
        write_settings(dir.path(), r#"{"gatewayProxy": false}"#);

        restore_installer_settings(dir.path(), prior).unwrap();

        assert_eq!(
            read_settings(dir.path())["gatewayProxy"],
            serde_json::json!(false)
        );
    }
}

// ---- workspace sync: /cloud push|pull (dev-plan/51) ----

/// Options for `/cloud push|pull`.
#[derive(Debug, Clone, Default)]
pub struct SyncOpts {
    pub delete: bool,
    pub dry_run: bool,
    pub workspace: Option<String>,
    pub force_rebind: bool,
    /// Skip the divergence guard — overwrite the other end even though it has
    /// changed since the last sync (the clobbered files still land in
    /// `.sync-trash/`).
    pub force: bool,
}

/// Push, streaming each progress line to `emit` the moment it is produced.
pub async fn push_streaming(
    cwd: &Path,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
    opts: SyncOpts,
    emit: &mut (dyn FnMut(String) + Send),
) {
    if let Err(e) = sync_inner(cwd, cloud_url, cloud_cfg, opts, true, emit).await {
        emit(format!("push failed: {}", e));
    }
}

/// Pull, streaming each progress line to `emit` the moment it is produced.
pub async fn pull_streaming(
    cwd: &Path,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
    opts: SyncOpts,
    emit: &mut (dyn FnMut(String) + Send),
) {
    if let Err(e) = sync_inner(cwd, cloud_url, cloud_cfg, opts, false, emit).await {
        emit(format!("pull failed: {}", e));
    }
}

pub async fn push_lines(
    cwd: &Path,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
    opts: SyncOpts,
) -> Vec<String> {
    let mut log = Vec::new();
    push_streaming(cwd, cloud_url, cloud_cfg, opts, &mut |l| log.push(l)).await;
    log
}

pub async fn pull_lines(
    cwd: &Path,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
    opts: SyncOpts,
) -> Vec<String> {
    let mut log = Vec::new();
    pull_streaming(cwd, cloud_url, cloud_cfg, opts, &mut |l| log.push(l)).await;
    log
}

async fn resolve_workspace(
    client: &Client,
    want: Option<&str>,
) -> Result<crate::cloud::client::WorkspaceSummary, String> {
    let mut wss = client.list_workspaces().await?;
    if wss.is_empty() {
        return Err("no hosted workspaces on your account — create one at /dashboard".into());
    }
    if let Some(slug) = want {
        return wss
            .into_iter()
            .find(|w| w.slug == slug)
            .ok_or_else(|| format!("no hosted workspace with slug '{}'", slug));
    }
    if wss.len() == 1 {
        return Ok(wss.remove(0));
    }
    let slugs: Vec<String> = wss.iter().map(|w| w.slug.clone()).collect();
    Err(format!(
        "you have {} workspaces — pass --workspace <slug>: {}",
        slugs.len(),
        slugs.join(", ")
    ))
}

async fn sync_inner(
    cwd: &Path,
    cloud_url: Option<&str>,
    cloud_cfg: Option<&CloudConfig>,
    opts: SyncOpts,
    is_push: bool,
    emit: &mut (dyn FnMut(String) + Send),
) -> Result<(), String> {
    // dev-plan/51 #3: both ends must be idle. Refuse if a local turn is running.
    if crate::agent_activity::busy_count() > 0 {
        return Err("a local turn is active — wait for it to finish before syncing".into());
    }
    let url = resolve_cloud_url(cloud_url, cloud_cfg);
    let token = crate::cloud::token();
    if token.is_none() {
        return Err("not logged in — paste your CLI token in Settings → thClaws.cloud".into());
    }
    emit("Connecting to thClaws.cloud…".into());
    let client = Client::new(&url, token);
    let ws = resolve_workspace(&client, opts.workspace.as_deref()).await?;
    emit(format!("Workspace: {} ({})", ws.slug, ws.id));
    let jwt = client.cli_exchange().await?;
    // Probe the runner directly — status strings ("ready"/"running") aren't a
    // reliable "is it up" signal, so try /sync/stat and only wake on failure.
    let stat = match client.ws_sync_stat(&ws.url, &jwt).await {
        Ok(s) => s,
        Err(e) if e.contains("404") => {
            return Err(format!(
                "'{}' is up but its engine doesn't expose /workspace/sync yet — \
                 restart it (pause→resume) to pick up the v0.81+ engine",
                ws.slug
            ));
        }
        Err(_) => {
            emit(format!(
                "Workspace not responding ({}) — resuming…",
                ws.status
            ));
            client.wake_workspace(&ws.id).await?;
            wait_for_runner(&client, &ws.url, &jwt, emit).await?
        }
    };
    if stat.busy {
        return Err("the cloud workspace has an active turn — try again when it's idle".into());
    }
    let local_binding = wssync::read_binding(cwd);
    let local_bound = local_binding.workspace_id.clone();
    let bound_note = local_bound
        .as_deref()
        .map(|l| format!(" (folder bound to {})", l))
        .unwrap_or_default();

    if is_push {
        if !stat.empty && local_bound.as_deref() != Some(ws.id.as_str()) && !opts.force_rebind {
            return Err(format!(
                "cloud workspace '{}' is not empty and this folder isn't bound to it{} — re-run with --force-rebind to overwrite it deliberately",
                ws.slug, bound_note
            ));
        }
        // P2: incremental when the runner exposes a manifest; else full tarball.
        let remote_manifest = client.ws_sync_manifest(&ws.url, &jwt).await?;
        // Divergence guard: refuse to clobber work the cloud did since the last
        // sync. Best-effort — only checkable when the runner exposes a manifest.
        if !opts.force {
            if let Some(remote) = &remote_manifest {
                if wssync::diverged_from_base(cwd, remote) {
                    return Err(format!(
                        "'{}' has changes since your last sync — pushing would overwrite them (recoverable in .sync-trash). Pull first to reconcile, or re-run with --force.",
                        ws.slug
                    ));
                }
            }
        }
        match remote_manifest {
            Some(remote) => {
                let local = wssync::build_manifest(cwd)?;
                let (upload, extraneous) = wssync::diff(&local, &remote);
                if opts.dry_run {
                    emit(format!(
                        "[dry-run] incremental push → '{}': {} file(s) to upload{}",
                        ws.slug,
                        upload.len(),
                        if opts.delete {
                            format!(", {} to delete on cloud", extraneous.len())
                        } else {
                            String::new()
                        }
                    ));
                    return Ok(());
                }
                emit(format!("Pushing {} changed file(s)…", upload.len()));
                let tmp = pack_paths_temp(cwd, &upload)?;
                let r = push_with_progress(&client, &ws.url, &jwt, tmp.path(), false, &ws.id, emit)
                    .await?;
                let deleted = if opts.delete && !extraneous.is_empty() {
                    client
                        .ws_sync_trash(&ws.url, &jwt, &extraneous)
                        .await?
                        .deleted
                } else {
                    0
                };
                write_push_binding(cwd, &ws, &url, &local_binding)?;
                emit(format!(
                    "✓ pushed {} file(s){} to '{}' (incremental)",
                    r.written,
                    if deleted > 0 {
                        format!(", deleted {}", deleted)
                    } else {
                        String::new()
                    },
                    ws.slug
                ));
            }
            None => {
                let local = wssync::stat_workspace(cwd)?;
                if opts.dry_run {
                    emit(format!(
                        "[dry-run] would push {} local file(s) → cloud '{}' (cloud has {}){}",
                        local.file_count,
                        ws.slug,
                        stat.file_count,
                        if opts.delete {
                            ", deleting extraneous on cloud"
                        } else {
                            ""
                        }
                    ));
                    return Ok(());
                }
                emit(format!(
                    "Packing {} file(s) ({:.1} KB)…",
                    local.file_count,
                    local.bytes as f64 / 1024.0
                ));
                let tmp = pack_workspace_temp(cwd, false)?;
                let r = push_with_progress(
                    &client,
                    &ws.url,
                    &jwt,
                    tmp.path(),
                    opts.delete,
                    &ws.id,
                    emit,
                )
                .await?;
                write_push_binding(cwd, &ws, &url, &local_binding)?;
                emit(format!(
                    "✓ pushed {} file(s){} to '{}'",
                    r.written,
                    if r.deleted > 0 {
                        format!(", deleted {}", r.deleted)
                    } else {
                        String::new()
                    },
                    ws.slug
                ));
            }
        }
    } else {
        let local_empty = wssync::is_empty(cwd)?;
        if !local_empty && local_bound.as_deref() != Some(ws.id.as_str()) && !opts.force_rebind {
            return Err(format!(
                "local folder is not empty and isn't bound to '{}'{} — re-run with --force-rebind to overwrite it deliberately",
                ws.slug, bound_note
            ));
        }
        // Divergence guard: refuse to clobber local work done since the last
        // sync. `local_manifest` is reused by the incremental branch below.
        let local_manifest = wssync::build_manifest(cwd)?;
        if !opts.force && wssync::diverged_from_base(cwd, &local_manifest) {
            return Err(format!(
                "this folder has changes since your last sync — pulling '{}' would overwrite them (recoverable in .sync-trash). Push first to reconcile, or re-run with --force.",
                ws.slug
            ));
        }
        match client.ws_sync_manifest(&ws.url, &jwt).await? {
            Some(remote) => {
                let local = local_manifest;
                let (download, extraneous) = wssync::diff(&remote, &local);
                if opts.dry_run {
                    emit(format!(
                        "[dry-run] incremental pull ← '{}': {} file(s) to download{}",
                        ws.slug,
                        download.len(),
                        if opts.delete {
                            format!(", {} to delete locally", extraneous.len())
                        } else {
                            String::new()
                        }
                    ));
                    return Ok(());
                }
                emit(format!("Pulling {} changed file(s)…", download.len()));
                if !download.is_empty() {
                    let tmp = client.ws_sync_export(&ws.url, &jwt, &download).await?;
                    let f =
                        std::fs::File::open(tmp.path()).map_err(|e| format!("open temp: {}", e))?;
                    wssync::untar_workspace_from(f, cwd, false)?;
                }
                let deleted = if opts.delete && !extraneous.is_empty() {
                    wssync::trash_paths(cwd, &extraneous)?.deleted
                } else {
                    0
                };
                write_pull_binding(cwd, &ws, &url, &local_binding)?;
                emit(format!(
                    "✓ pulled {} file(s){} into {} (incremental)",
                    download.len(),
                    if deleted > 0 {
                        format!(", deleted {}", deleted)
                    } else {
                        String::new()
                    },
                    cwd.display()
                ));
            }
            None => {
                if opts.dry_run {
                    let local = wssync::stat_workspace(cwd)?;
                    emit(format!(
                        "[dry-run] would pull cloud '{}' ({} file(s)) → local (has {}){}",
                        ws.slug,
                        stat.file_count,
                        local.file_count,
                        if opts.delete {
                            ", deleting extraneous locally"
                        } else {
                            ""
                        }
                    ));
                    return Ok(());
                }
                emit(format!(
                    "Pulling cloud '{}' ({} file(s))…",
                    ws.slug, stat.file_count
                ));
                let tmp = client.ws_sync_pull(&ws.url, &jwt, false).await?;
                let f = std::fs::File::open(tmp.path()).map_err(|e| format!("open temp: {}", e))?;
                let r = wssync::untar_workspace_from(f, cwd, opts.delete)?;
                write_pull_binding(cwd, &ws, &url, &local_binding)?;
                emit(format!(
                    "✓ pulled {} file(s){} into {}",
                    r.written,
                    if r.deleted > 0 {
                        format!(", deleted {}", r.deleted)
                    } else {
                        String::new()
                    },
                    cwd.display()
                ));
            }
        }
    }
    // Record the now-agreed content state so the NEXT sync can tell whether
    // the CLOUD end drifted. Best-effort — a watermark failure must not fail
    // the sync itself. (dry-run paths already returned above.)
    //
    // Base off the RUNNER's export manifest (re-fetched), NOT the local
    // manifest. The divergence guard compares the runner's manifest against
    // this base, so both sides of that comparison must be computed the SAME
    // way. A local manifest can legitimately differ from the runner's export
    // across engine versions — e.g. when the client's sync strip rules change
    // (build/ handling) before the runner's do — which would otherwise read
    // as perpetual divergence and force `--force` on every push. Comparing
    // runner-to-runner makes the guard fire only on genuine cloud-side edits.
    if let Ok(Some(remote)) = client.ws_sync_manifest(&ws.url, &jwt).await {
        let _ = wssync::write_sync_base(cwd, &wssync::manifest_fingerprint(&remote));
    } else if let Ok(m) = wssync::build_manifest(cwd) {
        // Runner predates the manifest endpoint (P2 404) — fall back to the
        // local fingerprint (the pre-P2 whole-tarball behaviour).
        let _ = wssync::write_sync_base(cwd, &wssync::manifest_fingerprint(&m));
    }
    Ok(())
}

/// Upload the tarball, emitting a live percentage readout for uploads large
/// enough that the transfer is the slow part. Small tarballs skip the ticker
/// (the surrounding "Pushing…"/"✓ pushed" lines already bracket them).
/// Pack the synced workspace to a temp `.tar.gz` (streamed to disk).
fn pack_workspace_temp(
    cwd: &Path,
    include_runtime: bool,
) -> Result<tempfile::NamedTempFile, String> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| format!("temp: {}", e))?;
    wssync::tar_workspace_to(cwd, include_runtime, tmp.as_file())?;
    Ok(tmp)
}

/// Pack a specific path list to a temp `.tar.gz` (streamed to disk).
fn pack_paths_temp(cwd: &Path, paths: &[String]) -> Result<tempfile::NamedTempFile, String> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| format!("temp: {}", e))?;
    wssync::tar_paths_to(cwd, paths, tmp.as_file())?;
    Ok(tmp)
}

async fn push_with_progress(
    client: &Client,
    ws_url: &str,
    jwt: &str,
    tarball_path: &Path,
    delete: bool,
    workspace_id: &str,
    emit: &mut (dyn FnMut(String) + Send),
) -> Result<crate::cloud::client::SyncPushResp, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    const TICKER_THRESHOLD: u64 = 256 * 1024;
    let total = std::fs::metadata(tarball_path)
        .map(|m| m.len())
        .unwrap_or(0);
    if total < TICKER_THRESHOLD {
        return client
            .ws_sync_push(ws_url, jwt, tarball_path, delete, workspace_id, None)
            .await;
    }
    let sent = Arc::new(AtomicU64::new(0));
    let fut = client.ws_sync_push(
        ws_url,
        jwt,
        tarball_path,
        delete,
        workspace_id,
        Some(sent.clone()),
    );
    tokio::pin!(fut);
    let total_mb = total as f64 / 1_048_576.0;
    let mut last_pct = u64::MAX;
    loop {
        tokio::select! {
            r = &mut fut => return r,
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                let done = sent.load(Ordering::Relaxed).min(total);
                let pct = done * 100 / total;
                if pct != last_pct {
                    last_pct = pct;
                    emit(format!(
                        "  uploading… {:.1}/{:.1} MB ({}%)",
                        done as f64 / 1_048_576.0,
                        total_mb,
                        pct
                    ));
                }
            }
        }
    }
}

/// Poll the runner's `/sync/stat` until it answers (after a resume) or times out.
async fn wait_for_runner(
    client: &Client,
    ws_url: &str,
    jwt: &str,
    emit: &mut (dyn FnMut(String) + Send),
) -> Result<crate::cloud::client::SyncStatResp, String> {
    let mut last = String::new();
    for attempt in 0..30 {
        match client.ws_sync_stat(ws_url, jwt).await {
            Ok(s) => return Ok(s),
            Err(e) if e.contains("404") => {
                return Err(format!(
                    "engine doesn't expose /workspace/sync — restart the workspace \
                     for the v0.81+ engine ({e})"
                ));
            }
            Err(e) => last = e,
        }
        if attempt == 0 {
            emit("Waiting for the workspace to come up…".into());
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Err(format!(
        "workspace didn't become reachable in time: {}",
        last
    ))
}

fn write_push_binding(
    cwd: &Path,
    ws: &crate::cloud::client::WorkspaceSummary,
    url: &str,
    prev: &wssync::Binding,
) -> Result<(), String> {
    wssync::write_binding(
        cwd,
        &wssync::Binding {
            workspace_id: Some(ws.id.clone()),
            slug: Some(ws.slug.clone()),
            cloud_url: Some(url.to_string()),
            last_push: Some(now_string()),
            last_pull: prev.last_pull.clone(),
        },
    )
}

fn write_pull_binding(
    cwd: &Path,
    ws: &crate::cloud::client::WorkspaceSummary,
    url: &str,
    prev: &wssync::Binding,
) -> Result<(), String> {
    wssync::write_binding(
        cwd,
        &wssync::Binding {
            workspace_id: Some(ws.id.clone()),
            slug: Some(ws.slug.clone()),
            cloud_url: Some(url.to_string()),
            last_push: prev.last_push.clone(),
            last_pull: Some(now_string()),
        },
    )
}

fn now_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}
