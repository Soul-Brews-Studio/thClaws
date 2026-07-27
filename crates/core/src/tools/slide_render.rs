//! `RenderSlides` — relay a Marp markdown deck to the `slide-render`
//! microservice and write the resulting PDF + per-slide PNGs into the
//! workspace. Mirrors `TextToImage`: opt-in (`imageToolsEnabled`),
//! approval-gated, backend-initiated so the frontend never calls the
//! render API directly (the token lives in Rust — see
//! `jimmy-plugins/slide-render/README.md` "Architecture constraint").
//!
//! Endpoint resolution mirrors the media tools' native-or-gateway
//! logic, but `slide-render` is a thClaws-hosted service (not a
//! third-party provider) so the native branch is a full URL rather than
//! a key:
//!   1. `SLIDE_RENDER_API` set  → POST `<url>/slides` directly
//!      (Bearer `SLIDE_RENDER_TOKEN` if that env is also set).
//!   2. else thClaws Gateway key present → POST
//!      `<gateway>/slide-render/slides` (Bearer gateway key; metered).
//!   3. else error.
//!
//! Output is text-only (paths + a slide count). The rendered pages are
//! artifacts for the user — the GUI shows them from disk — so the bytes
//! are never shipped back into the model's context (same rationale as
//! `image_gen::build_image_result`).

use crate::error::{Error, Result};
use crate::tools::{req_str, Tool};
use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

