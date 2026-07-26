//! Workspace sync (dev-plan/51): tar/untar the working directory for the
//! `/cloud push|pull` round-trip between the desktop app and a hosted cloud
//! workspace.
//!
//! Unlike catalog publish (`pack::is_strippable`), `/cloud push|pull` is a
//! FULL directory teleport: the working tree AND all runtime state under
//! `.thclaws/state/` (sessions, kms, browser profile, workflow run-state, …)
//! ride along so work resumes on the other end mid-session. The ONLY things
//! dropped are regenerable, machine/arch-specific build dirs that would
//! corrupt the destination or waste the payload (`node_modules/`, `target/`,
//! `.venv/`, …) — see [`SYNC_STRIP_DIRS`]. Both ends stream the tarball
//! through a temp file (tar→disk on pack, disk→untar on apply) so memory
//! stays flat regardless of payload size. Sync-specific pieces:
//!   - a 10 GiB payload cap (`MAX_SYNC_BYTES`, the PVC quota),
//!   - `--delete` mirroring that moves removed files to `.sync-trash/<ts>/`
//!     (recoverable, not a hard delete),
//!   - traversal-safe extraction (rejects `..` / absolute, skips symlinks),
//!   - the UUID binding file `.thclaws/cloud-sync.json` that ties a local folder
//!     to exactly one hosted workspace.
//!
//! v1 is whole-tarball (`.tar.gz`, matching `pack.rs`); the incremental
//! manifest-diff path layers on top in P2.

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard cap on a single sync payload (uncompressed sum of synced files).
/// Matches the hosted workspace PVC quota; sync streams via temp files so the
/// cap tracks disk, not memory.
pub const MAX_SYNC_BYTES: u64 = 10 * 1024 * 1024 * 1024;

const BINDING_REL: &str = ".thclaws/cloud-sync.json";
const SETTINGS_REL: &str = ".thclaws/settings.json";
const SYNCIGNORE_REL: &str = ".thclaws/syncignore";
const TRASH_PREFIX: &str = ".sync-trash";
/// Local-only divergence watermark: the content fingerprint of the last
/// SUCCESSFUL sync. Excluded from the payload (never travels — each side keeps
/// its own), so a peer's tarball can't clobber it. Used to warn before a
/// push/pull overwrites work the other end did since that agreed state.
const SYNC_BASE_REL: &str = ".thclaws/cloud-sync-base.json";

/// Paths kept OUT of the divergence fingerprint: the sync plumbing itself
/// differs per-end by design (binding carries per-folder timestamps; settings
/// gets the gateway overlay injected on cloud), so hashing them would read as
/// "always diverged". Real work (sources, sessions, state) still counts.
const FINGERPRINT_SKIP: &[&str] = &[BINDING_REL, SETTINGS_REL, SYNCIGNORE_REL, SYNC_BASE_REL];

/// Records which hosted workspace a folder is paired with. Lives at
/// `.thclaws/cloud-sync.json` on both ends (dev-plan/51 decision #5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Binding {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_push: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pull: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SyncStat {
    pub file_count: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct UntarResult {
    pub written: usize,
    pub deleted: usize,
    pub trash_dir: Option<PathBuf>,
}

