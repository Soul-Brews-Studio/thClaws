//! Filesystem-preview helpers powering the Files-tab IPC arms (and the
//! `--serve` mode equivalents). All three lifted from `gui.rs` in M6.36
//! SERVE9i so the WS transport's `file_list` / `file_read` / `file_write`
//! IPC arms can call them from the always-on dispatch table:
//!
//! - [`ospath`] — Windows path slash translator (no-op elsewhere)
//! - [`csv_to_markdown_table`] — CSV → GFM pipe-table for in-iframe preview
//! - [`render_markdown_to_html`] — themed standalone HTML doc wrapping
//!   GFM-rendered markdown (sandboxed iframe consumer)

use base64::Engine;

/// Convert a frontend-supplied path (always slash-separated, since it
/// comes from JSON / the React tree) to the OS-native form before
/// passing it to filesystem APIs. No-op on macOS/Linux. On Windows,
/// translates `/` → `\` so paths like `src/api/foo.ts` resolve via
/// `Sandbox::check` instead of being rejected as malformed.
pub fn ospath(path: &str) -> String {
    #[cfg(not(target_os = "windows"))]
    {
        path.to_string()
    }
    #[cfg(target_os = "windows")]
    {
        path.replace('/', "\\")
    }
}

/// Convert a CSV string to a GFM markdown pipe-table. First row is
/// treated as the header. Pipe characters in cells are escaped (`\|`)
/// so they don't break the row structure. Empty input → empty string.
/// Used by `file_read` to preview spreadsheet extracts via the same
/// markdown→HTML pipeline as `.md` files.
pub fn csv_to_markdown_table(csv: &str) -> String {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(csv.as_bytes());
    let rows: Vec<Vec<String>> = rdr
        .records()
        .filter_map(|r| r.ok())
        .map(|r| {
            r.iter()
                .map(|c| c.replace('|', "\\|").replace('\n', " "))
                .collect()
        })
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut out = String::new();
    let pad = |row: &[String], cols: usize| {
        let mut line = String::from("|");
        for i in 0..cols {
            line.push(' ');
            line.push_str(row.get(i).map(String::as_str).unwrap_or(""));
            line.push_str(" |");
        }
        line.push('\n');
        line
    };
    out.push_str(&pad(&rows[0], cols));
    let mut sep = String::from("|");
    for _ in 0..cols {
        sep.push_str(" --- |");
    }
    sep.push('\n');
    out.push_str(&sep);
    for row in &rows[1..] {
        out.push_str(&pad(row, cols));
    }
    out
}

/// `---` on its own line, 3+ dashes, unindented — the shape that is both
/// a thematic break and a setext underline.
fn is_rule_line(line: &str) -> bool {
    let t = line.trim_end();
    t.len() >= 3 && t.chars().all(|c| c == '-')
}

/// Given the index of an opening `---`, the index of the `---` that
/// closes a YAML block — i.e. every line between the two is non-blank
/// (a blank line means the author meant two separate rules) and the
/// first one reads as YAML (`key:` or `- item`). `None` when the pair
/// isn't a YAML block.
fn yaml_block_end(lines: &[&str], open: usize) -> Option<usize> {
    let first = lines.get(open + 1)?.trim_end();
    let key = first.split_once(':').is_some_and(|(k, _)| {
        !k.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    });
    if !key && !first.starts_with("- ") {
        return None;
    }
    for (i, line) in lines.iter().enumerate().skip(open + 1) {
        if line.trim().is_empty() {
            return None;
        }
        if is_rule_line(line) {
            return Some(i);
        }
    }
    None
}

/// Pre-process markdown so `---` renders the way the author meant it.
/// Two CommonMark traps, both of which the Files tab hits constantly:
///
/// - A paragraph line followed directly by `---` is a setext H2, so a
///   section separator silently turns the line above it into a heading.
///   Insert the blank line that makes it a thematic break instead.
/// - A `---` pair with no blank line between the fences and their
///   contents is YAML (frontmatter at the top of the file, metadata
///   blocks further down) — the closing `---` underlines the last key,
///   so `model: bar` becomes a heading. Re-emit those as a yaml code
///   block, which also keeps one key per line.
///
/// Fenced code is left untouched, and so is a `---` after a list item /
/// blockquote / table row (already a thematic break there — forcing a
/// blank line would only loosen the list).
fn normalize_rules(md: &str) -> String {
    let lines: Vec<&str> = md.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 4);
    let mut fence: Option<(char, usize)> = None;
    let mut prev_blank = true;
    let mut prev_paragraph = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let body = line.trim_end_matches('\r');
        let trimmed = body.trim_start();
        let indent = body.len() - trimmed.len();
        let run = |ch: char| trimmed.chars().take_while(|c| *c == ch).count();
        if let Some((ch, len)) = fence {
            if indent <= 3 && run(ch) >= len && trimmed.trim_end().chars().all(|c| c == ch) {
                fence = None;
            }
        } else if indent <= 3 && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            let ch = trimmed.as_bytes()[0] as char;
            fence = Some((ch, run(ch)));
        } else if indent == 0 && is_rule_line(trimmed) {
            if prev_blank {
                if let Some(end) = yaml_block_end(&lines, i) {
                    out.push("```yaml".into());
                    out.extend(lines[i + 1..end].iter().map(|l| (*l).to_string()));
                    out.push("```".into());
                    out.push(String::new());
                    i = end + 1;
                    prev_blank = true;
                    prev_paragraph = false;
                    continue;
                }
            } else if prev_paragraph {
                out.push(String::new());
            }
        }
        out.push(line.to_string());
        prev_blank = trimmed.is_empty();
        prev_paragraph = fence.is_none()
            && indent == 0
            && !trimmed.is_empty()
            && !trimmed.starts_with(['-', '*', '+', '>', '#', '|', '='])
            && !trimmed
                .split_once(['.', ')'])
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        i += 1;
    }
    out.join("\n")
}