fn opt(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

struct SlideEndpoint {
    url: String,
    bearer: Option<String>,
    via_gateway: bool,
}

/// Native (`SLIDE_RENDER_API`) wins over the gateway so a self-hosted /
/// local render service is used without touching cloud credits; the
/// gateway is the zero-config fallback for cloud-logged-in users.
fn resolve_slide_endpoint() -> Result<SlideEndpoint> {
    resolve_slide_endpoint_for("slides")
}

/// Same resolution, for a named endpoint on the service (`slides`,
/// `pptx`). Split out when `/pptx` landed — the two share every rule
/// (native URL wins over gateway, gateway is the zero-config fallback)
/// and only differ in the path.
fn resolve_slide_endpoint_for(endpoint: &str) -> Result<SlideEndpoint> {
    if let Ok(base) = std::env::var("SLIDE_RENDER_API") {
        let base = base.trim();
        if !base.is_empty() {
            let token = std::env::var("SLIDE_RENDER_TOKEN")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            return Ok(SlideEndpoint {
                url: format!("{}/{endpoint}", base.trim_end_matches('/')),
                bearer: token,
                via_gateway: false,
            });
        }
    }
    if let Some(key) = crate::providers::thclaws_gateway::resolve_access_key() {
        let base = crate::providers::thclaws_gateway::resolve_base_url();
        return Ok(SlideEndpoint {
            url: format!("{}/slide-render/{endpoint}", base.trim_end_matches('/')),
            bearer: Some(key),
            via_gateway: true,
        });
    }
    Err(Error::Tool(
        "no slide-render endpoint — set SLIDE_RENDER_API to a self-hosted service, or enable the \
         thClaws Gateway (sign in to thClaws.cloud, add a `gateway` key, or set \
         THCLAWS_GATEWAY_API_KEY)"
            .into(),
    ))
}

fn mime_of(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

const MAX_IMAGES: usize = 120;
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

/// Read image files (png/jpg/webp/gif/svg) directly under `dir` — the
/// figures/screenshots a deck references by `<img src="name.png">`.
/// Non-recursive: the converter emits flat filenames.
fn collect_images(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Tool(format!("read image_dir {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        if out.len() >= MAX_IMAGES {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg"
        ) {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|e| Error::Tool(format!("stat: {e}")))?;
        if meta.len() > MAX_IMAGE_BYTES {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::Tool(format!("read {}: {e}", path.display())))?;
        out.push((name, bytes));
    }
    Ok(out)
}

/// Unzip the service response ({deck.pdf, slides/slide.NNN.png}) into
/// `out_dir`, flattening `slides/`. The PDF keeps its name (`deck.pdf`);
/// PNGs are re-numbered in sorted order to `slide-NN.png` (dash,
/// 2-digit zero-pad) — the convention the rest of the toolchain pairs
/// against (`slide-01.png` ↔ `slide-01.wav`, video builder globs
/// `slide-*.png`). Marp emits `slide.001.png` (dot), which would miss
/// that glob, so normalising here makes the tool a drop-in. Returns
/// (pdf_written, png_count).
fn extract_deck(zip_bytes: &[u8], out_dir: &Path) -> Result<(bool, usize)> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| Error::Tool(format!("open render zip: {e}")))?;

    // First pass: classify entries (index + basename) without extracting,
    // so PNGs can be renumbered in a stable sorted order.
    let mut pdf_idx: Option<usize> = None;
    let mut png_entries: Vec<(usize, String)> = Vec::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| Error::Tool(format!("zip entry {i}: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        let Some(base) = entry
            .enclosed_name()
            .and_then(|n| n.file_name().and_then(|b| b.to_str()).map(str::to_string))
        else {
            continue;
        };
        let lower = base.to_ascii_lowercase();
        if lower.ends_with(".pdf") {
            pdf_idx.get_or_insert(i);
        } else if lower.ends_with(".png") {
            png_entries.push((i, base));
        }
    }
    png_entries.sort_by(|a, b| a.1.cmp(&b.1));

    let extract_to = |archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
                      idx: usize,
                      out_name: &str|
     -> Result<()> {
        let mut entry = archive
            .by_index(idx)
            .map_err(|e| Error::Tool(format!("zip entry {idx}: {e}")))?;
        let dest = out_dir.join(out_name);
        let mut f = std::fs::File::create(&dest)
            .map_err(|e| Error::Tool(format!("write {}: {e}", dest.display())))?;
        std::io::copy(&mut entry, &mut f)
            .map_err(|e| Error::Tool(format!("extract {out_name}: {e}")))?;
        Ok(())
    };

    let pdf = if let Some(idx) = pdf_idx {
        extract_to(&mut archive, idx, "deck.pdf")?;
        true
    } else {
        false
    };
    for (n, (idx, _)) in png_entries.iter().enumerate() {
        extract_to(&mut archive, *idx, &format!("slide-{:02}.png", n + 1))?;
    }
    Ok((pdf, png_entries.len()))
}

pub struct RenderSlidesTool;

#[async_trait]
impl Tool for RenderSlidesTool {
    fn name(&self) -> &'static str {
        "RenderSlides"
    }
    fn description(&self) -> &'static str {
        "Render a Marp markdown slide deck into a PDF + per-slide PNG images via the thClaws \
         slide-render service (Marp + headless Chromium + Thai fonts). Pass the deck as \
         `markdown_path` (a .md/.marp.md file in the workspace) or inline `markdown`, an optional \
         `theme` (default `thai-tech`), an optional `image_dir` holding any figures/screenshots the \
         deck references by `<img src=\"name.png\">`, and an `out_dir`. Writes `<out_dir>/deck.pdf` \
         plus `slide-*.png`. Use for the thai-book-production slide pipeline (compile chNN.yaml → \
         Marp md with slides_yaml2marp.py, then render here). Requires `imageToolsEnabled: true` in \
         `.thclaws/settings.json`, plus either `SLIDE_RENDER_API` (self-hosted service) or the \
         thClaws Gateway key (metered). Backend-only: the desktop/GUI must never call the render API \
         directly — it goes through this tool."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "markdown_path": {
                    "type": "string",
                    "description": "Path to a Marp markdown deck inside the workspace (e.g. build/slides/ch01.marp.md). Provide this or `markdown`."
                },
                "markdown": {
                    "type": "string",
                    "description": "Inline Marp markdown deck. Use instead of `markdown_path` when the deck isn't on disk."
                },
                "theme": {
                    "type": "string",
                    "description": "CSS theme name known to the service. Default `thai-tech`."
                },
                "image_dir": {
                    "type": "string",
                    "description": "Optional workspace directory holding images the deck references by filename (png/jpg/webp/gif/svg). Sent alongside the markdown so `<img src=\"name.png\">` resolves."
                },
                "out_dir": {
                    "type": "string",
                    "description": "Workspace directory to write `deck.pdf` + `slide-*.png` into (created if missing). E.g. build/slides/preview/ch01."
                }
            },
            "required": ["out_dir"]
        })
    }
    fn requires_approval(&self, _input: &Value) -> bool {
        // Network call (may be metered via the gateway) + writes files.
        true
    }
    async fn call(&self, input: Value) -> Result<String> {
        let out_dir_raw = req_str(&input, "out_dir")?;
        let theme = {
            let t = opt(&input, "theme");
            if t.is_empty() {
                "thai-tech".to_string()
            } else {
                t
            }
        };

        // Resolve the markdown: inline wins, else read the file.
        let md_path = opt(&input, "markdown_path");
        let md_inline = opt(&input, "markdown");
        let markdown = if !md_inline.is_empty() {
            md_inline
        } else if !md_path.is_empty() {
            let abs = crate::sandbox::Sandbox::check(&md_path)
                .map_err(|e| Error::Tool(format!("markdown_path: {e}")))?;
            std::fs::read_to_string(&abs)
                .map_err(|e| Error::Tool(format!("read {}: {e}", abs.display())))?
        } else {
            return Err(Error::Tool(
                "provide `markdown_path` (a .marp.md file) or inline `markdown`".into(),
            ));
        };
        if markdown.trim().is_empty() {
            return Err(Error::Tool("markdown is empty — nothing to render".into()));
        }

        // Collect referenced images, if a dir was given.
        let image_dir = opt(&input, "image_dir");
        let images = if image_dir.is_empty() {
            Vec::new()
        } else {
            let abs = crate::sandbox::Sandbox::check(&image_dir)
                .map_err(|e| Error::Tool(format!("image_dir: {e}")))?;
            if abs.is_dir() {
                collect_images(&abs)?
            } else {
                Vec::new()
            }
        };

        // Sandbox-check + create the output directory.
        let out_dir = crate::sandbox::Sandbox::check_write(out_dir_raw)
            .map_err(|e| Error::Tool(format!("out_dir: {e}")))?;
        std::fs::create_dir_all(&out_dir)
            .map_err(|e| Error::Tool(format!("mkdir {}: {e}", out_dir.display())))?;

        let ep = resolve_slide_endpoint()?;

        let img_count = images.len();
        let mut form = Form::new()
            .text("markdown", markdown)
            .text("theme", theme.clone());
        for (name, bytes) in images {
            let mime = mime_of(&name);
            let part = Part::bytes(bytes)
                .file_name(name)
                .mime_str(mime)
                .map_err(|e| Error::Tool(format!("multipart: {e}")))?;
            form = form.part("images", part);
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| Error::Tool(format!("http client: {e}")))?;
        let mut rb = crate::multi_tenant::attach_member(client.post(&ep.url));
        if let Some(ref token) = ep.bearer {
            rb = rb.bearer_auth(token);
        }
        let resp = rb
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Tool(format!("slide-render request: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Tool(format!(
                "slide-render http {status}: {}",
                &body[..body.len().min(400)]
            )));
        }
        let zip_bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Tool(format!("read render response: {e}")))?;

        let (pdf, pngs) = extract_deck(&zip_bytes, &out_dir)?;
        if !pdf && pngs == 0 {
            return Err(Error::Tool(
                "slide-render returned no pdf or png — check the deck / theme".into(),
            ));
        }
        let via = if ep.via_gateway { "gateway" } else { "direct" };
        Ok(format!(
            "Rendered {pngs} slide(s){} → {} ({} theme, {img_count} image(s) sent, via {via})",
            if pdf { " + deck.pdf" } else { "" },
            out_dir.display(),
            theme,
        ))
    }
}