fn norm(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// Regenerable, machine/arch-specific dirs that must NEVER ride a workspace
/// teleport: they're rebuilt on demand and are platform-bound, so shipping a
/// macOS `target/` or a `.venv/` with absolute interpreter paths onto a Linux
/// runner (or vice-versa) corrupts the destination — and they dwarf the real
/// work in size. Matched as a path SEGMENT anywhere in the tree (not just at
/// root), so a monorepo's `frontend/node_modules/` is dropped too. Everything
/// NOT in this list teleports verbatim — sessions/state, `.git/`, secrets —
/// which is the whole point of push|pull vs. a catalog publish.
/// Always-stripped tool dirs — unambiguously regenerable caches.
pub const SYNC_STRIP_DIRS: &[&str] = &["node_modules", ".venv", "__pycache__", ".next"];

/// Conditionally-stripped names: real toolchain OUTPUT only when the
/// marker file that generates them sits beside them. `build/` in a JS
/// project (sibling `package.json`) is regenerable; `build/` in a
/// book-production workspace (sibling `book.yaml`, no `package.json`)
/// is the DELIVERABLES — epub/pdf/rendered slides/TTS audio that cost
/// real money to produce — and was being silently dropped from
/// /cloud push (544MB of a 1GB workspace in the reported case).
pub const SYNC_STRIP_IF_MARKER: &[(&str, &str)] = &[
    ("target", "Cargo.toml"),
    ("build", "package.json"),
    ("dist", "package.json"),
];

fn in_stripped_dir(root: &Path, rel: &Path) -> bool {
    let mut parent = PathBuf::new();
    for c in rel.components() {
        if let Component::Normal(seg) = c {
            if let Some(s) = seg.to_str() {
                if SYNC_STRIP_DIRS.contains(&s) {
                    return true;
                }
                for (name, marker) in SYNC_STRIP_IF_MARKER {
                    if s == *name && root.join(&parent).join(marker).is_file() {
                        return true;
                    }
                }
            }
        }
        parent.push(c);
    }
    false
}

/// Inside the sync exclude set? Only the regenerable build dirs
/// ([`SYNC_STRIP_DIRS`] + marker-confirmed [`SYNC_STRIP_IF_MARKER`])
/// plus the `.sync-trash/` tree itself (never sync the trash). NOT
/// `pack::is_strippable` — push|pull keeps runtime state.
fn excluded(root: &Path, rel: &Path) -> bool {
    let s = norm(rel);
    s == SYNC_BASE_REL
        || s == TRASH_PREFIX
        || s.starts_with(&format!("{TRASH_PREFIX}/"))
        || in_stripped_dir(root, rel)
}

/// Collect files relative to `root`. `keep` decides inclusion; symlinks are
/// always skipped (never followed — traversal safety).
fn walk(root: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<Vec<PathBuf>, String> {
    Ok(walk_with_dirs(root, keep)?.0)
}

/// Like [`walk`] but also returns every kept DIRECTORY (for empty-dir
/// preservation: dirs with no synced file beneath still ride the tar as
/// directory entries so scaffolding like `media/screenshots/` survives
/// a push).
fn walk_with_dirs(
    root: &Path,
    keep: &dyn Fn(&Path) -> bool,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    walk_inner(root, root, keep, &mut out, &mut dirs)?;
    out.sort();
    dirs.sort();
    Ok((out, dirs))
}

/// Kept directories with no synced file beneath them.
fn empty_dirs_for(root: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<Vec<PathBuf>, String> {
    let (files, dirs) = walk_with_dirs(root, keep)?;
    let file_norms: Vec<String> = files.iter().map(|f| norm(f)).collect();
    Ok(dirs
        .into_iter()
        .filter(|d| {
            let prefix = format!("{}/", norm(d));
            !file_norms.iter().any(|f| f.starts_with(&prefix))
        })
        .collect())
}

/// Empty dirs under the standard synced view (strip set + syncignore).
fn empty_synced_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let ignores = load_syncignore(root);
    empty_dirs_for(root, &|rel| {
        !excluded(root, rel) && !ignored_by(&norm(rel), &ignores)
    })
}

fn walk_inner(
    root: &Path,
    dir: &Path,
    keep: &dyn Fn(&Path) -> bool,
    out: &mut Vec<PathBuf>,
    dirs: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("read_dir {}: {}", dir.display(), e)),
    };
    for ent in rd {
        let ent = ent.map_err(|e| format!("dir entry: {}", e))?;
        let path = ent.path();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if !keep(&rel) {
            continue;
        }
        let ft = ent.file_type().map_err(|e| format!("file_type: {}", e))?;
        if ft.is_symlink() {
            continue; // never follow or sync symlinks
        } else if ft.is_dir() {
            dirs.push(rel.clone());
            walk_inner(root, &path, keep, out, dirs)?;
        } else if ft.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

/// User exclude patterns from `.thclaws/syncignore` (dev-plan/51 open
/// question, resolved 2026-07-04): one path per line, `/`-separated,
/// `#` comments + blank lines skipped, trailing `/` tolerated. A line
/// matches its exact rel path or anything under it (prefix at a `/`
/// boundary). Deliberately NOT a glob engine — plain prefixes cover the
/// real use (keep big data/build dirs out of the sync) with zero
/// pattern-language surprises. The file itself lives inside the synced
/// set, so a push propagates the ignore list to the other side.
fn load_syncignore(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(SYNCIGNORE_REL)) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.trim_start_matches("./")
                .trim_end_matches('/')
                .replace('\\', "/")
        })
        .filter(|l| !l.is_empty())
        .collect()
}