/// Convert a markdown string to a full standalone HTML document
/// (sandboxed iframe consumer). GFM extensions: tables, strikethrough,
/// task lists, autolinks, footnotes, header ids. Raw HTML in source is
/// stripped (`render.unsafe_ = false`) so a `<script>` in a `.md` file
/// can't escape the iframe sandbox.
///
/// `theme` must be the *resolved* value (`"light"` or `"dark"`); the
/// frontend resolves `"system"` before sending so this function never
/// inspects an OS signal. Default = dark for back-compat when caller
/// passes anything else.
pub fn render_markdown_to_html(md: &str, theme: &str) -> String {
    let mut opts = comrak::ComrakOptions::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.tasklist = true;
    opts.extension.autolink = true;
    opts.extension.footnotes = true;
    opts.extension.header_ids = Some(String::new());
    opts.render.unsafe_ = false;

    // Frontmatter is shown as a yaml block rather than dropped — the
    // Files tab is a file viewer, so hiding part of the file is worse
    // than showing it verbatim.
    let body = comrak::markdown_to_html(&normalize_rules(md), &opts);

    let (fg, bg, muted, accent, code_bg, border, color_scheme) = if theme == "light" {
        (
            "#1a1a1a", "#ffffff", "#606366", "#2867c4", "#f3f4f6", "#d0d7de", "light",
        )
    } else {
        (
            "#e6e6e6", "#1a1a1a", "#9aa0a6", "#6cb0ff", "#2a2a2a", "#333", "dark",
        )
    };

    format!(
        r##"<!DOCTYPE html>
<html><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root {{
    color-scheme: {color_scheme};
    --fg: {fg};
    --bg: {bg};
    --muted: {muted};
    --accent: {accent};
    --code-bg: {code_bg};
    --border: {border};
  }}
  html, body {{ margin: 0; padding: 0; }}
  body {{
    font: 14px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI",
          "Helvetica Neue", Arial, "Noto Sans Thai", sans-serif;
    color: var(--fg); background: var(--bg); padding: 24px 32px;
    max-width: 880px; margin: 0 auto;
  }}
  h1, h2, h3, h4, h5, h6 {{ line-height: 1.25; margin: 1.4em 0 0.5em; }}
  h1 {{ font-size: 1.8em; border-bottom: 1px solid var(--border); padding-bottom: 0.3em; }}
  h2 {{ font-size: 1.4em; border-bottom: 1px solid var(--border); padding-bottom: 0.25em; }}
  h3 {{ font-size: 1.2em; }}
  p, ul, ol, blockquote, pre, table {{ margin: 0.8em 0; }}
  a {{ color: var(--accent); text-decoration: none; }}
  a:hover {{ text-decoration: underline; }}
  code {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
          font-size: 0.92em; background: var(--code-bg);
          padding: 2px 5px; border-radius: 3px; }}
  pre {{ background: var(--code-bg); padding: 12px 14px; border-radius: 6px;
         overflow-x: auto; }}
  pre code {{ background: transparent; padding: 0; font-size: 0.9em; }}
  blockquote {{ margin: 0.8em 0; padding: 0 1em; color: var(--muted);
                border-left: 3px solid var(--border); }}
  table {{ border-collapse: collapse; }}
  th, td {{ border: 1px solid var(--border); padding: 6px 12px; text-align: left; }}
  th {{ background: var(--code-bg); font-weight: 600; }}
  hr {{ border: 0; border-top: 1px solid var(--border); margin: 2em 0; }}
  img {{ max-width: 100%; height: auto; }}
  ul.contains-task-list {{ list-style: none; padding-left: 1em; }}
  .task-list-item input[type="checkbox"] {{ margin-right: 0.5em; }}
</style>
</head><body>
{body}
</body></html>"##,
        body = body
    )
}

