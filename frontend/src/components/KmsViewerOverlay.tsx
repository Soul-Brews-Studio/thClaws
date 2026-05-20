import { useEffect, useMemo, useRef, useState } from "react";
import { X, ArrowLeft, Loader2 } from "lucide-react";
import { marked } from "marked";
import { send, subscribe } from "../hooks/useIPC";
import type { ViewerTarget } from "./KmsBrowserSidebar";

/// M6.39.9: KMS viewer pane. Renders a KMS file as HTML inside the
/// main content area — replaces the active tab visually, but tabs
/// stay mounted so xterm/etc don't lose state. Mounted as an
/// `absolute inset-0` sibling inside the main-pane container; close
/// returns the user to whichever tab they were on.
///
/// Markdown → HTML via `marked` (already a dep, used by
/// MarkdownEditor / InstructionsEditorModal too). Click handlers
/// rewrite links so:
///   - `[[<run-prefix>__<slug>]]` Obsidian wikilinks → load that
///     page in the same pane
///   - relative markdown links `[..](../sources/foo.md)` and
///     `[..](other-page.md)` → load that page/source in the pane
///   - http(s) links → open in external browser via `open_external`
///     IPC (delegates to the OS default browser; doesn't navigate
///     the wry webview which is single-document)
///
/// Keeps a small back-stack so the user can step backward through
/// linked pages. ESC + the X button close the pane; ArrowLeft in
/// the title bar pops the back-stack one entry.

marked.setOptions({ gfm: true, breaks: false, async: false });

interface Props {
  initial: ViewerTarget;
  onClose: () => void;
}

export function KmsViewerOverlay({ initial, onClose }: Props) {
  const [stack, setStack] = useState<ViewerTarget[]>([initial]);
  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const current = stack[stack.length - 1];

  // Reset stack when `initial` changes (parent opens a different file).
  // Clear `content` in the same effect so the viewer shows the spinner
  // on the very next render — otherwise the old file's HTML flashes
  // briefly under the new title before the fetch effect clears it.
  useEffect(() => {
    setStack([initial]);
    setContent(null);
    setError(null);
  }, [initial.kms, initial.kind, initial.name]);

  // Fetch content for the top-of-stack file.
  useEffect(() => {
    setContent(null);
    setError(null);
    const unsub = subscribe((msg) => {
      if (
        msg.type === "kms_file_content" &&
        msg.kms === current.kms &&
        msg.kind === current.kind &&
        msg.name === current.name
      ) {
        if (msg.ok) {
          setContent(msg.content as string);
        } else {
          setError((msg.error as string) ?? "read failed");
        }
      }
    });
    send({
      type: "kms_read_file",
      kms: current.kms,
      kind: current.kind,
      name: current.name,
    });
    return unsub;
  }, [current.kms, current.kind, current.name]);

  // ESC closes the overlay. Listener attaches on the document so it
  // fires regardless of focus.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [onClose]);

  const html = useMemo(() => {
    if (content === null) return "";
    return renderMarkdownToHtml(content);
  }, [content]);

  // Intercept clicks on rendered anchors. Resolve KMS-internal
  // targets (wikilinks, relative paths) into back-stack pushes;
  // delegate http(s) links to the OS browser.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      const anchor = target?.closest("a") as HTMLAnchorElement | null;
      if (!anchor) return;
      const href = anchor.getAttribute("href");
      if (!href) return;
      e.preventDefault();
      // External link: hand to OS browser. No wry navigation.
      if (/^https?:\/\//i.test(href)) {
        send({ type: "open_external", url: href });
        return;
      }
      // Wikilink converted to `wikilink:slug` href by our renderer.
      if (href.startsWith("wikilink:")) {
        const slug = href.slice("wikilink:".length);
        // Wikilinks in research output use the prefixed-filename form
        // already (rewriter applied at synth time). Treat as a page.
        setStack((s) => [...s, { kms: current.kms, kind: "page", name: slug }]);
        return;
      }
      // Relative markdown link → resolve relative to current file's
      // directory inside the KMS.
      if (href.endsWith(".md") || href.includes(".md#") || href.includes(".md?")) {
        const target = resolveRelativeLink(current, href);
        if (target) {
          setStack((s) => [...s, target]);
          return;
        }
      }
      // Other href shapes (anchor-only `#section`, mailto:, etc.) —
      // ignore the click; preventDefault stops the wry default but
      // we don't navigate anywhere either.
    };
    container.addEventListener("click", handler);
    return () => container.removeEventListener("click", handler);
  }, [current, html]);

  const goBack = () => {
    setStack((s) => (s.length > 1 ? s.slice(0, -1) : s));
  };

  return (
    <div
      className="absolute inset-0 flex flex-col"
      style={{
        background: "var(--bg-primary)",
        zIndex: 30, // above the tabs, below modals (which use fixed z-50)
      }}
    >
      <div
        className="flex items-center justify-between px-4 py-2 border-b shrink-0"
        style={{
          borderColor: "var(--border)",
          background: "var(--bg-secondary)",
        }}
      >
        <div className="flex items-center gap-2 truncate">
          <button
            type="button"
            onClick={goBack}
            disabled={stack.length <= 1}
            className="p-1 rounded hover:bg-white/10"
            style={{
              color: "var(--text-secondary)",
              opacity: stack.length <= 1 ? 0.3 : 1,
              cursor: stack.length <= 1 ? "default" : "pointer",
            }}
            title="Back"
          >
            <ArrowLeft size={14} />
          </button>
          <span
            className="text-xs"
            style={{ color: "var(--text-secondary)" }}
          >
            {current.kms} / {current.kind}s /
          </span>
          <span
            className="text-sm font-semibold truncate"
            style={{ color: "var(--text-primary)" }}
          >
            {current.name}
          </span>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="p-1 rounded hover:bg-white/10"
          style={{ color: "var(--text-secondary)" }}
          title="Close (Esc) — return to active tab"
        >
          <X size={14} />
        </button>
      </div>

      <div
        ref={containerRef}
        className="flex-1 overflow-auto kms-viewer-prose"
        style={{ color: "var(--text-primary)" }}
      >
        <div className="max-w-4xl mx-auto px-8 py-6">
          {error && (
            <div
              className="px-3 py-2 rounded"
              style={{
                background: "var(--bg-secondary)",
                color: "var(--danger, #e06c75)",
              }}
            >
              {error}
            </div>
          )}
          {content === null && !error && (
            <div
              className="px-3 py-2 italic text-sm flex items-center gap-2"
              style={{ color: "var(--text-secondary)" }}
            >
              <Loader2 size={14} className="animate-spin" />
              <span>Loading…</span>
            </div>
          )}
          {content !== null && (
            <div dangerouslySetInnerHTML={{ __html: html }} />
          )}
        </div>
      </div>
    </div>
  );
}