fn ignored_by(rel: &str, patterns: &[String]) -> bool {
    // The sync plumbing itself can't be ignored away — losing the
    // binding / settings / the ignore file mid-round-trip is a foot-gun.
    // Ancestor DIRS are exempt too (the walk prunes whole subtrees, so
    // an ignored `.thclaws` must still descend far enough to keep them).
    const PLUMBING: &[&str] = &[BINDING_REL, SETTINGS_REL, SYNCIGNORE_REL];
    if PLUMBING
        .iter()
        .any(|p| rel == *p || p.starts_with(&format!("{rel}/")))
    {
        return false;
    }
    patterns
        .iter()
        .any(|p| rel == p || rel.starts_with(&format!("{p}/")))
}

/// Synced (non-excluded) files, relative to `root`. Applies the strip
/// set plus the user's `.thclaws/syncignore`.
fn walk_synced(root: &Path) -> Result<Vec<PathBuf>, String> {
    let ignores = load_syncignore(root);
    walk(root, &|rel| {
        !excluded(root, rel) && !ignored_by(&norm(rel), &ignores)
    })
}

pub fn stat_workspace(root: &Path) -> Result<SyncStat, String> {
    let files = walk_synced(root)?;
    let mut bytes = 0u64;
    for rel in &files {
        bytes += std::fs::metadata(root.join(rel))
            .map(|m| m.len())
            .unwrap_or(0);
    }
    Ok(SyncStat {
        file_count: files.len(),
        bytes,
    })
}

/// "Empty" for the binding guard: no synced files other than a bare
/// `.thclaws/settings.json` (a fresh workspace ships only settings).
pub fn is_empty(root: &Path) -> Result<bool, String> {
    Ok(walk_synced(root)?.iter().all(|r| norm(r) == SETTINGS_REL))
}

pub fn read_binding(root: &Path) -> Binding {
    std::fs::read(root.join(BINDING_REL))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_binding(root: &Path, b: &Binding) -> Result<(), String> {
    let p = root.join(BINDING_REL);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let data = serde_json::to_vec_pretty(b).map_err(|e| format!("serialize binding: {}", e))?;
    std::fs::write(&p, data).map_err(|e| format!("write binding: {}", e))
}

/// Tar+gzip a list of rel paths under `root` into `w`. `empty_dirs`
/// ride as directory entries (~0 bytes) so scaffold folders survive;
/// receivers that predate dir handling skip them harmlessly.
fn write_tar<W: Write>(
    root: &Path,
    files: &[PathBuf],
    empty_dirs: &[PathBuf],
    w: W,
) -> Result<(), String> {
    let enc = GzEncoder::new(w, Compression::default());
    let mut tar = tar::Builder::new(enc);
    for rel in empty_dirs {
        let abs = root.join(rel);
        if abs.is_dir() {
            tar.append_dir(rel, &abs)
                .map_err(|e| format!("tar append dir {}: {}", rel.display(), e))?;
        }
    }
    for rel in files {
        let abs = root.join(rel);
        let mut f =
            std::fs::File::open(&abs).map_err(|e| format!("open {}: {}", abs.display(), e))?;
        tar.append_file(rel, &mut f)
            .map_err(|e| format!("tar append {}: {}", rel.display(), e))?;
    }
    let enc = tar.into_inner().map_err(|e| format!("tar finish: {}", e))?;
    enc.finish().map_err(|e| format!("gz finish: {}", e))?;
    Ok(())
}

/// Tar+gzip the synced files under `root` into `w`. `include_runtime` bypasses
/// the strip set (still skips `.sync-trash/`). Enforces `MAX_SYNC_BYTES`.
/// Returns the uncompressed byte total.
pub fn tar_workspace_to<W: Write>(root: &Path, include_runtime: bool, w: W) -> Result<u64, String> {
    let runtime_keep = |rel: &Path| {
        let s = norm(rel);
        s != TRASH_PREFIX && !s.starts_with(&format!("{TRASH_PREFIX}/"))
    };
    let (files, empty_dirs) = if include_runtime {
        (
            walk(root, &runtime_keep)?,
            empty_dirs_for(root, &runtime_keep)?,
        )
    } else {
        (walk_synced(root)?, empty_synced_dirs(root)?)
    };
    let total: u64 = files
        .iter()
        .map(|r| {
            std::fs::metadata(root.join(r))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();
    if total > MAX_SYNC_BYTES {
        return Err(format!(
            "workspace is {} MB, over the {} MB sync cap",
            total / 1_048_576,
            MAX_SYNC_BYTES / 1_048_576
        ));
    }
    write_tar(root, &files, &empty_dirs, w)?;
    Ok(total)
}

/// In-memory wrapper over [`tar_workspace_to`] (back-compat / tests).
pub fn tar_workspace(root: &Path, include_runtime: bool) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    tar_workspace_to(root, include_runtime, &mut buf)?;
    Ok(buf)
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Canonicalize `root`, creating it if missing.
fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("mkdir {}: {}", root.display(), e))?;
    root.canonicalize()
        .map_err(|e| format!("canonicalize {}: {}", root.display(), e))
}

/// Extract a `.tar.gz` into the (canonical) `root`, overwriting in place.
/// Traversal-safe. Returns (files written, set of incoming relative paths).
fn extract_tarball<R: Read>(reader: R, root: &Path) -> Result<(usize, BTreeSet<PathBuf>), String> {
    let mut written = 0usize;
    let mut incoming: BTreeSet<PathBuf> = BTreeSet::new();
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    for entry in archive
        .entries()
        .map_err(|e| format!("read archive: {}", e))?
    {
        let mut entry = entry.map_err(|e| format!("read entry: {}", e))?;
        let path = entry
            .path()
            .map_err(|e| format!("entry path: {}", e))?
            .into_owned();
        if is_unsafe_entry(&path) {
            return Err(format!("refused unsafe entry path: {}", path.display()));
        }
        if entry.header().entry_type().is_dir() {
            // Empty-dir preservation: materialize directory entries so
            // scaffold folders (media/screenshots/, output/, …) survive.
            std::fs::create_dir_all(root.join(&path))
                .map_err(|e| format!("mkdir {}: {}", path.display(), e))?;
            continue;
        }
        let out = root.join(&path);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
        }
        let mut f =
            std::fs::File::create(&out).map_err(|e| format!("create {}: {}", out.display(), e))?;
        std::io::copy(&mut entry, &mut f).map_err(|e| format!("write {}: {}", out.display(), e))?;
        incoming.insert(path);
        written += 1;
    }
    Ok((written, incoming))
}