/// Base64-encode a binary file's bytes for the `file_content` envelope's
/// `content` field. Pure convenience wrapper around the standard
/// engine — saved as a top-level helper so the IPC layer doesn't have
/// to import the base64 crate just for this one call.
pub fn encode_bytes_b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ospath_is_noop_on_unix() {
        // CI runs Unix; Windows behavior is checked at compile time
        // via the cfg branches.
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(ospath("src/api/foo.ts"), "src/api/foo.ts");
            assert_eq!(ospath(""), "");
        }
    }

    #[test]
    fn csv_to_markdown_renders_headers_and_rows() {
        let csv = "name,age\nAlice,30\nBob,25";
        let md = csv_to_markdown_table(csv);
        assert!(md.contains("| name | age |"));
        assert!(md.contains("| --- | --- |"));
        assert!(md.contains("| Alice | 30 |"));
        assert!(md.contains("| Bob | 25 |"));
    }

    #[test]
    fn csv_to_markdown_preserves_thai_cells() {
        // Migrated from gui.rs::csv_table_tests in M6.36 SERVE9k —
        // pinning that the markdown rendering doesn't break on
        // multi-byte UTF-8.
        let md = csv_to_markdown_table("ชื่อ,อายุ\nสมชาย,25");
        assert!(md.contains("ชื่อ"));
        assert!(md.contains("สมชาย"));
    }

    #[test]
    fn csv_to_markdown_escapes_pipe_characters() {
        let csv = "col1,col2\n\"a|b\",c";
        let md = csv_to_markdown_table(csv);
        assert!(md.contains("a\\|b"));
    }

    #[test]
    fn csv_to_markdown_empty_input_yields_empty_string() {
        assert_eq!(csv_to_markdown_table(""), "");
    }

    #[test]
    fn render_markdown_includes_body_and_theme_palette() {
        let html = render_markdown_to_html("# Hello\n\nworld", "light");
        assert!(html.contains("<h1"));
        assert!(html.contains(">Hello"));
        assert!(html.contains(">world"));
        assert!(html.contains("color-scheme: light"));
    }

    #[test]
    fn dashes_after_a_paragraph_render_as_a_rule_not_a_heading() {
        let html = render_markdown_to_html("Intro line\n---\nNext line\n", "dark");
        assert!(html.contains("<hr"), "no rule: {html}");
        assert!(!html.contains("<h2"), "setext heading survived: {html}");
    }

    #[test]
    fn dashes_inside_a_code_fence_are_left_alone() {
        let html = render_markdown_to_html("```\nkey: v\n---\nkey2: v\n```\n", "dark");
        assert!(!html.contains("<hr"), "rule leaked into code: {html}");
        assert!(html.contains("---"), "fence content lost: {html}");
    }

    #[test]
    fn list_items_keep_their_thematic_break() {
        // `- a` + `---` is already a rule in CommonMark; the fix must not
        // inject a blank line there and turn the list loose.
        let html = render_markdown_to_html("- a\n- b\n---\n", "dark");
        assert!(html.contains("<hr"), "no rule: {html}");
        assert!(!html.contains("<li><p>"), "list went loose: {html}");
    }

    #[test]
    fn frontmatter_renders_as_yaml_not_a_heading() {
        let html = render_markdown_to_html("---\nname: foo\nmodel: bar\n---\n\nBody\n", "dark");
        assert!(
            !html.contains("<h2"),
            "frontmatter became a heading: {html}"
        );
        assert!(html.contains("name: foo"), "frontmatter lost: {html}");
        assert!(html.contains(">Body"), "body lost: {html}");
    }

    #[test]
    fn mid_document_yaml_block_renders_as_yaml() {
        // A `---` pair with no blank line against its contents is a
        // metadata block wherever it sits, not two rules.
        let html =
            render_markdown_to_html("Intro\n\n---\nid: s01\nmodel: bar\n---\n\nAfter\n", "dark");
        assert!(html.contains("<code"), "not a code block: {html}");
        assert!(html.contains("id: s01"), "yaml lost: {html}");
        assert!(!html.contains("<hr"), "rendered as rules: {html}");
        assert!(html.contains(">After"), "body after the block lost: {html}");
    }

    #[test]
    fn dashes_with_a_blank_line_stay_two_rules() {
        let html = render_markdown_to_html("---\n\nname: foo\n\n---\n", "dark");
        assert_eq!(html.matches("<hr").count(), 2, "not two rules: {html}");
    }

    #[test]
    fn prose_between_dashes_is_not_treated_as_yaml() {
        let html = render_markdown_to_html("A\n\n---\njust prose here\n---\n\nB\n", "dark");
        assert!(!html.contains("<code"), "prose became yaml: {html}");
        assert!(html.contains("just prose here"), "prose lost: {html}");
    }

    #[test]
    fn setext_headings_still_work_with_a_short_underline() {
        let html = render_markdown_to_html("Title\n-\n", "dark");
        assert!(html.contains("<h2"), "setext heading lost: {html}");
    }

    #[test]
    fn render_markdown_strips_raw_html_for_safety() {
        // unsafe_ = false → comrak refuses to emit raw HTML in source
        // markdown. The exact rendering varies (comrak emits an HTML
        // comment placeholder), but the live <script> tag must NOT
        // appear. Pin the safety invariant rather than the exact
        // rendering — sandboxed iframe is the actual defense.
        let html = render_markdown_to_html("# Hi\n\n<script>alert(1)</script>", "dark");
        assert!(
            !html.contains("<script>alert"),
            "live <script> survived render: {html}"
        );
    }
}
