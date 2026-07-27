import { useState, useEffect, useCallback, useRef } from "react";
import {
  Folder,
  File,
  ArrowUp,
  Pencil,
  Eye,
  EyeOff,
  Save,
  X,
  FilePlus,
  FolderPlus,
  Download,
  Trash2,
  Library,
  Languages,
  FileText,
  Code2,
  Type,
} from "lucide-react";
import { send, subscribe } from "../hooks/useIPC";
import { assetUrl, workspacePrefix } from "../lib/assetUrl";
import { parentDir } from "../lib/markdownRoundTrip";
import { useTheme } from "../hooks/useTheme";
import { MarkdownEditor } from "./MarkdownEditor";
import { CodeEditor } from "./CodeEditor";
import { EpubViewer } from "./EpubViewer";

// Target languages for the Files-tab "Translate" action. `code` becomes the
// `<stem>-<code>.md` filename suffix; `label` is what the combobox shows.
const TRANSLATE_LANGUAGES: ReadonlyArray<{ code: string; label: string }> = [
  { code: "th", label: "Thai (ไทย)" },
  { code: "en", label: "English" },
  { code: "zh", label: "Chinese (中文)" },
  { code: "ja", label: "Japanese (日本語)" },
  { code: "ko", label: "Korean (한국어)" },
  { code: "es", label: "Spanish (Español)" },
  { code: "fr", label: "French (Français)" },
  { code: "de", label: "German (Deutsch)" },
  { code: "pt", label: "Portuguese (Português)" },
  { code: "vi", label: "Vietnamese (Tiếng Việt)" },
  { code: "id", label: "Indonesian (Bahasa)" },
  { code: "hi", label: "Hindi (हिन्दी)" },
  { code: "ar", label: "Arabic (العربية)" },
  { code: "ru", label: "Russian (Русский)" },
];