/// Extract a full `.tar.gz` (streamed from `reader`) into `root`, overwriting in
/// place. When `delete` is set, synced files not present in the tarball are
/// moved to `.sync-trash/<ts>/` (recoverable mirror).
pub fn untar_workspace_from<R: Read>(
    reader: R,
    root: &Path,
    delete: bool,
) -> Result<UntarResult, String> {
    let root = canonical_root(root)?;
    let (written, incoming) = extract_tarball(reader, &root)?;
    let trash = root.join(TRASH_PREFIX).join(unix_secs().to_string());
    let mut trash_used = false;
    let mut deleted = 0usize;
    if delete {
        for rel in walk_synced(&root)? {
            if !incoming.contains(&rel) {
                move_to_trash(&root, &trash, &rel, &mut trash_used)?;
                deleted += 1;
            }
        }
    }
    Ok(UntarResult {
        written,
        deleted,
        trash_dir: trash_used.then_some(trash),
    })
}

/// In-memory wrapper over [`untar_workspace_from`] (back-compat / tests).
pub fn untar_workspace(bytes: &[u8], root: &Path, delete: bool) -> Result<UntarResult, String> {
    untar_workspace_from(std::io::Cursor::new(bytes), root, delete)
}

/// Reject archive entries that would escape the extraction root.
fn is_unsafe_entry(path: &Path) -> bool {
    path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir))
}

fn move_to_trash(root: &Path, trash: &Path, rel: &Path, used: &mut bool) -> Result<(), String> {
    let src = root.join(rel);
    if !src.exists() {
        return Ok(());
    }
    let dst = trash.join(rel);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir trash {}: {}", parent.display(), e))?;
    }
    std::fs::rename(&src, &dst)
        .or_else(|_| {
            std::fs::copy(&src, &dst)
                .and_then(|_| std::fs::remove_file(&src))
                .map(|_| ())
        })
        .map_err(|e| format!("trash {}: {}", rel.display(), e))?;
    *used = true;
    Ok(())
}

// ---- P2: incremental manifest-diff ----

/// One file's identity in a sync manifest. `sha256` is the diff key (mtime is
/// unreliable across machines, so we hash content).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Content manifest of the synced files under `root` (the diff input).
pub fn build_manifest(root: &Path) -> Result<Vec<FileEntry>, String> {
    let mut out = Vec::new();
    for rel in walk_synced(root)? {
        let data =
            std::fs::read(root.join(&rel)).map_err(|e| format!("read {}: {}", rel.display(), e))?;
        out.push(FileEntry {
            path: norm(&rel),
            size: data.len() as u64,
            sha256: sha256_hex(&data),
        });
    }
    Ok(out)
}