/// Render Markdown → HTML, with two pre-processing passes for
/// KMS-specific syntax that vanilla `marked` doesn't understand:
///
/// 1. Strip the YAML frontmatter block at the very top (between
///    `---\n` and `\n---\n`) — no point rendering it as a code block.
/// 2. Convert Obsidian `[[slug]]` and `[[slug|display]]` wikilinks
///    into anchor tags with a custom `wikilink:` href scheme. The
///    overlay's click handler intercepts those and pushes a new
///    target onto the back-stack.
function renderMarkdownToHtml(markdown: string): string {
  let body = stripFrontmatter(markdown);
  body = rewriteWikilinks(body);
  return marked.parse(body) as string;
}

function stripFrontmatter(s: string): string {
  if (!s.startsWith("---\n")) return s;
  const end = s.indexOf("\n---\n", 4);
  if (end < 0) return s;
  return s.slice(end + "\n---\n".length).trimStart();
}

function rewriteWikilinks(s: string): string {
  // Convert `[[slug]]` → `[slug](wikilink:slug)`
  //         `[[slug|display]]` → `[display](wikilink:slug)`
  // Markdown then lets `marked` render these as ordinary anchors.
  // Keep it simple — the rewriter runs BEFORE marked so we just
  // emit markdown link syntax.
  let out = "";
  let i = 0;
  while (i < s.length) {
    if (i + 1 < s.length && s[i] === "[" && s[i + 1] === "[") {
      const end = s.indexOf("]]", i + 2);
      if (end > 0 && end - i - 2 <= 200) {
        const inner = s.slice(i + 2, end);
        if (!inner.includes("\n")) {
          const pipe = inner.indexOf("|");
          const slug = pipe >= 0 ? inner.slice(0, pipe).trim() : inner.trim();
          const display =
            pipe >= 0 ? inner.slice(pipe + 1).trim() : inner.trim();
          if (slug.length > 0) {
            out += `[${escapeMd(display)}](wikilink:${encodeURIComponent(slug)})`;
            i = end + 2;
            continue;
          }
        }
      }
    }
    out += s[i];
    i++;
  }
  return out;
}

function escapeMd(s: string): string {
  return s.replace(/([\\\[\]])/g, "\\$1");
}

/// Resolve a relative markdown link from the perspective of the
/// currently-viewed file. Pages live at `<kms>/pages/`, sources at
/// `<kms>/sources/`. Common shapes our pipeline emits:
///   `[[slug]]` → handled separately as `wikilink:` scheme
///   `[T](../sources/<slug>.md)` from a page → resolves to `source`
///   `[T](other-page.md)` from a page → resolves to `page`
///   `[T](../pages/<slug>.md)` from a source → resolves to `page`
function resolveRelativeLink(
  current: ViewerTarget,
  href: string,
): ViewerTarget | null {
  // Strip query / fragment.
  let path = href.split("#")[0].split("?")[0];
  // Always lowercase the kind segment for matching.
  if (path.startsWith("../sources/") && path.endsWith(".md")) {
    const name = path.slice("../sources/".length, -3);
    if (!name.includes("/")) {
      return { kms: current.kms, kind: "source", name };
    }
  }
  if (path.startsWith("../pages/") && path.endsWith(".md")) {
    const name = path.slice("../pages/".length, -3);
    if (!name.includes("/")) {
      return { kms: current.kms, kind: "page", name };
    }
  }
  // Bare filename `<slug>.md` — resolve as same-kind sibling.
  if (path.endsWith(".md") && !path.includes("/")) {
    const name = path.slice(0, -3);
    return { kms: current.kms, kind: current.kind, name };
  }
  return null;
}