// ─── .pptx preview (Files tab) ───────────────────────────────────────

/// Where a rendered deck is cached, relative to the workspace root.
///
/// Project-level, not `~/.config`: two projects routinely hold a
/// `slides.pptx` apiece, and a user-level cache keyed on filename would
/// serve one project's deck to the other. Excluded from `/cloud push`
/// (see `cloud::wssync`) — it's derived bytes, several MB per deck, and
/// the far end can re-render on demand.
pub const PPTX_CACHE_REL: &str = ".thclaws/state/pptx-preview";

/// True when a deck can be rendered right now — i.e. a slide-render
/// endpoint resolves (self-hosted URL, or a valid gateway key). The
/// Files tab calls this before auto-rendering so a signed-out user
/// falls back to the text extraction instead of failing.
pub fn pptx_preview_available() -> bool {
    resolve_slide_endpoint_for("pptx").is_ok()
}

/// What a caller should do with a `.pptx` right now, decided WITHOUT
/// touching the network.
///
/// The Files tab re-reads the open file on a timer, so "just try to
/// render and see" isn't viable: a deck that can't be rendered would
/// retry on every tick, and the pane flipped between "Rendering…" and
/// the text fallback for as long as it stayed open. Deciding up front
/// makes each read deterministic — and a cache hit skips the spawn
/// entirely instead of flashing a spinner at an already-rendered deck.
pub enum PptxPreview {
    /// Rendered earlier; this is the PDF.
    Ready(std::path::PathBuf),
    /// Won't be attempted — oversized, or a previous attempt failed.
    /// Carries the reason so the caller can show it with the text.
    Skip(String),
    /// Not cached, worth rendering.
    Renderable,
}

/// Marker written into the cache dir when a render fails, so the next
/// read doesn't repeat a call we already know ends badly. Removing the
/// cache dir (or editing the deck, which moves the hash) retries.
const RENDER_FAILED_MARKER: &str = "render-failed.txt";