/// Compare a source manifest against a destination manifest. Returns
/// `(to_transfer, extraneous)`: files in `src` that are missing or
/// content-different in `dst` (must be sent src→dst), and files in `dst` not in
/// `src` (candidates for `--delete`). Pure — the unit-testable heart of P2.
pub fn diff(src: &[FileEntry], dst: &[FileEntry]) -> (Vec<String>, Vec<String>) {
    use std::collections::{HashMap, HashSet};
    let dst_hash: HashMap<&str, &str> = dst
        .iter()
        .map(|e| (e.path.as_str(), e.sha256.as_str()))
        .collect();
    let src_paths: HashSet<&str> = src.iter().map(|e| e.path.as_str()).collect();
    let mut transfer: Vec<String> = src
        .iter()
        .filter(|e| {
            dst_hash
                .get(e.path.as_str())
                .map(|h| *h != e.sha256.as_str())
                .unwrap_or(true)
        })
        .map(|e| e.path.clone())
        .collect();
    let mut extraneous: Vec<String> = dst
        .iter()
        .filter(|e| !src_paths.contains(e.path.as_str()))
        .map(|e| e.path.clone())
        .collect();
    transfer.sort();
    extraneous.sort();
    (transfer, extraneous)
}

/// Order-independent content fingerprint of a manifest, over the real work
/// only (plumbing in [`FINGERPRINT_SKIP`] excluded). Two sides that hold the
/// same content produce the same fingerprint regardless of their per-end
/// binding/settings — the basis of the divergence check.
pub fn manifest_fingerprint(entries: &[FileEntry]) -> String {
    let mut parts: Vec<String> = entries
        .iter()
        .filter(|e| !FINGERPRINT_SKIP.contains(&e.path.as_str()))
        .map(|e| format!("{}\0{}", e.path, e.sha256))
        .collect();
    parts.sort();
    sha256_hex(parts.join("\n").as_bytes())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SyncBase {
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<String>,
}

/// The content fingerprint recorded at the last successful sync (the agreed
/// state), or `None` if this folder has never completed one.
pub fn read_sync_base(root: &Path) -> Option<String> {
    std::fs::read(root.join(SYNC_BASE_REL))
        .ok()
        .and_then(|b| serde_json::from_slice::<SyncBase>(&b).ok())
        .and_then(|s| s.base)
}

/// Record the agreed-state fingerprint after a successful sync. Excluded from
/// the payload, so it stays local to this end.
pub fn write_sync_base(root: &Path, fingerprint: &str) -> Result<(), String> {
    let path = root.join(SYNC_BASE_REL);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
    }
    let body = serde_json::to_vec(&SyncBase {
        base: Some(fingerprint.to_string()),
    })
    .map_err(|e| format!("encode base: {}", e))?;
    std::fs::write(&path, body).map_err(|e| format!("write base: {}", e))
}

/// Has `manifest` drifted from the recorded agreed state? `false` when there
/// is no base yet (first sync — nothing to clobber). Content-only, so it never
/// fires on the per-end plumbing differences and needs no clocks.
pub fn diverged_from_base(root: &Path, manifest: &[FileEntry]) -> bool {
    match read_sync_base(root) {
        Some(base) => manifest_fingerprint(manifest) != base,
        None => false,
    }
}

/// Tar+gzip a specific list of relative paths into `w` (incremental push body /
/// pull export). Skips missing or unsafe paths. Enforces `MAX_SYNC_BYTES`.
/// Returns the uncompressed byte total.
pub fn tar_paths_to<W: Write>(root: &Path, paths: &[String], w: W) -> Result<u64, String> {
    let mut total = 0u64;
    let mut valid: Vec<PathBuf> = Vec::new();
    for p in paths {
        let rel = Path::new(p);
        if is_unsafe_entry(rel) {
            continue;
        }
        if let Ok(m) = std::fs::metadata(root.join(rel)) {
            if m.is_file() {
                total += m.len();
                valid.push(rel.to_path_buf());
            }
        }
    }
    if total > MAX_SYNC_BYTES {
        return Err(format!(
            "changed files total {} MB, over the {} MB sync cap",
            total / 1_048_576,
            MAX_SYNC_BYTES / 1_048_576
        ));
    }
    // Every incremental push also carries the CURRENT empty dirs — dir
    // entries are ~free, create_dir_all on the receiver is idempotent,
    // and the manifest (files-only, fixed wire shape) can't express them.
    let empty_dirs = empty_synced_dirs(root)?;
    write_tar(root, &valid, &empty_dirs, w)?;
    Ok(total)
}

