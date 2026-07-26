import { useEditor, EditorContent } from "@tiptap/react";
import { useEffect, useRef } from "react";
import {
  MARKDOWN_NODE_CSS,
  editorHtmlToMarkdown,
  markdownExtensions,
  markdownToEditorHtml,
  joinFrontmatter,
  splitFrontmatter,
} from "../lib/markdownRoundTrip";

interface Props {
  source: string;
  onChange: (markdown: string) => void;
  /// Directory of the file being edited, so relative `![](img/x.png)`
  /// references resolve through the /file-asset handler. Omit for
  /// content with no on-disk location — images still round-trip, they
  /// just won't render.
  baseDir?: string;
}

export function MarkdownEditor({ source, onChange, baseDir }: Props) {
  // Track the last markdown we emitted so an echoed `source` prop
  // doesn't reset the editor and jump the caret on every keystroke.
  const lastEmittedRef = useRef<string | null>(null);
  // YAML frontmatter is held aside rather than edited: it isn't
  // markdown, and the converter mangles it into a thematic break plus a
  // heading. Re-attached verbatim on every save.
  const frontmatterRef = useRef("");

  const editor = useEditor({
    extensions: markdownExtensions,
    content: "",
    onUpdate: ({ editor }) => {
      const md = joinFrontmatter(frontmatterRef.current, editorHtmlToMarkdown(editor.getHTML()));
      lastEmittedRef.current = md;
      onChange(md);
    },
    editorProps: {
      attributes: {
        // `tiptap-compact` + the inline <style> below mirror
        // InstructionsEditorModal's styling. The `prose` Tailwind
        // typography plugin isn't installed, and Tailwind 4 preflight
        // strips heading sizes + list markers — so without these rules
        // headings and bullets render as plain paragraphs.
        class: "tiptap-compact max-w-none focus:outline-none px-4 py-3",
        spellcheck: "false",
      },
    },
  });

  useEffect(() => {
    if (!editor) return;
    if (lastEmittedRef.current === source) return;
    const { frontmatter, body } = splitFrontmatter(source);
    frontmatterRef.current = frontmatter;
    const html = markdownToEditorHtml(body, baseDir);
    queueMicrotask(() => {
      if (editor.isDestroyed) return;
      editor.commands.setContent(html, {
        emitUpdate: false,
        parseOptions: { preserveWhitespace: false },
      });
      lastEmittedRef.current = source;
    });
  }, [source, baseDir, editor]);

  return (
    <div
      className="flex-1 min-h-0 overflow-auto rounded border"
      style={{
        background: "var(--bg-secondary)",
        borderColor: "var(--border)",
      }}
    >
      <style>{`
        .tiptap-compact { font-size: 13px; line-height: 1.55; }
        .tiptap-compact p { font-size: 13px; margin-top: 0.35em; margin-bottom: 0.35em; }
        .tiptap-compact h1 { font-size: 1.4rem; font-weight: 600; margin-top: 0.7em; margin-bottom: 0.35em; }
        .tiptap-compact h2 { font-size: 1.2rem; font-weight: 600; margin-top: 0.6em; margin-bottom: 0.3em; }
        .tiptap-compact h3 { font-size: 1.05rem; font-weight: 600; margin-top: 0.55em; margin-bottom: 0.25em; }
        .tiptap-compact h4, .tiptap-compact h5, .tiptap-compact h6 { font-size: 0.95rem; font-weight: 600; margin-top: 0.5em; margin-bottom: 0.2em; }
        .tiptap-compact ul {
          list-style: disc;
          list-style-position: outside;
          margin-top: 0.3em;
          margin-bottom: 0.3em;
          padding-left: 1.5em;
        }
        .tiptap-compact ol {
          list-style: decimal;
          list-style-position: outside;
          margin-top: 0.3em;
          margin-bottom: 0.3em;
          padding-left: 1.5em;
        }
        .tiptap-compact ul ul { list-style: circle; }
        .tiptap-compact ul ul ul { list-style: square; }
        .tiptap-compact li {
          font-size: 13px;
          margin-top: 0.15em;
          margin-bottom: 0.15em;
          display: list-item;
        }
        .tiptap-compact li > p { margin: 0; }
        .tiptap-compact code { font-size: 12px; padding: 0.1em 0.3em; border-radius: 3px; background: rgba(127,127,127,0.15); }
        .tiptap-compact pre { font-size: 12px; padding: 0.6em 0.8em; border-radius: 4px; background: rgba(127,127,127,0.12); overflow-x: auto; }
        .tiptap-compact pre code { background: transparent; padding: 0; }
        .tiptap-compact blockquote { font-size: 13px; margin: 0.4em 0; padding-left: 0.8em; border-left: 3px solid rgba(127,127,127,0.35); color: var(--text-secondary); }
        .tiptap-compact a { color: var(--accent, #61afef); text-decoration: underline; }
        .tiptap-compact strong { font-weight: 600; }
        .tiptap-compact em { font-style: italic; }
        .tiptap-compact hr { border: none; border-top: 1px solid var(--border); margin: 0.8em 0; }
        ${MARKDOWN_NODE_CSS}
      `}</style>
      <EditorContent editor={editor} className="h-full" />
    </div>
  );
}