pub fn pptx_preview_state(workspace: &Path, deck: &Path) -> PptxPreview {
    let Ok(meta) = std::fs::metadata(deck) else {
        return PptxPreview::Skip("file is unreadable".into());
    };
    if meta.len() > MAX_PPTX_BYTES {
        return PptxPreview::Skip(format!(
            "deck is {:.1} MB, over the {:.0} MB render limit",
            meta.len() as f64 / 1_048_576.0,
            MAX_PPTX_BYTES as f64 / 1_048_576.0,
        ));
    }
    let Ok(dir) = pptx_cache_dir(workspace, deck) else {
        return PptxPreview::Renderable;
    };
    let pdf = dir.join("deck.pdf");
    if pdf.is_file() {
        return PptxPreview::Ready(pdf);
    }
    if let Ok(reason) = std::fs::read_to_string(dir.join(RENDER_FAILED_MARKER)) {
        return PptxPreview::Skip(reason.trim().to_string());
    }
    PptxPreview::Renderable
}

/// Record a failure so `pptx_preview_state` can skip the retry.
fn mark_render_failed(workspace: &Path, deck: &Path, reason: &str) {
    let Ok(dir) = pptx_cache_dir(workspace, deck) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(RENDER_FAILED_MARKER), reason);
    }
}

/// Cache directory for `deck`, keyed on its CONTENT — edit the file and
/// the hash moves, so a stale render can't be served, and reverting an
/// edit reuses the earlier one.
fn pptx_cache_dir(workspace: &Path, deck: &Path) -> Result<std::path::PathBuf> {
    use sha2::{Digest, Sha256};
    let bytes =
        std::fs::read(deck).map_err(|e| Error::Tool(format!("read {}: {e}", deck.display())))?;
    let digest = Sha256::digest(&bytes);
    let short: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok(workspace.join(PPTX_CACHE_REL).join(short))
}

/// Render `deck` to a PDF + per-slide PNGs, returning the PDF path.
///
/// Cached: a second open of an unchanged deck costs nothing (no upload,
/// no credits). Renders through the same service `RenderSlides` uses —
/// the Files tab previously showed only extracted text for a .pptx, and
/// requiring LibreOffice on every user's machine isn't realistic
/// (Windows especially), so the conversion happens server-side.
pub async fn render_pptx_preview(workspace: &Path, deck: &Path) -> Result<std::path::PathBuf> {
    let dir = pptx_cache_dir(workspace, deck)?;
    let pdf = dir.join("deck.pdf");
    if pdf.is_file() {
        return Ok(pdf);
    }

    let bytes =
        std::fs::read(deck).map_err(|e| Error::Tool(format!("read {}: {e}", deck.display())))?;
    if bytes.len() as u64 > MAX_PPTX_BYTES {
        return Err(Error::Tool(format!(
            "deck is {:.1} MB — over the {:.0} MB render limit",
            bytes.len() as f64 / 1_048_576.0,
            MAX_PPTX_BYTES as f64 / 1_048_576.0,
        )));
    }
    let name = deck
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("deck.pptx")
        .to_string();

    let ep = resolve_slide_endpoint_for("pptx")?;
    let part = Part::bytes(bytes)
        .file_name(name)
        .mime_str("application/vnd.openxmlformats-officedocument.presentationml.presentation")
        .map_err(|e| Error::Tool(format!("multipart: {e}")))?;
    let form = Form::new().part("file", part);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| Error::Tool(format!("http client: {e}")))?;
    let mut rb = crate::multi_tenant::attach_member(client.post(&ep.url));
    if let Some(ref token) = ep.bearer {
        rb = rb.bearer_auth(token);
    }
    let resp = rb
        .multipart(form)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("slide-render request: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let reason = format!(
            "slide-render http {status}: {}",
            &body[..body.len().min(400)]
        );
        // A 4xx is about THIS deck (unsupported, corrupt, too large for
        // the service) and will fail identically next time — remember
        // it. A 5xx or a transport error might not, so those stay
        // retryable.
        if status.is_client_error() {
            mark_render_failed(workspace, deck, &reason);
        }
        return Err(Error::Tool(reason));
    }
    let zip_bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::Tool(format!("read render response: {e}")))?;

    std::fs::create_dir_all(&dir).map_err(|e| Error::Tool(format!("mkdir cache: {e}")))?;
    let (has_pdf, pngs) = extract_deck(&zip_bytes, &dir)?;
    if !has_pdf {
        // Leave nothing half-written behind, or the `pdf.is_file()`
        // check above would serve a broken cache entry forever.
        let _ = std::fs::remove_dir_all(&dir);
        return Err(Error::Tool(format!(
            "slide-render returned {pngs} png(s) but no pdf"
        )));
    }
    Ok(pdf)
}