/// In-memory wrapper over [`tar_paths_to`] (back-compat / tests).
pub fn tar_paths(root: &Path, paths: &[String]) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    tar_paths_to(root, paths, &mut buf)?;
    Ok(buf)
}

/// Move a list of relative paths to `.sync-trash/<ts>/` (incremental `--delete`:
/// the partial tarball is applied via `untar_workspace(.., delete=false)`, then
/// the extraneous paths are trashed with this).
pub fn trash_paths(root: &Path, paths: &[String]) -> Result<UntarResult, String> {
    let root = canonical_root(root)?;
    let trash = root.join(TRASH_PREFIX).join(unix_secs().to_string());
    let mut trash_used = false;
    let mut deleted = 0usize;
    for p in paths {
        let rel = Path::new(p);
        if is_unsafe_entry(rel) {
            continue;
        }
        if root.join(rel).exists() {
            move_to_trash(&root, &trash, rel, &mut trash_used)?;
            deleted += 1;
        }
    }
    Ok(UntarResult {
        written: 0,
        deleted,
        trash_dir: trash_used.then_some(trash),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("wssync-{tag}-{ts}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn roundtrip_and_strip() {
        let src = tmp("src");
        write(&src, "a.txt", "hello");
        write(&src, "sub/b.md", "world");
        write(&src, ".thclaws/settings.json", "{}");
        write(&src, ".thclaws/state/sessions/x.jsonl", "SESSION"); // teleported now
        write(&src, "node_modules/pkg/i.js", "js"); // stripped (regenerable)
        let bytes = tar_workspace(&src, false).unwrap();
        let dst = tmp("dst");
        let r = untar_workspace(&bytes, &dst, false).unwrap();
        assert_eq!(r.written, 4); // a.txt, sub/b.md, settings.json, state/sessions/x.jsonl
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "hello");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub/b.md")).unwrap(),
            "world"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join(".thclaws/state/sessions/x.jsonl")).unwrap(),
            "SESSION",
            "runtime state must teleport with push|pull"
        );
        assert!(
            !dst.join("node_modules/pkg/i.js").exists(),
            "regenerable build dirs must be stripped"
        );
    }

    #[test]
    fn delete_moves_extraneous_to_trash() {
        let dst = tmp("del");
        write(&dst, "keep.txt", "v1");
        write(&dst, "stale.txt", "old"); // not in tarball → should be trashed
        let src = tmp("delsrc");
        write(&src, "keep.txt", "v2");
        let bytes = tar_workspace(&src, false).unwrap();
        let r = untar_workspace(&bytes, &dst, true).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("keep.txt")).unwrap(), "v2");
        assert!(!dst.join("stale.txt").exists(), "extraneous removed");
        assert_eq!(r.deleted, 1);
        let trash = r.trash_dir.expect("trash created");
        assert_eq!(
            std::fs::read_to_string(trash.join("stale.txt")).unwrap(),
            "old",
            "recoverable in trash"
        );
    }

    #[test]
    fn no_delete_keeps_extraneous() {
        let dst = tmp("nodel");
        write(&dst, "stale.txt", "old");
        let src = tmp("nodelsrc");
        write(&src, "a.txt", "x");
        let bytes = tar_workspace(&src, false).unwrap();
        let r = untar_workspace(&bytes, &dst, false).unwrap();
        assert!(
            dst.join("stale.txt").exists(),
            "without --delete, extraneous stays"
        );
        assert_eq!(r.deleted, 0);
    }

    #[test]
    fn is_empty_treats_bare_settings_as_empty() {
        let root = tmp("empty");
        write(&root, ".thclaws/settings.json", "{}");
        assert!(is_empty(&root).unwrap());
        write(&root, "real.txt", "x");
        assert!(!is_empty(&root).unwrap());
    }

    #[test]
    fn binding_roundtrip() {
        let root = tmp("bind");
        let b = Binding {
            workspace_id: Some("ws-123".into()),
            slug: Some("my-agent".into()),
            ..Default::default()
        };
        write_binding(&root, &b).unwrap();
        assert_eq!(read_binding(&root).workspace_id.as_deref(), Some("ws-123"));
    }

    #[test]
    fn rejects_path_traversal() {
        // The safe tar Builder refuses to even write a `..` entry, so test the
        // guard predicate directly — it's what untar enforces on every entry.
        assert!(is_unsafe_entry(Path::new("../escape.txt")));
        assert!(is_unsafe_entry(Path::new("a/../../escape")));
        assert!(is_unsafe_entry(Path::new("/etc/passwd")));
        assert!(!is_unsafe_entry(Path::new("ok/rel.txt")));
        assert!(!is_unsafe_entry(Path::new("a/b/c.md")));
    }

    #[test]
    fn manifest_and_diff() {
        let a = tmp("mfa");
        write(&a, "same.txt", "x");
        write(&a, "changed.txt", "v1");
        write(&a, "only_a.txt", "a");
        let b = tmp("mfb");
        write(&b, "same.txt", "x");
        write(&b, "changed.txt", "v2");
        write(&b, "only_b.txt", "b");
        let (transfer, extraneous) =
            diff(&build_manifest(&a).unwrap(), &build_manifest(&b).unwrap());
        assert_eq!(
            transfer,
            vec!["changed.txt".to_string(), "only_a.txt".to_string()]
        );
        assert_eq!(extraneous, vec!["only_b.txt".to_string()]);
    }

    #[test]
    fn incremental_push_apply() {
        let dst = tmp("incdst");
        write(&dst, "keep.txt", "old");
        write(&dst, "stale.txt", "bye");
        let src = tmp("incsrc");
        write(&src, "keep.txt", "new");
        write(&src, "added.txt", "hi");
        let (transfer, extraneous) = diff(
            &build_manifest(&src).unwrap(),
            &build_manifest(&dst).unwrap(),
        );
        let tarball = tar_paths(&src, &transfer).unwrap();
        let w = untar_workspace(&tarball, &dst, false).unwrap();
        let t = trash_paths(&dst, &extraneous).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("keep.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("added.txt")).unwrap(),
            "hi"
        );
        assert!(!dst.join("stale.txt").exists(), "extraneous removed");
        assert_eq!(w.written, 2);
        assert_eq!(t.deleted, 1);
    }

    #[test]
    fn syncignore_excludes_prefixes_but_not_plumbing() {
        let root = tmp("ignore");
        write(&root, "keep.txt", "k");
        write(&root, "bigdata/blob.bin", "xxxx");
        write(&root, "bigdata/sub/deep.bin", "yyyy");
        write(&root, "node_modules/pkg/index.js", "js");
        write(&root, ".thclaws/settings.json", "{}");
        write(
            &root,
            ".thclaws/syncignore",
            "# comment\n\nbigdata/\nnode_modules\n.thclaws\n",
        );
        let files: Vec<String> = walk_synced(&root)
            .unwrap()
            .iter()
            .map(|r| norm(r))
            .collect();
        assert!(files.contains(&"keep.txt".to_string()));
        assert!(!files.iter().any(|f| f.starts_with("bigdata")));
        assert!(!files.iter().any(|f| f.starts_with("node_modules")));
        // Plumbing survives even a `.thclaws` wholesale ignore.
        assert!(files.contains(&".thclaws/settings.json".to_string()));
        assert!(files.contains(&".thclaws/syncignore".to_string()));
        // Prefix must respect the `/` boundary: `bigdata2.txt` is NOT
        // covered by the `bigdata` pattern.
        write(&root, "bigdata2.txt", "z");
        let files: Vec<String> = walk_synced(&root)
            .unwrap()
            .iter()
            .map(|r| norm(r))
            .collect();
        assert!(files.contains(&"bigdata2.txt".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn divergence_tracks_content_not_plumbing() {
        let root = tmp("diverge");
        write(&root, "src/main.rs", "fn main(){}");
        write(&root, ".thclaws/state/sessions/s.jsonl", "turn1");
        write(&root, ".thclaws/settings.json", "{}");
        // No base yet → first sync, nothing to clobber.
        let m0 = build_manifest(&root).unwrap();
        assert!(!diverged_from_base(&root, &m0));
        // Record the agreed state.
        write_sync_base(&root, &manifest_fingerprint(&m0)).unwrap();
        assert!(!diverged_from_base(&root, &build_manifest(&root).unwrap()));
        // Plumbing churn (settings + the moving binding) must NOT read as drift.
        write(&root, ".thclaws/settings.json", "{\"gatewayProxy\":true}");
        write(&root, ".thclaws/cloud-sync.json", "{\"last_push\":\"999\"}");
        assert!(
            !diverged_from_base(&root, &build_manifest(&root).unwrap()),
            "per-end plumbing must not count as divergence"
        );
        // Real work does.
        write(&root, ".thclaws/state/sessions/s.jsonl", "turn2");
        assert!(
            diverged_from_base(&root, &build_manifest(&root).unwrap()),
            "a changed session must read as divergence"
        );
        // The base file itself never travels.
        assert!(excluded(&root, Path::new(".thclaws/cloud-sync-base.json")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_teleports_state_and_sessions_but_not_build_dirs() {
        let root = tmp("teleport");
        write(&root, "src/main.rs", "fn main(){}");
        write(&root, ".env", "SECRET=1");
        // Runtime state — the reported bug: sessions moved under state/ and
        // were being stripped. A teleport must carry all of it.
        write(&root, ".thclaws/state/sessions/s1.json", "{}");
        write(&root, ".thclaws/state/kms/key", "k");
        // Regenerable / arch-specific — never ride, incl. a NESTED node_modules.
        write(&root, "node_modules/pkg/index.js", "js");
        write(&root, "frontend/node_modules/x.js", "js");
        write(&root, "Cargo.toml", "[package]");
        write(&root, "target/debug/app", "bin");
        write(&root, "__pycache__/m.pyc", "x");
        // Marker heuristic: a book-style build/ (no package.json) is
        // CONTENT and must ride; a JS build/ (sibling package.json) is
        // regenerable output and must not.
        write(&root, "build/slides/ch01.pdf", "pdf");
        write(&root, "web/package.json", "{}");
        write(&root, "web/build/bundle.js", "js");
        write(&root, "tools/target/data.csv", "csv"); // no Cargo.toml → content
        let files: Vec<String> = walk_synced(&root)
            .unwrap()
            .iter()
            .map(|r| norm(r))
            .collect();
        assert!(files.contains(&".thclaws/state/sessions/s1.json".to_string()));
        assert!(files.contains(&".thclaws/state/kms/key".to_string()));
        assert!(files.contains(&".env".to_string()));
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(
            !files.iter().any(|f| f.contains("node_modules")),
            "nested/root node_modules dropped"
        );
        assert!(!files.iter().any(|f| f.starts_with("target/")));
        assert!(!files.iter().any(|f| f.contains("__pycache__")));
        assert!(
            files.contains(&"build/slides/ch01.pdf".to_string()),
            "book-style build/ (no package.json) must sync"
        );
        assert!(
            !files.iter().any(|f| f.starts_with("web/build/")),
            "JS build/ beside package.json must strip"
        );
        assert!(
            files.contains(&"tools/target/data.csv".to_string()),
            "target/ without Cargo.toml must sync"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_dirs_survive_roundtrip() {
        let root = tmp("emptydirs-src");
        write(&root, "chapters/ch01.md", "# ch1");
        // Scaffold dirs with no files — must survive a push.
        std::fs::create_dir_all(root.join("media/screenshots")).unwrap();
        std::fs::create_dir_all(root.join("media/generated")).unwrap();
        std::fs::create_dir_all(root.join("output")).unwrap();
        // A stripped empty dir must NOT ride.
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        // A dir that DOES have a file is not an "empty dir" but still lands.
        write(&root, "reports/r.md", "r");

        let empties: Vec<String> = empty_synced_dirs(&root)
            .unwrap()
            .iter()
            .map(|d| norm(d))
            .collect();
        assert!(empties.contains(&"media/screenshots".to_string()));
        assert!(empties.contains(&"media/generated".to_string()));
        assert!(empties.contains(&"output".to_string()));
        assert!(!empties.iter().any(|d| d.contains("node_modules")));
        assert!(
            !empties.contains(&"reports".to_string()),
            "reports has a file"
        );

        // Full round-trip: tar → untar into a fresh root.
        let bytes = tar_workspace(&root, false).unwrap();
        let dst = tmp("emptydirs-dst");
        untar_workspace(&bytes, &dst, false).unwrap();
        assert!(
            dst.join("media/screenshots").is_dir(),
            "empty scaffold dir teleported"
        );
        assert!(dst.join("media/generated").is_dir());
        assert!(dst.join("output").is_dir());
        assert!(dst.join("chapters/ch01.md").is_file());
        assert!(
            !dst.join("node_modules").exists(),
            "stripped dir not teleported"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn syncignore_applies_to_manifest_and_stat() {
        let root = tmp("ignore-manifest");
        write(&root, "a.txt", "a");
        write(&root, "skipme/b.txt", "b");
        write(&root, ".thclaws/syncignore", "skipme\n");
        let m = build_manifest(&root).unwrap();
        assert!(m.iter().any(|e| e.path == "a.txt"));
        assert!(!m.iter().any(|e| e.path.starts_with("skipme")));
        let s = stat_workspace(&root).unwrap();
        assert_eq!(s.file_count, m.len());
        let _ = std::fs::remove_dir_all(&root);
    }
}