// Confirmation dialog with two backends.
//
// Desktop (`wry` WebView in `--gui`): the IPC bridge (`window.ipc`)
// is present, so round-trip through the Rust backend to get a real
// native modal on macOS / Linux / Windows. The backend shows the
// dialog on its IPC worker thread and replies with a
// `confirm_result` message keyed by `id`.
//
// `--serve` (web browser): no `window.ipc`, so the IPC round-trip
// would never resolve. Fall back to the browser's built-in
// `window.confirm()`.
function platformConfirm(opts: {
  title: string;
  message: string;
  yesLabel?: string;
  noLabel?: string;
}): Promise<boolean> {
  return new Promise((resolve) => {
    const inBrowser = typeof window !== "undefined" && !window.ipc;
    if (inBrowser) {
      resolve(window.confirm(`${opts.title}\n\n${opts.message}`));
      return;
    }
    const id =
      typeof crypto !== "undefined" && "randomUUID" in crypto
        ? crypto.randomUUID()
        : `cf-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
    const unsub = subscribe((msg) => {
      if (msg.type === "confirm_result" && msg.id === id) {
        unsub();
        resolve(Boolean(msg.ok));
      }
    });
    send({
      type: "confirm",
      id,
      title: opts.title,
      message: opts.message,
      yes_label: opts.yesLabel ?? "OK",
      no_label: opts.noLabel ?? "Cancel",
    });
  });
}

type FileEntry = {
  name: string;
  is_dir: boolean;
};

interface Props {
  active: boolean;
}

// UI view mode — what the user is looking at.
type ViewMode = "preview" | "edit";
// Backend read mode — what we asked the server for. "preview" returns
// pre-rendered HTML for `.md` (and raw text for everything else);
// "source" always returns raw text.
type ReadMode = "preview" | "source";

// Extensions we can open in the text editor. Binary types (image /
// pdf) stay preview-only.
const TEXT_EDITABLE = new Set([
  "md", "markdown", "html", "htm", "js", "jsx", "mjs", "cjs", "ts", "tsx",
  "css", "scss", "sass", "less", "py", "pyi", "rs", "go", "java", "kt",
  "swift", "c", "cpp", "cc", "cxx", "h", "hpp", "hh", "cs", "rb", "php",
  "sh", "bash", "zsh", "fish", "json", "jsonc", "yaml", "yml", "toml",
  "xml", "svg", "sql", "lua", "vim", "Dockerfile", "dockerfile", "ini",
  "conf", "env", "gitignore", "txt", "log",
]);

// Subset of TEXT_EDITABLE for which we actually want the preview pane
// to render through CodeMirror (syntax highlighting + line numbers)
// instead of a plain <pre>. Plain-text extensions stay in <pre> since
// CodeMirror wouldn't add anything useful there.
const SYNTAX_PREVIEW = new Set([
  "js", "jsx", "mjs", "cjs", "ts", "tsx",
  "html", "htm", "css", "scss", "sass", "less",
  "py", "pyi", "rs", "go", "java", "kt",
  "c", "cpp", "cc", "cxx", "h", "hpp", "hh",
  "php", "json", "jsonc", "yaml", "yml", "xml", "svg", "sql",
]);

function extOf(path: string): string {
  const base = path.split("/").pop() ?? "";
  if (!base.includes(".")) return base.toLowerCase();
  return (base.split(".").pop() ?? "").toLowerCase();
}

function isTextEditable(path: string): boolean {
  return TEXT_EDITABLE.has(extOf(path));
}

// Read a dropped File as base64 (without the `data:…;base64,` prefix) for
// the `file_upload` IPC, which decodes it back to raw bytes on the Rust
// side — binary-safe, unlike the text-only `file_write`.
function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const r = reader.result;
      if (typeof r !== "string")
        return reject(new Error("FileReader: non-string result"));
      const comma = r.indexOf(",");
      resolve(comma >= 0 ? r.slice(comma + 1) : r);
    };
    reader.onerror = () => reject(reader.error ?? new Error("FileReader failed"));
    reader.readAsDataURL(file);
  });
}

// A context-menu row with a clear hover highlight (accent fill) that works
// across themes — the old `hover:bg-white/10` was invisible on the light
// theme. `danger` paints the resting state red (e.g. Delete); hovering any
// row fills it with the accent and flips the text to the accent foreground.
function MenuItem({
  icon,
  label,
  danger,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  danger?: boolean;
  onClick: () => void;
}) {
  const [hover, setHover] = useState(false);
  return (
    <button
      type="button"
      className="flex items-center gap-2 w-full text-left px-3 py-1.5"
      style={{
        background: hover ? "var(--accent)" : undefined,
        color: hover ? "var(--accent-fg, #fff)" : danger ? "#ef4444" : undefined,
      }}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      onClick={onClick}
    >
      {icon} {label}
    </button>
  );
}

// Compact path for the explorer header — the viewer navbar already
// shows the full path. Root stays "."; nested dirs show "../<last>".
function shortPath(p: string): string {
  if (p === "." || p === "") return ".";
  const last = p.split("/").filter(Boolean).pop() ?? p;
  return p.includes("/") ? `../${last}` : last;
}

function isMarkdownPath(path: string): boolean {
  // Used to gate the iframe's `srcDoc` branch (vs. the asset-URL fetch
  // branch). Backend-rendered HTML previews — Markdown source files
  // *and* the Office formats whose extracted text we render through
  // the same comrak pipeline — both want srcDoc. Adding the office
  // extensions here is what makes Files-tab previews work for them.
  const e = extOf(path);
  return (
    e === "md" ||
    e === "markdown" ||
    e === "docx" ||
    e === "xlsx" ||
    e === "xlsm" ||
    e === "xlsb" ||
    e === "xls" ||
    e === "ods" ||
    e === "pptx"
  );
}

// In cloud, every workspace is mounted under `/u/<user>/<ws>/` by
// Traefik (see infra/k3s/runner/ingressroute.yaml.j2) and the
// strip-prefix middleware peels that off before forwarding to the
// runner pod. The pod's --serve also registers the file-asset route
// PREFIXED with the same path so the Host()+PathPrefix() rule
// Inject a <base href> pointing at the markdown file's parent directory
// via the file-asset handler so relative refs in srcDoc'd HTML (e.g.
// `<img src="img/foo.png">` from `![alt](img/foo.png)`) resolve to the
// .md file's sibling assets. Without this the srcDoc iframe has an
// opaque base URL and relative paths fail silently. The asset handler
// already enforces the sandbox check, so security is unchanged.
function injectBaseHref(html: string, filePath: string): string {
  const normalized = filePath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  const dir = lastSlash >= 0 ? normalized.slice(0, lastSlash) : "";
  const segments = dir.split("/").map(encodeURIComponent).join("/");
  const leadingSlash = segments.startsWith("/") ? "" : "/";
  const baseHref = `${window.location.origin}${workspacePrefix()}/file-asset${leadingSlash}${segments}/`;
  return html.replace(/<head>/i, `<head><base href="${baseHref}">`);
}

export function FilesView({ active }: Props) {
  const [currentPath, setCurrentPath] = useState(".");
  // Show dotfile entries (`.thclaws/`, `.claude/`, `.env`, etc.) in
  // the listing. Off by default — the agent workspace usually has
  // dozens of dot-prefixed paths the user doesn't need to see. The
  // toggle persists for the lifetime of the tab (no localStorage —
  // it's a transient view setting, not a preference).
  const [showHidden, setShowHidden] = useState(false);
  const [entries, setEntries] = useState<FileEntry[]>([]);
  // Explorer right-click context menu (null = closed).
  const [explorerMenu, setExplorerMenu] = useState<{ x: number; y: number } | null>(
    null,
  );
  // Per-entry right-click menu (Download for files, navigate for dirs).
  // Separate from `explorerMenu` (which is the empty-area "new file
  // / new folder" menu) so the two don't shadow each other.
  const [entryMenu, setEntryMenu] = useState<
    { x: number; y: number; path: string; name: string; isDir: boolean } | null
  >(null);
  // KMS list (name/scope) for the entry menu's "Add to KMS" submenu.
  // Kept in sync via `kms_update` broadcasts; requested once on mount.
  const [kmsList, setKmsList] = useState<{ name: string; scope: string }[]>([]);
  // Whether the entry menu's "Add to KMS" row is expanded into its KMS
  // picker (only used when more than one KMS exists). Reset when the menu
  // opens/closes so it never leaks between right-clicks.
  const [kmsPick, setKmsPick] = useState(false);
  // New file / folder name modal. null = closed; otherwise which kind.
  const [createKind, setCreateKind] = useState<"file" | "folder" | null>(null);
  const [createName, setCreateName] = useState("");
  const [createError, setCreateError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  // Rename modal. null = closed; otherwise the entry being renamed.
  const [renameTarget, setRenameTarget] = useState<{
    path: string;
    name: string;
    isDir: boolean;
  } | null>(null);
  const [renameName, setRenameName] = useState("");
  const [renameError, setRenameError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  // Language-action modal (markdown only): translate or summarize into a
  // chosen language. null = closed; otherwise the .md file + which action.
  // `langCode` is the selected target language code.
  const [langAction, setLangAction] = useState<{
    mode: "translate" | "summarize";
    path: string;
    name: string;
  } | null>(null);
  const [langCode, setLangCode] = useState("th");
  const { resolved: themeMode } = useTheme();

  // The file being displayed. `content` is what the backend returned —
  // for preview mode of a `.md` file, that's the rendered HTML; for
  // source mode it's the raw text. `mime` drives the preview renderer;
  // `mode` echoes the request so we know which we're looking at.
  const [preview, setPreview] = useState<{
    path: string;
    content: string;
    mime: string;
    readMode: ReadMode;
    /// Set when the backend rendered the file into something the
    /// browser can show — a `.pptx` converted to PDF by the
    /// slide-render service. The viewer mounts THIS path; `path`
    /// stays the file the user actually opened.
    renderPath?: string;
  } | null>(null);

  // Bumped on every Refresh click; used as part of iframe `key` props so
  // the iframe unmounts + re-fetches its asset (otherwise the browser
  // caches the iframe content even after the file on disk changes —
  // most visible with the productivity plugin's dashboard.html, which
  // an agent regenerates after every TASKS.md mutation).
  const [previewVersion, setPreviewVersion] = useState(0);
  // Path currently being rendered backend-side (a .pptx going through
  // the slide-render service). Cleared when its file_content lands.
  // Without this the tab looks frozen for the ~13s a cold render takes.
  const [renderPending, setRenderPending] = useState<string | null>(null);

  const [mode, setMode] = useState<ViewMode>("preview");
  // Source-text kept separate from preview.content because the preview
  // content may be rendered HTML while the editor always operates on
  // raw text.
  const [editorSource, setEditorSource] = useState<string>("");
  const [editorDirty, setEditorDirty] = useState(false);
  // Edit a `.md` as raw markdown instead of through the WYSIWYG. The
  // rich editor round-trips through HTML, which preserves content but
  // normalizes formatting — it reflows hard-wrapped prose, respaces list
  // markers, and rewrites raw HTML blocks as markdown. Files with a
  // machine-read format (agent definitions, `{{screenshot:…}}` book
  // chapters) want the bytes left alone. Sticks for the tab's lifetime.
  const [mdSourceEdit, setMdSourceEdit] = useState(false);
  const [saveToast, setSaveToast] = useState<string | null>(null);
  // True while files are being dragged over the tree panel (drop-to-upload).
  const [dragActive, setDragActive] = useState(false);
  const pendingNavigation = useRef<{ path: string } | null>(null);

  useEffect(() => {
    const unsub = subscribe((msg) => {
      if (msg.type === "file_tree") {
        setEntries(msg.entries as FileEntry[]);
        if (msg.path) setCurrentPath(msg.path as string);
      } else if (msg.type === "file_content") {
        const incomingPath = msg.path as string;
        const incomingContent = msg.content as string;
        // Dashboard host bridge: if a dashboard requested THIS
        // file via the load message, forward the content back to
        // it and DON'T touch the preview pane state — the user
        // is viewing dashboard.html, not TASKS.md.
        const pending = pendingDashboardLoad.current;
        if (pending && pending.targetPath === incomingPath) {
          pendingDashboardLoad.current = null;
          try {
            pending.source.postMessage(
              {
                type: "thclaws-dashboard-load-ack",
                reqId: pending.reqId,
                ok: true,
                content: incomingContent,
              },
              "*",
            );
          } catch {
            // iframe was torn down between request and response —
            // benign.
          }
          return;
        }
        const incomingReadMode: ReadMode =
          (msg.mode as ReadMode) ?? "preview";
        setPreview({
          path: incomingPath,
          content: incomingContent,
          mime: msg.mime as string,
          readMode: incomingReadMode,
          renderPath: (msg.render_path as string) || undefined,
        });
        setRenderPending(null);
        if (incomingReadMode === "source") {
          setEditorSource(incomingContent);
          setEditorDirty(false);
        }
      } else if (msg.type === "file_render_pending") {
        setRenderPending(msg.path as string);
      } else if (msg.type === "file_written") {
        const ok = msg.ok as boolean;
        const err = msg.error as string | null | undefined;
        if (ok) {
          setEditorDirty(false);
          setSaveToast("saved");
          // If the user had queued another file to open, do it now.
          if (pendingNavigation.current) {
            const p = pendingNavigation.current.path;
            pendingNavigation.current = null;
            openFile(p);
          }
        } else {
          setSaveToast(err ? `save failed: ${err}` : "save failed");
        }
        setTimeout(() => setSaveToast(null), 2500);
      }
    });
    send({ type: "file_list", path: ".", show_hidden: showHidden });
    return unsub;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Auto-refresh directory listing + preview while tab active.
  // Never auto-refresh when user is editing — we'd clobber their work.
  // `themeMode` is a dep so a light/dark swap re-fetches the .md
  // preview with the fresh palette baked into its iframe HTML.
  useEffect(() => {
    if (!active) return;
    // Don't re-read while a backend render is in flight. The tick is 2s
    // and a cold .pptx render takes ~13s, so without this each open
    // fires six more renders of the same deck — each one a fresh upload
    // and a fresh charge — before the first one answers.
    const refresh = () => {
      send({ type: "file_list", path: currentPath, show_hidden: showHidden });
      if (preview && mode === "preview" && !renderPending) {
        send({ type: "file_read", path: preview.path, mode: "preview", theme: themeMode });
      }
    };
    refresh();
    const interval = setInterval(refresh, 2000);
    return () => clearInterval(interval);
  // `preview?.path` is intentional — using the full `preview` object
  // would re-run on every polling cycle (setPreview creates a new
  // reference each time), resetting the interval unnecessarily.
  // `showHidden` triggers an immediate refresh when toggled so the
  // listing flips without waiting for the next 2s tick.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, currentPath, preview?.path, mode, themeMode, showHidden, renderPending]);

  // Drag-and-drop upload into the directory currently shown in the tree.
  // Each dropped file is base64-encoded and written via `file_upload`
  // (binary-safe, refuses to clobber). A per-upload self-unsubscribing
  // listener captures the *current* path/showHidden so the listing
  // refreshes correctly even though the mount-time subscriber can't.
  const uploadDroppedFiles = useCallback(
    (files: FileList) => {
      for (const file of Array.from(files)) {
        const reqId = Date.now() + Math.floor(Math.random() * 100000);
        const target =
          currentPath === "." ? file.name : `${currentPath}/${file.name}`;
        const unsub = subscribe((msg) => {
          if (msg.type !== "file_upload_result" || msg.id !== reqId) return;
          unsub();
          if (msg.ok) {
            setSaveToast(`uploaded ${file.name}`);
            send({ type: "file_list", path: currentPath, show_hidden: showHidden });
          } else {
            setSaveToast(
              `upload failed: ${file.name}${msg.error ? ` — ${msg.error}` : ""}`,
            );
          }
          setTimeout(() => setSaveToast(null), 3000);
        });
        fileToBase64(file)
          .then((data) =>
            send({ type: "file_upload", id: reqId, path: target, data }),
          )
          .catch(() => {
            unsub();
            setSaveToast(`upload failed: ${file.name}`);
            setTimeout(() => setSaveToast(null), 3000);
          });
      }
    },
    [currentPath, showHidden],
  );

  const onTreeDragOver = (e: React.DragEvent) => {
    // Only react to OS file drags, not internal element drags.
    if (!Array.from(e.dataTransfer.types).includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    if (!dragActive) setDragActive(true);
  };
  const onTreeDragLeave = (e: React.DragEvent) => {
    // Ignore leaves that just move onto a child element.
    if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
      setDragActive(false);
    }
  };
  const onTreeDrop = (e: React.DragEvent) => {
    const files = e.dataTransfer?.files;
    if (!files || files.length === 0) return;
    e.preventDefault();
    setDragActive(false);
    uploadDroppedFiles(files);
  };

  // Keep the KMS list fresh for the entry menu's "Add to KMS" action.
  // The backend broadcasts `kms_update` on any KMS change; request it once
  // on mount so the picker is populated before the first right-click.
  useEffect(() => {
    const unsub = subscribe((msg) => {
      if (msg.type !== "kms_update") return;
      const list = Array.isArray(msg.kmss) ? msg.kmss : [];
      setKmsList(
        list
          .map((k: { name?: unknown; scope?: unknown }) => ({
            name: String(k.name ?? ""),
            scope: String(k.scope ?? ""),
          }))
          .filter((k: { name: string }) => k.name),
      );
    });
    send({ type: "kms_list" });
    return unsub;
  }, []);

  // Collapse the "Add to KMS" picker whenever the entry menu opens or
  // closes, so a stale expansion never carries into the next right-click.
  useEffect(() => {
    setKmsPick(false);
  }, [entryMenu]);

  // Ingest a .md file into the named KMS (Files-tab "Add to KMS"). Mirrors
  // the delete/rename request-id + one-shot subscription pattern. On an
  // alias collision (`force` false) the backend flags `collision`; we then
  // confirm and re-ingest with `force: true` to replace the existing entry.
  const addToKms = useCallback(
    (path: string, name: string, kmsName: string, force = false) => {
      const reqId = Date.now() + Math.floor(Math.random() * 100000);
      const unsub = subscribe((msg) => {
        if (msg.type !== "kms_ingest_result" || msg.id !== reqId) return;
        unsub();
        if (msg.ok) {
          const imgs = Number(msg.images_copied ?? 0);
          const verb = msg.overwrote ? "updated in" : "added to";
          setSaveToast(
            `${verb} ${kmsName}: ${String(msg.alias ?? name)}${
              imgs > 0 ? ` (+${imgs} image${imgs === 1 ? "" : "s"})` : ""
            } — summarizing…`,
          );
          setTimeout(() => setSaveToast(null), 3500);
          // Auto-summary: hand the backend-composed prompt to the main
          // agent as a normal chat turn so it upgrades the stub page into
          // a curated summary (KmsRead/KmsSearch/KmsWrite). Visible in chat.
          const prompt = String(msg.summarize_prompt ?? "");
          if (prompt) send({ type: "shell_input", text: prompt });
          return;
        }
        if (msg.collision) {
          // Recoverable: an entry with this alias already exists. Ask
          // before overwriting it (and its images), then force-ingest.
          void platformConfirm({
            title: "Replace in KMS?",
            message: `"${name}" already exists in ${kmsName}. Replace the existing entry?`,
            yesLabel: "Replace",
            noLabel: "Cancel",
          }).then((confirmed) => {
            if (confirmed) addToKms(path, name, kmsName, true);
          });
          return;
        }
        setSaveToast(
          `add to KMS failed: ${name}${msg.error ? ` — ${msg.error}` : ""}`,
        );
        setTimeout(() => setSaveToast(null), 3500);
      });
      send({ type: "kms_ingest", id: reqId, path, kms: kmsName, force });
    },
    [],
  );

  // Translate OR summarize a .md file into the chosen language, saved beside
  // the original as `<stem>-<code>.md` (translate) or `<stem>-<code>-sum.md`
  // (summarize). Like "Add to KMS", we hand an explicit prompt to the main
  // agent as a chat turn (it has Read + Write) rather than block the UI on a
  // bespoke IPC. We route to the purpose-built `translator` / `summarizer`
  // built-in subagents via `/agent <name>` (side channel): they carry
  // Read+Write+Edit with permissionMode:auto, understand `--language=<code>`,
  // and — crucially — run INDEPENDENT of the main session, so a translation
  // doesn't bloat (or block behind) the main agent's conversation. We pass an
  // explicit output path so the written file matches the modal's preview
  // regardless of each subagent's own default suffix. Progress surfaces on the
  // Chat tab's side-channel indicator; the new file lands in the tree.
  const submitLangAction = useCallback(
    (e: React.FormEvent) => {
      e.preventDefault();
      if (!langAction) return;
      const { mode, path, name } = langAction;
      const lang =
        TRANSLATE_LANGUAGES.find((l) => l.code === langCode) ??
        TRANSLATE_LANGUAGES[0];
      const slash = path.lastIndexOf("/");
      const dir = slash >= 0 ? path.slice(0, slash) : "";
      const stem = name.replace(/\.(md|markdown)$/i, "");
      const suffix = mode === "summarize" ? `-${lang.code}-sum` : `-${lang.code}`;
      const outPath = `${dir ? `${dir}/` : ""}${stem}${suffix}.md`;
      const prompt =
        mode === "summarize"
          ? `/agent summarizer --language=${lang.code} Summarize the file ` +
            `\`${path}\` and write the ${lang.label} summary to \`${outPath}\`. ` +
            `Do not modify the original.`
          : `/agent translator --language=${lang.code} Translate the file ` +
            `\`${path}\` into ${lang.label} and write the result to \`${outPath}\`. ` +
            `Do not modify the original.`;
      send({ type: "shell_input", text: prompt });
      setSaveToast(
        `${mode === "summarize" ? "summarizing" : "translating"} ${name} → ${lang.code}…`,
      );
      setTimeout(() => setSaveToast(null), 3500);
      setLangAction(null);
    },
    [langAction, langCode],
  );

  // Delete a file or folder from the tree context menu. Confirms first
  // with the native dialog, then `file_delete` (sandbox-checked; recursive
  // for folders). On success the listing refreshes and the preview clears
  // if the deleted path was the one being viewed.
  const deleteEntry = useCallback(
    async (path: string, name: string, isDir: boolean) => {
      const confirmed = await platformConfirm({
        title: `Delete ${isDir ? "folder" : "file"}?`,
        message: isDir
          ? `"${name}" and everything inside it will be permanently deleted.`
          : `"${name}" will be permanently deleted.`,
        yesLabel: "Delete",
        noLabel: "Cancel",
      });
      if (!confirmed) return;
      const reqId = Date.now() + Math.floor(Math.random() * 100000);
      const unsub = subscribe((msg) => {
        if (msg.type !== "file_delete_result" || msg.id !== reqId) return;
        unsub();
        if (msg.ok) {
          setSaveToast(`deleted ${name}`);
          // Clear the preview if it pointed at the deleted path (or, for a
          // folder, anything inside it).
          if (
            preview &&
            (preview.path === path || preview.path.startsWith(`${path}/`))
          ) {
            setPreview(null);
          }
          send({ type: "file_list", path: currentPath, show_hidden: showHidden });
        } else {
          setSaveToast(
            `delete failed: ${name}${msg.error ? ` — ${msg.error}` : ""}`,
          );
        }
        setTimeout(() => setSaveToast(null), 3000);
      });
      send({ type: "file_delete", id: reqId, path });
    },
    [currentPath, showHidden, preview],
  );

  // One-shot file download — sends `file_download` with a unique
  // request id, waits for the matching `file_download_result`, then
  // converts the base64 payload into a Blob and triggers a browser
  // `<a download>` click. The subscriber unhooks itself once the
  // matching reply lands so we don't leak handlers across clicks.
  const downloadFile = useCallback((path: string) => {
    const reqId = Date.now() + Math.floor(Math.random() * 1000);
    const unsub = subscribe((msg) => {
      if (msg.type !== "file_download_result" || msg.id !== reqId) return;
      unsub();
      if (!msg.ok) {
        // Surface as a toast rather than a modal — same channel as
        // save errors.
        setSaveToast(`download failed: ${(msg.error as string) || "unknown"}`);
        setTimeout(() => setSaveToast(null), 4000);
        return;
      }
      const b64 = msg.content as string;
      const mime = (msg.mime as string) || "application/octet-stream";
      const filename = (msg.filename as string) || "download";
      try {
        const bin = atob(b64);
        const bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        const blob = new Blob([bytes], { type: mime });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        // Revoke after a short delay so Safari doesn't drop the
        // download mid-flight.
        setTimeout(() => URL.revokeObjectURL(url), 1500);
      } catch (e) {
        setSaveToast(`download decode failed: ${(e as Error).message}`);
        setTimeout(() => setSaveToast(null), 4000);
      }
    });
    send({ type: "file_download", id: reqId, path });
  }, []);

  const navigate = (name: string) => {
    const path = currentPath === "." ? name : `${currentPath}/${name}`;
    send({ type: "file_list", path, show_hidden: showHidden });
  };

  const goUp = () => {
    const parent = currentPath.includes("/")
      ? currentPath.substring(0, currentPath.lastIndexOf("/"))
      : ".";
    send({ type: "file_list", path: parent || ".", show_hidden: showHidden });
  };

  // Open the new file / folder modal, creating in the current directory.
  const startCreate = (kind: "file" | "folder") => {
    setExplorerMenu(null);
    setCreateName("");
    setCreateError(null);
    setCreateKind(kind);
  };

  const submitCreate = (e: React.FormEvent) => {
    e.preventDefault();
    if (creating || createKind === null) return;
    const n = createName.trim();
    if (!n) return setCreateError("name required");
    if (n.includes("/")) return setCreateError("name can't contain '/'");
    const path = currentPath === "." ? n : `${currentPath}/${n}`;
    setCreating(true);
    setCreateError(null);
    send({ type: createKind === "folder" ? "file_mkdir" : "file_create", path });
  };

  // Resolve the create round-trip; refresh the listing on success.
  useEffect(() => {
    if (createKind === null) return;
    const wanted =
      createKind === "folder" ? "file_mkdir_result" : "file_create_result";
    const unsub = subscribe((msg) => {
      if (msg.type !== wanted) return;
      setCreating(false);
      if (msg.ok) {
        setCreateKind(null);
        setCreateName("");
        send({ type: "file_list", path: currentPath, show_hidden: showHidden });
      } else {
        setCreateError((msg.error as string) ?? "create failed");
      }
    });
    return unsub;
  }, [createKind, currentPath]);

  // Open the rename modal for a tree entry, prefilled with its name.
  const startRename = (path: string, name: string, isDir: boolean) => {
    setEntryMenu(null);
    setRenameName(name);
    setRenameError(null);
    setRenameTarget({ path, name, isDir });
  };

  // Submit a rename: same parent directory, new basename. Defined as a
  // plain function (not memoized) so it always closes over the current
  // path / preview. Uses a per-request listener keyed by `id`.
  const submitRename = (e: React.FormEvent) => {
    e.preventDefault();
    if (renaming || !renameTarget) return;
    const n = renameName.trim();
    if (!n) return setRenameError("name required");
    if (n.includes("/")) return setRenameError("name can't contain '/'");
    if (n === renameTarget.name) return setRenameTarget(null);
    const target = renameTarget;
    const slash = target.path.lastIndexOf("/");
    const dir = slash >= 0 ? target.path.slice(0, slash) : "";
    const newPath = dir ? `${dir}/${n}` : n;
    const reqId = Date.now() + Math.floor(Math.random() * 100000);
    setRenaming(true);
    setRenameError(null);
    const unsub = subscribe((msg) => {
      if (msg.type !== "file_rename_result" || msg.id !== reqId) return;
      unsub();
      setRenaming(false);
      if (msg.ok) {
        // Clear the preview if it pointed at the renamed path (or inside a
        // renamed folder) — its old path no longer exists.
        if (
          preview &&
          (preview.path === target.path ||
            preview.path.startsWith(`${target.path}/`))
        ) {
          setPreview(null);
        }
        setRenameTarget(null);
        setRenameName("");
        setSaveToast(`renamed to ${n}`);
        setTimeout(() => setSaveToast(null), 2500);
        send({ type: "file_list", path: currentPath, show_hidden: showHidden });
      } else {
        setRenameError((msg.error as string) ?? "rename failed");
      }
    });
    send({ type: "file_rename", id: reqId, from: target.path, to: newPath });
  };

  const openFile = useCallback((path: string) => {
    setMode("preview");
    send({ type: "file_read", path, mode: "preview", theme: themeMode });
  }, [themeMode]);

  const onSidebarClick = async (name: string) => {
    const path = currentPath === "." ? name : `${currentPath}/${name}`;
    if (mode === "edit" && editorDirty) {
      const ok = await platformConfirm({
        title: "Unsaved changes",
        message: `You have unsaved edits to ${preview?.path ?? "this file"}. Discard them and open the new file?`,
        yesLabel: "Discard",
        noLabel: "Cancel",
      });
      if (!ok) return;
      setEditorDirty(false);
    }
    openFile(path);
  };

  const closePreview = async () => {
    setRenderPending(null);
    if (mode === "edit" && editorDirty) {
      const ok = await platformConfirm({
        title: "Discard unsaved changes",
        message: `Discard unsaved edits to ${preview?.path ?? "this file"} and close?`,
        yesLabel: "Discard",
        noLabel: "Keep editing",
      });
      if (!ok) return;
    }
    setPreview(null);
    setMode("preview");
    setEditorDirty(false);
  };

  const enterEditMode = () => {
    if (!preview) return;
    setMode("edit");
    send({ type: "file_read", path: preview.path, mode: "source" });
  };

  /// Refresh the current preview — re-fetches content from disk via
  /// the backend AND forces the preview iframe (when applicable) to
  /// re-mount so it re-fetches its asset URL. Needed because:
  ///   1. iframe content is browser-cached by URL; when an agent
  ///      regenerates a file on disk, the iframe still shows the old
  ///      content until it remounts.
  ///   2. The send() re-read alone updates preview.content (used for
  ///      .md and code-mirror previews), but iframe-rendered HTML
  ///      uses src={assetUrl(path)} not srcDoc={content}, so it
  ///      doesn't notice the state change without a key bump.
  const refreshPreview = () => {
    if (!preview) return;
    if (editorDirty) {
      const ok = window.confirm(
        "You have unsaved changes in the editor. Refresh anyway? Unsaved edits will be lost."
      );
      if (!ok) return;
      setEditorDirty(false);
    }
    setPreviewVersion((v) => v + 1);
    send({
      type: "file_read",
      path: preview.path,
      mode: mode === "preview" ? "preview" : "source",
      theme: themeMode,
    });
  };

  const exitEditMode = async () => {
    // If there are unsaved edits, surface a native OS confirm so the
    // user can abort a misclick. When the editor is already clean
    // ("Preview" button label), skip the prompt and go straight back.
    if (editorDirty) {
      const ok = await platformConfirm({
        title: "Discard unsaved changes",
        message: `Discard unsaved edits to ${preview?.path ?? "this file"} and return to preview?`,
        yesLabel: "Discard",
        noLabel: "Keep editing",
      });
      if (!ok) return;
    }
    setMode("preview");
    setEditorDirty(false);
    setEditorSource("");
    if (preview) {
      send({ type: "file_read", path: preview.path, mode: "preview", theme: themeMode });
    }
  };

  const save = useCallback(() => {
    if (!preview || !editorDirty) return;
    send({ type: "file_write", path: preview.path, content: editorSource });
  }, [preview, editorDirty, editorSource]);

  // ── thClaws → dashboard host bridge ─────────────────────────────
  //
  // Lets self-contained HTML dashboards (e.g. the productivity
  // plugin's dashboard.html, opened in an iframe via this Files
  // tab) save AND load sibling files via thClaws's IPC — without
  // ever prompting the user for a File System Access API permission
  // and without depending on agent-regenerated stale snapshots.
  //
  // Two message types from the iframe:
  //   - thclaws-dashboard-save  {filename, content}  →  file_write IPC
  //   - thclaws-dashboard-load  {filename}           →  file_read IPC
  // Each pairs with a *-ack response back to the same iframe.
  //
  // Sender origin isn't checked because the iframe runs sandboxed
  // from a `thclaws://` asset URL — the attack surface is bounded
  // to our own dashboard content.
  //
  // The load path correlates async file_read responses to requesting
  // iframes via a single-slot pendingDashboardLoad ref. Concurrent
  // requests overwrite (rare in practice — one dashboard, one read).
  const pendingDashboardLoad = useRef<{
    source: Window;
    reqId: string;
    targetPath: string;
  } | null>(null);

  useEffect(() => {
    const handler = (e: MessageEvent) => {
      const d = e.data as
        | {
            type?: string;
            reqId?: string;
            filename?: string;
            content?: string;
          }
        | undefined;
      if (!d || !preview) return;

      // Resolve `filename` (e.g. "TASKS.md") against the directory of
      // the currently-previewed file. So opening
      // `/proj/business-cards/dashboard.html` and asking for
      // "TASKS.md" hits `/proj/business-cards/TASKS.md`.
      const slash = preview.path.lastIndexOf("/");
      const dir = slash > 0 ? preview.path.slice(0, slash) : ".";
      const targetPath = `${dir}/${d.filename || "TASKS.md"}`;

      if (d.type === "thclaws-dashboard-save") {
        try {
          send({
            type: "file_write",
            path: targetPath,
            content: d.content || "",
          });
          if (e.source && "postMessage" in e.source) {
            (e.source as Window).postMessage(
              {
                type: "thclaws-dashboard-save-ack",
                reqId: d.reqId,
                ok: true,
              },
              "*",
            );
          }
        } catch (err) {
          if (e.source && "postMessage" in e.source) {
            (e.source as Window).postMessage(
              {
                type: "thclaws-dashboard-save-ack",
                reqId: d.reqId,
                ok: false,
                error: String(err),
              },
              "*",
            );
          }
        }
      } else if (d.type === "thclaws-dashboard-load") {
        if (!e.source || !("postMessage" in e.source) || !d.reqId) return;
        // Stash the requesting iframe + reqId so the file_content
        // subscriber below can route the response back. Single-slot
        // — concurrent requests overwrite (rare in practice).
        pendingDashboardLoad.current = {
          source: e.source as Window,
          reqId: d.reqId,
          targetPath,
        };
        send({ type: "file_read", path: targetPath, mode: "source" });
      }
    };
    window.addEventListener("message", handler);
    return () => window.removeEventListener("message", handler);
  }, [preview]);

  // Global Cmd/Ctrl-S when Files tab is active + in edit mode.
  useEffect(() => {
    if (!active || mode !== "edit") return;
    const handler = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key.toLowerCase() === "s") {
        e.preventDefault();
        save();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [active, mode, save]);

  // `beforeunload` in wry WebViews is a best-effort warning; if the
  // native host ignores it, at least we're not losing data silently
  // because the Discard button and "save or discard first" toast
  // already guard the in-app flow.
  useEffect(() => {
    if (!editorDirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [editorDirty]);

  const isHtml = preview?.mime === "text/html";
  const isImage = preview?.mime.startsWith("image/");
  const isPdf = preview?.mime === "application/pdf";
  const isEpub = preview?.mime === "application/epub+zip";
  const isAudio = !!preview?.mime.startsWith("audio/");
  const isVideo = !!preview?.mime.startsWith("video/");
  const canEdit = preview && isTextEditable(preview.path);
  const hasSyntaxPreview =
    preview && SYNTAX_PREVIEW.has(extOf(preview.path));


  return (
    <div
      className="flex flex-col sm:flex-row h-full"
      style={{ background: "var(--bg-primary)" }}
    >
      {/* Tree panel — full-width strip on top below `sm` (capped height so
          the editor stays visible), fixed-width left column at `sm:`+. */}
      <div
        className="w-full sm:w-64 max-sm:max-h-[38%] overflow-y-auto border-b sm:border-b-0 sm:border-r shrink-0 flex flex-col"
        style={{
          borderColor: dragActive ? "var(--accent)" : "var(--border)",
          ...(dragActive
            ? {
                outline: "2px dashed var(--accent)",
                outlineOffset: "-2px",
                background: "rgba(13,148,136,0.08)",
              }
            : {}),
        }}
        onDragOver={onTreeDragOver}
        onDragLeave={onTreeDragLeave}
        onDrop={onTreeDrop}
      >
        <div
          className="flex items-center gap-1 px-2 py-1.5 border-b text-[10px] font-mono shrink-0"
          style={{
            background: "var(--bg-secondary)",
            borderColor: "var(--border)",
            color: "var(--text-secondary)",
          }}
        >
          <button
            onClick={goUp}
            className="p-0.5 rounded hover:bg-white/10"
            title="Go up"
          >
            <ArrowUp size={12} />
          </button>
          <span className="truncate flex-1" title={currentPath}>
            {shortPath(currentPath)}
          </span>
          <button
            onClick={() => setShowHidden((v) => !v)}
            className="p-0.5 rounded hover:bg-white/10 shrink-0"
            title={
              showHidden
                ? "Hide dotfiles (.thclaws, .claude, .env, …)"
                : "Show dotfiles (.thclaws, .claude, .env, …)"
            }
            style={{ color: showHidden ? "var(--accent)" : "var(--text-secondary)" }}
          >
            {showHidden ? <Eye size={12} /> : <EyeOff size={12} />}
          </button>
          <button
            onClick={() => startCreate("file")}
            className="p-0.5 rounded hover:bg-white/10 shrink-0"
            title={`New file in ${currentPath}`}
          >
            <FilePlus size={12} />
          </button>
          <button
            onClick={() => startCreate("folder")}
            className="p-0.5 rounded hover:bg-white/10 shrink-0"
            title={`New folder in ${currentPath}`}
          >
            <FolderPlus size={12} />
          </button>
        </div>

        <div
          className="overflow-y-auto flex-1 p-1"
          onContextMenu={(e) => {
            e.preventDefault();
            setExplorerMenu({ x: e.clientX, y: e.clientY });
          }}
        >
          {dragActive && (
            <div
              className="text-[10px] font-mono px-2 py-1 mb-1 rounded text-center"
              style={{ background: "var(--accent)", color: "#fff" }}
            >
              Drop to upload to {shortPath(currentPath)}
            </div>
          )}
          {entries.length === 0 ? (
            <div className="text-xs p-2" style={{ color: "var(--text-secondary)" }}>
              Empty directory
            </div>
          ) : (
            entries.map((entry) => {
              // Same path composition as openFile() so the comparison
              // matches whatever the preview pane currently points at.
              // Directories never get the selection mark — they
              // navigate instead of preview, so highlighting one
              // would lie about which file is open.
              const entryPath =
                currentPath === "." ? entry.name : `${currentPath}/${entry.name}`;
              const isSelected = !entry.is_dir && preview?.path === entryPath;
              return (
                <button
                  key={entry.name}
                  aria-current={isSelected ? "true" : undefined}
                  className="flex items-center gap-1.5 w-full px-2 py-1 rounded text-xs text-left"
                  style={{
                    // Text + icon colour stay theme-default for both
                    // states — readability shouldn't depend on theme
                    // contrast against an accent fill.
                    color: "var(--text-primary)",
                    // Selection mark is just a faint background fill
                    // — same shape hover uses, slightly darker. Drops
                    // the left bar / accent text / bold from the
                    // previous pass; that combo read as a "click me"
                    // CTA rather than a passive indicator.
                    background: isSelected
                      ? "var(--bg-tertiary, rgba(255,255,255,0.06))"
                      : undefined,
                    paddingLeft: 8,
                  }}
                  onMouseEnter={(e) => {
                    if (!isSelected) e.currentTarget.style.background = "rgba(255,255,255,0.05)";
                  }}
                  onMouseLeave={(e) => {
                    if (!isSelected) e.currentTarget.style.background = "";
                  }}
                  onClick={() =>
                    entry.is_dir ? navigate(entry.name) : onSidebarClick(entry.name)
                  }
                  onContextMenu={(e) => {
                    // Per-entry menu — close the explorer-level
                    // "new file/folder" one if it was somehow open.
                    e.preventDefault();
                    e.stopPropagation();
                    setExplorerMenu(null);
                    setEntryMenu({
                      x: e.clientX,
                      y: e.clientY,
                      path: entryPath,
                      name: entry.name,
                      isDir: entry.is_dir,
                    });
                  }}
                >
                  {entry.is_dir ? (
                    <Folder size={13} style={{ color: "var(--accent)", flexShrink: 0 }} />
                  ) : (
                    <File size={13} style={{ color: "var(--text-secondary)", flexShrink: 0 }} />
                  )}
                  <span className="truncate">{entry.name}</span>
                </button>
              );
            })
          )}
        </div>
      </div>

      {/* Preview / editor panel */}
      <div className="flex-1 min-w-0 min-h-0 flex flex-col p-4">
        {renderPending ? (
          // A .pptx is being converted by the slide-render service. This
          // sits ABOVE the `preview` check on purpose: on a first open
          // there is no preview for this file yet, so a placeholder
          // gated on `preview.path` would never appear and the pane
          // would just sit there for ~13s looking hung.
          <div className="flex flex-col flex-1 min-w-0 min-h-0">
            <div
              className="text-xs font-mono truncate mb-3 shrink-0"
              style={{ color: "var(--text-secondary)" }}
            >
              {renderPending}
            </div>
            <div
              className="flex-1 min-h-0 flex flex-col items-center justify-center gap-2 text-xs"
              style={{ color: "var(--text-secondary)" }}
            >
              <div className="animate-pulse">Rendering slides…</div>
              <div className="opacity-70">
                Converting this deck on thclaws.cloud · cached after the first time
              </div>
            </div>
          </div>
        ) : preview ? (
          <div className="flex flex-col flex-1 min-w-0 min-h-0">
            <div className="flex items-center justify-between mb-3 shrink-0 gap-2">
              <div
                className="text-xs font-mono truncate min-w-0 flex-1"
                style={{ color: "var(--text-secondary)" }}
              >
                {preview.path}
                {editorDirty && (
                  <span style={{ color: "var(--accent)" }} title="unsaved changes">
                    {" "}●
                  </span>
                )}
              </div>
              <div className="flex items-center gap-1.5 shrink-0">
                {saveToast && (
                  <span
                    className="text-[10px] font-mono px-2 py-0.5 rounded"
                    style={{
                      background: saveToast.startsWith("save failed")
                        ? "rgba(220,80,80,0.15)"
                        : "rgba(100,180,100,0.15)",
                      color: saveToast.startsWith("save failed")
                        ? "#e06060"
                        : "#6fbf6f",
                    }}
                  >
                    {saveToast}
                  </span>
                )}
                <button
                  onClick={refreshPreview}
                  className="flex items-center gap-1 text-[11px] px-2 py-1 rounded hover:bg-white/5"
                  style={{ color: "var(--text-primary)" }}
                  title="Re-read this file from disk and re-render the preview"
                >
                  Refresh
                </button>
                {canEdit && mode === "preview" && (
                  <button
                    onClick={enterEditMode}
                    className="flex items-center gap-1 text-[11px] px-2 py-1 rounded hover:bg-white/5"
                    style={{ color: "var(--text-primary)" }}
                    title="Edit this file"
                  >
                    <Pencil size={12} />
                    Edit
                  </button>
                )}
                {mode === "edit" && isMarkdownPath(preview.path) && (
                  <button
                    onClick={() => setMdSourceEdit((v) => !v)}
                    className="flex items-center gap-1 text-[11px] px-2 py-1 rounded hover:bg-white/5"
                    style={{ color: "var(--text-secondary)" }}
                    title={
                      mdSourceEdit
                        ? "Switch to the rich-text editor (reformats markdown on save)"
                        : "Edit the raw markdown — leaves formatting exactly as written"
                    }
                  >
                    {mdSourceEdit ? <Type size={12} /> : <Code2 size={12} />}
                    {mdSourceEdit ? "Rich text" : "Source"}
                  </button>
                )}
                {mode === "edit" && (
                  <>
                    <button
                      onClick={save}
                      disabled={!editorDirty}
                      className="flex items-center gap-1 text-[11px] px-2 py-1 rounded hover:bg-white/5 disabled:opacity-40 disabled:cursor-not-allowed"
                      style={{ color: "var(--accent)" }}
                      title="Save (Cmd/Ctrl-S)"
                    >
                      <Save size={12} />
                      Save
                    </button>
                    <button
                      onClick={exitEditMode}
                      className="flex items-center gap-1 text-[11px] px-2 py-1 rounded hover:bg-white/5"
                      style={{ color: "var(--text-secondary)" }}
                      title="Back to preview"
                    >
                      {editorDirty ? <X size={12} /> : <Eye size={12} />}
                      {editorDirty ? "Discard" : "Preview"}
                    </button>
                  </>
                )}
                <button
                  onClick={closePreview}
                  className="flex items-center justify-center p-1 rounded hover:bg-white/5"
                  style={{ color: "var(--text-secondary)" }}
                  title="Close file"
                >
                  <X size={13} />
                </button>
              </div>
            </div>

            {/* Body: preview or editor */}
            {mode === "edit" ? (
              isMarkdownPath(preview.path) && !mdSourceEdit ? (
                <MarkdownEditor
                  source={editorSource}
                  baseDir={parentDir(preview.path)}
                  onChange={(md) => {
                    setEditorSource(md);
                    setEditorDirty(true);
                  }}
                />
              ) : (
                <CodeEditor
                  source={editorSource}
                  path={preview.path}
                  onChange={(text) => {
                    setEditorSource(text);
                    setEditorDirty(true);
                  }}
                  onSave={save}
                />
              )
            ) : isImage ? (
              <div className="flex-1 min-h-0 overflow-auto">
                <img
                  src={`data:${preview.mime};base64,${preview.content}`}
                  alt={preview.path}
                  className="max-w-full rounded"
                />
              </div>
            ) : isPdf ? (
              // Stream off /file-asset/ like audio/video — Chrome
              // refuses to run its PDF viewer inside a data: iframe
              // (opaque origin ⇒ implicitly sandboxed; the viewer's
              // internal about:srcdoc frame logs "Blocked script
              // execution… 'allow-scripts'"), and a book PDF would
              // round-trip 5MB+ of base64 through the WS anyway.
              <iframe
                key={`pdf-${preview.renderPath ?? preview.path}-${previewVersion}`}
                src={assetUrl(preview.renderPath ?? preview.path)}
                className="w-full flex-1 min-h-0 rounded border"
                style={{ borderColor: "var(--border)", background: "#fff" }}
                title={preview.path}
              />
            ) : isEpub ? (
              // EPUB renders via epub.js (see EpubViewer): it fetches
              // the bytes off /file-asset and unzips client-side, since
              // no browser renders EPUB natively the way it does PDF.
              <EpubViewer
                key={`epub-${preview.path}-${previewVersion}`}
                url={assetUrl(preview.path)}
                theme={themeMode}
              />
            ) : isAudio ? (
              // Audio + video stream off /file-asset/, not a base64
              // data URI — keeps a 50 MB clip from round-tripping
              // through IPC. `key` forces a fresh element when the
              // selected file changes so the player resets cleanly.
              <div className="flex-1 min-h-0 flex flex-col items-center justify-center gap-4 p-6">
                <audio
                  key={`audio-${preview.path}`}
                  src={assetUrl(preview.path)}
                  controls
                  preload="metadata"
                  className="w-full max-w-2xl"
                />
                <div
                  className="text-xs font-mono"
                  style={{ color: "var(--text-muted)" }}
                >
                  {preview.path.split("/").pop()} · {preview.mime}
                </div>
              </div>
            ) : isVideo ? (
              <div className="flex-1 min-h-0 overflow-auto flex items-center justify-center p-2">
                <video
                  key={`video-${preview.path}`}
                  src={assetUrl(preview.path)}
                  controls
                  preload="metadata"
                  className="max-w-full max-h-full rounded"
                  style={{ background: "#000" }}
                />
              </div>
            ) : isHtml ? (
              isMarkdownPath(preview.path) ? (
                // Markdown preview: backend renders MD → HTML and
                // returns it in `content`. Use `srcDoc` so the iframe
                // shows that HTML directly; `src={assetUrl}` would
                // fetch the raw .md via the custom protocol and the
                // iframe would end up blank. `injectBaseHref` rewrites
                // the document so relative `![alt](img/foo.png)` refs
                // resolve via /file-asset/ instead of failing against
                // srcDoc's opaque base URL.
                // sandbox: `allow-same-origin` WITHOUT `allow-scripts`.
                // The rendered markdown is static HTML — no JS needed —
                // but its <img> subresources must carry the session
                // cookie or the cloud ForwardAuth gate 302s them to
                // login (sandboxed opaque origins send no credentials,
                // so chapter figures showed as broken images). Without
                // allow-scripts the same-origin grant is inert: nothing
                // executes inside the frame.
                <iframe
                  key={`md-${preview.path}-${previewVersion}`}
                  srcDoc={injectBaseHref(preview.content, preview.path)}
                  className="w-full flex-1 min-h-0 rounded border"
                  style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}
                  sandbox="allow-same-origin"
                  title={preview.path}
                />
              ) : (
                <iframe
                  key={`html-${preview.path}-${previewVersion}`}
                  src={assetUrl(preview.path)}
                  className="w-full flex-1 min-h-0 rounded border"
                  style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}
                  sandbox="allow-scripts"
                  title={preview.path}
                />
              )
            ) : hasSyntaxPreview ? (
              <CodeEditor
                source={preview.content}
                path={preview.path}
                readOnly
              />
            ) : (
              <pre
                className="text-xs font-mono whitespace-pre-wrap rounded p-3 flex-1 min-h-0 overflow-auto"
                style={{
                  background: "var(--bg-secondary)",
                  color: "var(--text-primary)",
                  tabSize: 4,
                }}
              >
                {preview.content}
              </pre>
            )}
          </div>
        ) : (
          <div
            className="text-sm mt-20 text-center"
            style={{ color: "var(--text-secondary)" }}
          >
            Click a file to preview
          </div>
        )}
      </div>

      {entryMenu && (
        <>
          <div
            className="fixed inset-0 z-[55]"
            onClick={() => setEntryMenu(null)}
            onContextMenu={(e) => {
              e.preventDefault();
              setEntryMenu(null);
            }}
          />
          <div
            className="fixed z-[56] rounded border shadow-lg text-xs py-1"
            style={{
              left: entryMenu.x,
              top: entryMenu.y,
              minWidth: "160px",
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
              color: "var(--text-primary)",
            }}
          >
            {/* Files-only: Download. Directories navigate; bulk
                folder download would zip on the backend — not worth
                the surface area until users ask for it. */}
            {!entryMenu.isDir && (
              <MenuItem
                icon={<Download size={13} />}
                label="Download"
                onClick={() => {
                  downloadFile(entryMenu.path);
                  setEntryMenu(null);
                }}
              />
            )}
            {/* Markdown-only: ingest into a KMS. One KMS → ingest
                straight away; several → expand an inline picker; none →
                a hint toast. */}
            {!entryMenu.isDir && /\.(md|markdown)$/i.test(entryMenu.name) && (
              <>
                <MenuItem
                  icon={<Library size={13} />}
                  label={
                    kmsList.length > 1
                      ? `Add to KMS${kmsPick ? " ▾" : " ▸"}`
                      : "Add to KMS"
                  }
                  onClick={() => {
                    const m = entryMenu;
                    if (kmsList.length === 0) {
                      setEntryMenu(null);
                      setSaveToast("no KMS yet — create one first");
                      setTimeout(() => setSaveToast(null), 3000);
                    } else if (kmsList.length === 1) {
                      setEntryMenu(null);
                      addToKms(m.path, m.name, kmsList[0].name);
                    } else {
                      setKmsPick((v) => !v);
                    }
                  }}
                />
                {kmsPick &&
                  kmsList.length > 1 &&
                  kmsList.map((k) => (
                    <MenuItem
                      key={`${k.scope}:${k.name}`}
                      icon={<Library size={12} />}
                      label={`↳ ${k.name}`}
                      onClick={() => {
                        const m = entryMenu;
                        setEntryMenu(null);
                        addToKms(m.path, m.name, k.name);
                      }}
                    />
                  ))}
              </>
            )}
            {/* Markdown-only: translate or summarize into a chosen language,
                saved as a `<stem>-<code>.md` / `<stem>-<code>-sum.md` sibling. */}
            {!entryMenu.isDir && /\.(md|markdown)$/i.test(entryMenu.name) && (
              <>
                <MenuItem
                  icon={<Languages size={13} />}
                  label="Translate…"
                  onClick={() => {
                    const m = entryMenu;
                    setEntryMenu(null);
                    setLangCode("th");
                    setLangAction({ mode: "translate", path: m.path, name: m.name });
                  }}
                />
                <MenuItem
                  icon={<FileText size={13} />}
                  label="Summarize…"
                  onClick={() => {
                    const m = entryMenu;
                    setEntryMenu(null);
                    setLangCode("th");
                    setLangAction({ mode: "summarize", path: m.path, name: m.name });
                  }}
                />
              </>
            )}
            <MenuItem
              icon={<Pencil size={13} />}
              label="Rename"
              onClick={() => {
                const m = entryMenu;
                startRename(m.path, m.name, m.isDir);
              }}
            />
            <MenuItem
              icon={<Trash2 size={13} />}
              label="Delete"
              danger
              onClick={() => {
                const m = entryMenu;
                setEntryMenu(null);
                deleteEntry(m.path, m.name, m.isDir);
              }}
            />
          </div>
        </>
      )}

      {explorerMenu && (
        <>
          <div
            className="fixed inset-0 z-[55]"
            onClick={() => setExplorerMenu(null)}
            onContextMenu={(e) => {
              e.preventDefault();
              setExplorerMenu(null);
            }}
          />
          <div
            className="fixed z-[56] rounded border shadow-lg text-xs py-1"
            style={{
              left: explorerMenu.x,
              top: explorerMenu.y,
              minWidth: "160px",
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
              color: "var(--text-primary)",
            }}
          >
            <MenuItem
              icon={<FilePlus size={13} />}
              label="New file…"
              onClick={() => startCreate("file")}
            />
            <MenuItem
              icon={<FolderPlus size={13} />}
              label="New folder…"
              onClick={() => startCreate("folder")}
            />
          </div>
        </>
      )}

      {createKind && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center"
          style={{ background: "var(--modal-backdrop, rgba(0,0,0,0.55))" }}
          onClick={() => setCreateKind(null)}
          onKeyDown={(e) => {
            if (e.key === "Escape") setCreateKind(null);
          }}
        >
          <form
            className="rounded-lg border shadow-xl w-[420px] max-w-[92vw]"
            style={{
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
              color: "var(--text-primary)",
            }}
            onClick={(e) => e.stopPropagation()}
            onSubmit={submitCreate}
          >
            <div
              className="px-4 py-2 border-b text-sm font-semibold flex items-center gap-2"
              style={{ borderColor: "var(--border)" }}
            >
              <span style={{ color: "var(--accent)" }}>●</span>
              <span>
                New {createKind} in {currentPath}
              </span>
            </div>
            <div className="px-4 py-3 space-y-2 text-xs">
              <input
                autoFocus
                type="text"
                value={createName}
                onChange={(e) => setCreateName(e.target.value)}
                placeholder={createKind === "folder" ? "src" : "notes.md"}
                className="w-full px-2 py-1.5 rounded border font-mono text-xs"
                style={{
                  background: "var(--bg-secondary)",
                  borderColor: "var(--border)",
                  color: "var(--text-primary)",
                }}
              />
              {createError && (
                <div style={{ color: "var(--danger, #e06c75)" }}>{createError}</div>
              )}
            </div>
            <div
              className="px-4 py-2.5 border-t flex justify-end gap-2"
              style={{ borderColor: "var(--border)" }}
            >
              <button
                type="button"
                onClick={() => setCreateKind(null)}
                className="px-3 py-1.5 rounded border text-xs"
                style={{
                  background: "var(--bg-secondary)",
                  borderColor: "var(--border)",
                  color: "var(--text-secondary)",
                }}
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={creating}
                className="px-3 py-1.5 rounded text-xs font-medium"
                style={{
                  background: "var(--accent)",
                  color: "var(--accent-fg, #fff)",
                  opacity: creating ? 0.6 : 1,
                  cursor: creating ? "default" : "pointer",
                }}
              >
                {creating ? "Creating…" : "Create"}
              </button>
            </div>
          </form>
        </div>
      )}

      {renameTarget && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center"
          style={{ background: "var(--modal-backdrop, rgba(0,0,0,0.55))" }}
          onClick={() => setRenameTarget(null)}
          onKeyDown={(e) => {
            if (e.key === "Escape") setRenameTarget(null);
          }}
        >
          <form
            className="rounded-lg border shadow-xl w-[420px] max-w-[92vw]"
            style={{
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
              color: "var(--text-primary)",
            }}
            onClick={(e) => e.stopPropagation()}
            onSubmit={submitRename}
          >
            <div
              className="px-4 py-2 border-b text-sm font-semibold flex items-center gap-2"
              style={{ borderColor: "var(--border)" }}
            >
              <Pencil size={14} style={{ color: "var(--accent)" }} />
              <span>Rename {renameTarget.isDir ? "folder" : "file"}</span>
            </div>
            <div className="px-4 py-3 space-y-2 text-xs">
              <input
                autoFocus
                type="text"
                value={renameName}
                onChange={(e) => setRenameName(e.target.value)}
                onFocus={(e) => e.target.select()}
                placeholder={renameTarget.name}
                className="w-full px-2 py-1.5 rounded border font-mono text-xs"
                style={{
                  background: "var(--bg-secondary)",
                  borderColor: "var(--border)",
                  color: "var(--text-primary)",
                }}
              />
              {renameError && (
                <div style={{ color: "var(--danger, #e06c75)" }}>{renameError}</div>
              )}
            </div>
            <div
              className="px-4 py-2.5 border-t flex justify-end gap-2"
              style={{ borderColor: "var(--border)" }}
            >
              <button
                type="button"
                onClick={() => setRenameTarget(null)}
                className="px-3 py-1.5 rounded border text-xs"
                style={{
                  background: "var(--bg-secondary)",
                  borderColor: "var(--border)",
                  color: "var(--text-secondary)",
                }}
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={renaming}
                className="px-3 py-1.5 rounded text-xs font-medium"
                style={{
                  background: "var(--accent)",
                  color: "var(--accent-fg, #fff)",
                  opacity: renaming ? 0.6 : 1,
                  cursor: renaming ? "default" : "pointer",
                }}
              >
                {renaming ? "Renaming…" : "Rename"}
              </button>
            </div>
          </form>
        </div>
      )}

      {langAction && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center"
          style={{ background: "var(--modal-backdrop, rgba(0,0,0,0.55))" }}
          onClick={() => setLangAction(null)}
          onKeyDown={(e) => {
            if (e.key === "Escape") setLangAction(null);
          }}
        >
          <form
            className="rounded-lg border shadow-xl w-[420px] max-w-[92vw]"
            style={{
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
              color: "var(--text-primary)",
            }}
            onClick={(e) => e.stopPropagation()}
            onSubmit={submitLangAction}
          >
            <div
              className="px-4 py-2 border-b text-sm font-semibold flex items-center gap-2"
              style={{ borderColor: "var(--border)" }}
            >
              {langAction.mode === "summarize" ? (
                <FileText size={14} style={{ color: "var(--accent)" }} />
              ) : (
                <Languages size={14} style={{ color: "var(--accent)" }} />
              )}
              <span>
                {langAction.mode === "summarize" ? "Summarize" : "Translate"} file
              </span>
            </div>
            <div className="px-4 py-3 space-y-3 text-xs">
              <div style={{ color: "var(--text-secondary)" }}>
                {langAction.mode === "summarize" ? "Summarize" : "Translate"}{" "}
                <span className="font-mono" style={{ color: "var(--text-primary)" }}>
                  {langAction.name}
                </span>{" "}
                {langAction.mode === "summarize" ? "in:" : "into:"}
              </div>
              <select
                autoFocus
                value={langCode}
                onChange={(e) => setLangCode(e.target.value)}
                className="w-full px-2 py-1.5 rounded border text-xs"
                style={{
                  background: "var(--bg-secondary)",
                  borderColor: "var(--border)",
                  color: "var(--text-primary)",
                }}
              >
                {TRANSLATE_LANGUAGES.map((l) => (
                  <option key={l.code} value={l.code}>
                    {l.label}
                  </option>
                ))}
              </select>
              <div style={{ color: "var(--text-secondary)" }}>
                Saves to{" "}
                <span className="font-mono" style={{ color: "var(--text-primary)" }}>
                  {langAction.name.replace(/\.(md|markdown)$/i, "")}-{langCode}
                  {langAction.mode === "summarize" ? "-sum" : ""}.md
                </span>
              </div>
            </div>
            <div
              className="px-4 py-2.5 border-t flex justify-end gap-2"
              style={{ borderColor: "var(--border)" }}
            >
              <button
                type="button"
                onClick={() => setLangAction(null)}
                className="px-3 py-1.5 rounded border text-xs"
                style={{
                  background: "var(--bg-secondary)",
                  borderColor: "var(--border)",
                  color: "var(--text-secondary)",
                }}
              >
                Cancel
              </button>
              <button
                type="submit"
                className="px-3 py-1.5 rounded text-xs font-medium"
                style={{
                  background: "var(--accent)",
                  color: "var(--accent-fg, #fff)",
                }}
              >
                {langAction.mode === "summarize" ? "Summarize" : "Translate"}
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