/// Ceiling on a deck we'll upload. Rejecting here gives the user a
/// reason ("deck is 240 MB, over the 200 MB render limit") instead of a
/// bare 413 from the gateway. Kept just under the two caps behind it —
/// the gateway buffers at 220 MB, the service's multipart at 200 MB —
/// so this is the limit that actually speaks.
const MAX_PPTX_BYTES: u64 = 200 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// The Files tab re-reads an open file on a timer, so this decision
    /// has to be STABLE: an unrenderable deck that answered
    /// "Renderable" on one tick and "Skip" on the next made the pane
    /// flip between the spinner and the text fallback forever.
    #[test]
    fn preview_state_is_stable_across_repeated_reads() {
        let ws = tempfile::tempdir().unwrap();
        let deck = ws.path().join("talk.pptx");
        std::fs::write(&deck, b"PK\x03\x04 not really a deck").unwrap();

        // Nothing cached yet → worth a try, and asking twice doesn't
        // change the answer.
        for _ in 0..3 {
            assert!(matches!(
                pptx_preview_state(ws.path(), &deck),
                PptxPreview::Renderable
            ));
        }

        // After a failure is recorded, every later read skips — same
        // branch each time, which is what stops the flip-flop.
        mark_render_failed(ws.path(), &deck, "slide-render http 422: unsupported");
        for _ in 0..3 {
            match pptx_preview_state(ws.path(), &deck) {
                PptxPreview::Skip(reason) => assert!(reason.contains("422"), "{reason}"),
                other => panic!("expected Skip, got {}", state_name(&other)),
            }
        }

        // Editing the deck moves the content hash, so the failure no
        // longer applies and it becomes retryable.
        std::fs::write(&deck, b"PK\x03\x04 different bytes entirely").unwrap();
        assert!(matches!(
            pptx_preview_state(ws.path(), &deck),
            PptxPreview::Renderable
        ));
    }

    #[test]
    fn oversized_deck_skips_without_touching_the_cache() {
        let ws = tempfile::tempdir().unwrap();
        let deck = ws.path().join("huge.pptx");
        std::fs::write(&deck, vec![0u8; (MAX_PPTX_BYTES + 1) as usize]).unwrap();
        match pptx_preview_state(ws.path(), &deck) {
            PptxPreview::Skip(reason) => {
                assert!(reason.contains("over the"), "{reason}");
                assert!(reason.contains("MB"), "{reason}");
            }
            other => panic!("expected Skip, got {}", state_name(&other)),
        }
        // No render was attempted, so nothing should have been written.
        assert!(!ws.path().join(PPTX_CACHE_REL).exists());
    }

    #[test]
    fn cached_render_is_served_without_a_spawn() {
        let ws = tempfile::tempdir().unwrap();
        let deck = ws.path().join("talk.pptx");
        std::fs::write(&deck, b"deck bytes").unwrap();
        let dir = pptx_cache_dir(ws.path(), &deck).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deck.pdf"), b"%PDF-1.7").unwrap();

        match pptx_preview_state(ws.path(), &deck) {
            PptxPreview::Ready(pdf) => assert!(pdf.ends_with("deck.pdf")),
            other => panic!("expected Ready, got {}", state_name(&other)),
        }
    }

    fn state_name(s: &PptxPreview) -> &'static str {
        match s {
            PptxPreview::Ready(_) => "Ready",
            PptxPreview::Skip(_) => "Skip",
            PptxPreview::Renderable => "Renderable",
        }
    }

    #[test]
    fn mime_by_ext() {
        assert_eq!(mime_of("a.PNG"), "image/png");
        assert_eq!(mime_of("shot.jpeg"), "image/jpeg");
        assert_eq!(mime_of("fig.webp"), "image/webp");
        assert_eq!(mime_of("noext"), "image/png");
    }

    #[test]
    fn native_endpoint_beats_gateway() {
        std::env::set_var("SLIDE_RENDER_API", "http://localhost:8080/");
        std::env::set_var("SLIDE_RENDER_TOKEN", "tok");
        let ep = resolve_slide_endpoint().unwrap();
        assert_eq!(ep.url, "http://localhost:8080/slides");
        assert_eq!(ep.bearer.as_deref(), Some("tok"));
        assert!(!ep.via_gateway);
        std::env::remove_var("SLIDE_RENDER_API");
        std::env::remove_var("SLIDE_RENDER_TOKEN");
    }
}
