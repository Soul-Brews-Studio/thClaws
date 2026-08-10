# MCP — Model Context Protocol

JSON-RPC 2.0 client subsystem that lets thClaws talk to external tool-providing servers (filesystem, git, weather, search, custom skills) over stdio or HTTP. Discovered tools are wrapped with an [`McpTool`] adapter and registered into the same [`ToolRegistry`] the agent loop uses, so MCP-contributed tools are indistinguishable from built-ins from the model's perspective. The HTTP variant additionally implements OAuth 2.1 + PKCE for protected servers, with per-token authorization-server binding to defend against discovery-swap attacks.

A second axis — **MCP-Apps** — lets a trusted MCP server ship an interactive HTML widget alongside its tool result. The widget renders inside a sandboxed iframe in the chat surface, communicates with the host (thClaws) over `postMessage` JSON-RPC, and can call back into its originating server via `callServerTool`. Display modes (inline / fullscreen / picture-in-picture) and theme/locale propagation are part of the host contract.

This doc covers: server lifecycle, both transports, the OAuth flow, the allowlist gate, qualified-name sanitization, the JSON-RPC layer, the MCP-Apps protocol in detail (trust model, postMessage handshake, display-mode lifts, approval gate), wire-format examples, code organization, testing, and the migration / known-limitations notes from each sprint.

**Source modules:**
- `crates/core/src/mcp.rs` — types, allowlist, McpClient (stdio + HTTP), McpTool adapter, MCP-Apps resource fetch
- `crates/core/src/oauth.rs` — RFC 8414 / RFC 7591 / RFC 9728 / PKCE — discovery, dynamic client registration, browser flow, refresh, token store
- `crates/core/src/shared_session.rs` — worker spawn (MCP server task), `McpReady` / `McpFailed` / `McpAppCallTool` handlers
- `crates/core/src/gui.rs` — IPC bridge: `mcp_call_tool` → `ShellInput::McpAppCallTool`; renders `ui_resource` envelopes
- `crates/core/src/policy/mod.rs` — `external_mcp_disallowed()`, `external_scripts_disallowed()` (EE policy gates)
- `crates/core/src/permissions.rs` — `ApprovalSink` used by stdio-spawn approval AND widget tool-call approval
- `frontend/src/components/McpAppIframe.tsx` — host-side MCP-Apps widget shell
- `frontend/src/components/ChatView.tsx` — mounts `McpAppIframe` from the `chat_tool_result.ui_resource` envelope

**Public-facing repos:**
- Marketplace: MCP entries live alongside skills + plugins in `marketplace/mcp/<name>/` (see [`marketplace.md`](marketplace.md))
- Test fixtures: pinn.ai (`text2image` + `image2image` widgets), the weather MCP, etc.

---

## 1. What thClaws's MCP subset implements

The MCP spec is broad; thClaws implements the subset needed for tool routing + UI widgets.

| Spec area | thClaws | Notes |
|---|---|---|
| `initialize` + `notifications/initialized` | ✅ | Always sent on connect |
| `tools/list` + `tools/call` | ✅ | Tool discovery + invocation |
| `resources/read` | ✅ | Used by MCP-Apps to fetch widget HTML |
| `resources/list`, `resources/subscribe` | ❌ | Not needed for tool routing |
| `prompts/*` | ❌ | Skill subsystem fills the same niche |
| `sampling/*` | ❌ | thClaws is the LLM client, not an LLM provider |
| Bidirectional notifications | ❌ | Server-side notifications without a request ID are silently ignored (`mcp.rs::handle_incoming`) |
| `$/cancelRequest` | ❌ | Per-request 30 s timeout instead |
| stdio transport | ✅ | Primary — most servers ship as stdio binaries |
| Streamable HTTP (POST + SSE) | ✅ | Includes session header (`Mcp-Session-Id`) and 307/308 manual redirect handling |
| OAuth 2.1 + PKCE | ✅ | RFC 8414 metadata, RFC 7591 dynamic client registration, RFC 9728 protected-resource discovery |
| MCP-Apps (HTML widgets via `ui://` resources) | ✅ | Trust-gated, sandboxed iframe, three display modes |

Protocol version sent during `initialize`: `"2024-11-05"` (`mcp.rs:35`).

---

## 2. Server lifecycle

```
USER configures a server → mcp.json or marketplace install
  │
  ▼
WORKER SPAWN (shared_session.rs:726-757)
  │
  ▼
McpClient::spawn_with_approver(config, Some(approver))
  │
  ├── transport == "http"  → connect_http (§3.2)
  │                          ├── pre-resolve OAuth token  (oauth.rs)
  │                          ├── set up duplex bridge
  │                          └── initialize handshake
  │
  └── transport == "stdio" → check_stdio_command_allowed (§4)
                             ├── consult ~/.config/thclaws/mcp_allowlist.json
                             ├── if first-time, route through ApprovalSink
                             ├── spawn child process
                             └── initialize handshake
  │
  ▼
list_tools()  →  Vec<McpToolInfo>  (parses _meta.ui.resourceUri for MCP-Apps)
  │
  ▼
ShellInput::McpReady { server_name, client, tools }
  │
  ▼
For each tool: registry.register(McpTool::new(client, info))
  │  Each McpTool's name = `<sanitized_server>__<sanitized_tool>`
  │  Each McpTool's bare = original tool name (server matches byte-for-byte)
  │
  ▼
state.rebuild_agent(true)   → next turn sees the new tools
```

**Ordering:** servers spawn in **parallel** background tasks (`shared_session.rs:726-757`). Each one independently emits either `ShellInput::McpReady { … }` or `ShellInput::McpFailed { … }` once its handshake completes. The worker's main `select!` loop drains these messages and registers tools as they arrive. A slow / hung server doesn't block faster ones.

**Runtime add via `/mcp add`.** Two shapes routed by argument shape:

| Form | Slash variant | `transport` written |
|---|---|---|
| `/mcp add <name> <url>` (URL starts with `http://` / `https://`) | `SlashCommand::McpAdd` | `"http"` |
| `/mcp add <name> <command> [args...]` (anything else) | `SlashCommand::McpAddStdio` | `"stdio"` |

Both share the `persist_and_register_mcp` helper (`shell_dispatch.rs`): write to `mcp.json` → `McpClient::spawn_with_approver` → `list_tools` → `tool_registry.register` → `rebuild_agent`. Stdio entries flow through the §4 allowlist check on first spawn. The slash form does not accept `--env KEY=VAL`; servers needing env vars (`LDR_LLM_*`, `GITHUB_TOKEN`, …) are saved successfully but error on spawn — the user-facing message points at `mcp.json` so the user can hand-add the `env` block and retry.

**Approval modal coordination:** the `ready_gate` (signaled by frontend's `frontend_ready` IPC after the working-directory + secrets pickers close) gates the worker's main loop start, so an MCP spawn approval modal can't pop up before the user has even chosen a workspace.

---

## 3. Transports

### 3.1 stdio

```rust
let mut cmd = Command::new(&config.command);
cmd.args(&config.args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())     // throw away server logging — too noisy in production
    .kill_on_drop(true);       // McpClient drop kills the child
for (k, v) in &config.env { cmd.env(k, v); }
let child = cmd.spawn()?;
let stdout = child.stdout.take()?;
let stdin = child.stdin.take()?;

let client = Self::from_streams(name, stdout, stdin, config.trusted);
client.initialize().await?;
```

The `kill_on_drop(true)` + `Drop for McpClient { reader_task.abort() }` pair guarantees that dropping the McpClient (e.g. on `/quit`) tears down both the reader task and the child process cleanly. Without `abort()`, the reader's still holding a read-half of the stdio pair on tokio's runtime; on Unix the child can take seconds to notice the EOF.

### 3.2 HTTP (Streamable HTTP)

HTTP is more involved because each JSON-RPC call is an independent POST. We simulate the same stream-pair interface the rest of the client uses by piping through an in-memory `tokio::io::duplex`:

```
[McpClient ←→ duplex] ←→ [bridge task ←→ HTTP server]
```

The bridge task:
1. Reads JSON-RPC lines from the client side
2. Wraps them in `POST <url>` with `Content-Type: application/json` + `Accept: application/json, text/event-stream`
3. Attaches `Authorization: Bearer <token>` if OAuth is in use
4. Attaches `Mcp-Session-Id: <sid>` once the server has issued one
5. Forwards the response back through the duplex (handling SSE → JSON-RPC line conversion via `write_body_to_pipe`)

Two non-obvious behaviors:

**Manual 307/308 redirect handling.** reqwest's auto-redirect strips the `Authorization` header on every redirect (even same-origin 307). The bridge sets `Policy::none()` and follows redirects manually, preserving auth and fixing http→https when the server redirects to a broken http:// Location behind a TLS proxy (`mcp.rs:1085-1131`).

**Mid-session OAuth.** A 401 mid-session triggers `resolve_oauth_token`, which discovers the AS, optionally re-runs the browser flow, and retries the failed request. The session ID is preserved across the retry (re-attached in the retry POST headers). The token store is invalidated FIRST so a stale rejected token isn't returned by `resolve_oauth_token`'s cache-first path.

**Session expiry recovery.** A `400` or `404` containing the body string `"Session not found"` clears the cached `Mcp-Session-Id` and retries without it. The server then issues a fresh session ID in the retry response (`mcp.rs:613-651`).

### 3.3 OAuth 2.1 + PKCE flow (HTTP only)

```
1. POST <mcp_url> {ping}     ← resolve_token_upfront probe
   ├── 200 OK + cached token valid → use it
   └── 401 → trigger discovery
       │
       ▼
2. GET <origin>/.well-known/oauth-protected-resource  (RFC 9728)
   └── { "authorization_servers": ["https://as.example/"] }
       │
       ▼
3. GET <as>/.well-known/oauth-authorization-server     (RFC 8414)
   └── { authorization_endpoint, token_endpoint, registration_endpoint, scopes_supported }
       │
       ├── If registration_endpoint present:
       │     POST <as>/register {redirect_uris, …}   (RFC 7591)
       │     └── Issued client_id (+ optionally client_secret)
       │
       └── Otherwise: fall back to static client_id "thclaws"
       │
       ▼
4. Generate PKCE code_verifier + S256 code_challenge
   Generate random state for CSRF binding
   Bind a localhost ephemeral TCP port (or 19150-19160)
   │
   ▼
5. Open browser → GET <authorize_endpoint>?response_type=code&client_id=…&redirect_uri=http://localhost:<port>/callback
                   &scope=…&state=…&code_challenge=…&code_challenge_method=S256
   │
   ▼
6. User consents; browser redirects to http://localhost:<port>/callback?code=…&state=…
   │
   ▼
7. wait_for_callback validates state == expected (CSRF check), extracts code
   Sends a 200 OK page back to the browser ("You can close this tab…")
   │
   ▼
8. POST <token_endpoint> grant_type=authorization_code
                          code, redirect_uri, client_id, code_verifier
                          [client_secret if confidential client]
   └── { access_token, refresh_token, expires_in, scope }
       │
       ▼
9. Validate granted scope ⊇ requested scope (warn on mismatch, accept narrower grant)
   Store TokenEntry to ~/.config/thclaws/oauth_tokens.json
                       (0600 permissions on Unix; 0700 on parent dir)
                       (authorization_server origin captured for AS-binding check)
   │
   ▼
10. Attach Bearer token to every subsequent MCP POST.
    On expiry: try refresh_token (RFC 6749 §6); on failure, restart at step 5.
```

**Authorization-server binding (`oauth.rs:150-156`).** Every cached `TokenEntry` records the AS origin that issued it. Subsequent token retrievals via `get_validated(server_url, expected_as_origin)` reject the entry if the origins don't match. This blocks: an attacker who can swap the MCP server's `oauth-protected-resource` document to point at a malicious AS cannot then trick thClaws into reusing a previously-issued legitimate token against the new AS — the binding mismatches and re-auth is forced.

**Fragmented callback recovery (M6.15 BUG 6 — `dev-log/133`).** `wait_for_callback` accumulates the request until either `\r\n` (request-line terminator) is seen or 8 KiB / 5 s. A browser sending the GET split across TCP segments would otherwise be missing the `?code=…&state=…` query string on the first read.

---

## 4. stdio command allowlist

Project-scoped MCP configs (`.thclaws/mcp.json`) can come from a cloned repository — a malicious config could point `command` at any binary on PATH. Mitigation: per-command allowlist persisted at `~/.config/thclaws/mcp_allowlist.json`.

```rust
async fn check_stdio_command_allowed(
    config: &McpServerConfig,
    approver: Option<Arc<dyn ApprovalSink>>,
) -> Result<()> {
    if env_var("THCLAWS_MCP_ALLOW_ALL") == "1" { return Ok(()); }    // CI escape hatch
    let allowlist = McpAllowlist::load();
    if allowlist.contains(&config.command) { return Ok(()); }

    if let Some(approver) = approver {
        // GUI mode: route through ApprovalSink → modal in webview.
        // Decision: Allow / AllowForSession → insert + save, then proceed.
    } else {
        // CLI fallback: stderr/stdin prompt (requires TTY).
        // Refuses with a clear "edit allowlist or set THCLAWS_MCP_ALLOW_ALL=1" message
        // when stdin is not a terminal.
    }
}
```

**Atomic save (M6.15 BUG 5).** The allowlist write uses `write(tmp) → rename(tmp, path)` (`mcp.rs:108-117`) so a crash mid-write doesn't corrupt the file. Pre-fix, a corrupt file would silently deserialize as empty and re-prompt for every server.

**Keyed by `command` string.** The allowlist matches `config.command` byte-for-byte. Users who substitute the binary (e.g. via PATH manipulation or a different absolute path) re-trigger approval. This is intentional — a user moving from `weather-mcp` (PATH-resolved) to `/usr/local/bin/weather-mcp` should re-confirm.

---

## 5. Tool registration: qualified names + sanitization

Provider tool-name validation (OpenAI, Anthropic) requires `^[a-zA-Z0-9_-]+$`. MCP tool names can contain dots, slashes, colons. Server names too. We need to disambiguate (`weather.get_forecast` vs `git.get_forecast`?) AND fit the regex.

**Solution (`mcp.rs:898-914`):**
- Qualified name: `<sanitized_server>__<sanitized_tool>` — used to register in `ToolRegistry` and dispatch from the LLM
- Bare name: original tool name, NEVER sanitized — used in the `tools/call name="…"` JSON-RPC field, since the server matches byte-for-byte

```rust
pub fn sanitize_tool_name_segment(s: &str) -> String {
    let out: String = s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if out.is_empty() { "_".into() } else { out }
}
```

Examples:
- Server `pinn.ai`, tool `text2image` → registry name `pinn_ai__text2image`, bare name `text2image`
- Server `git`, tool `log` → registry name `git__log`, bare name `log`

Separator `__` (double underscore) was chosen because it's allowed by the provider regex AND can't appear in a sanitized segment naturally (a single `_` would, and would create ambiguity).

**`McpTool::name()` returns `&'static str`.** The Tool trait predates MCP; it returns leaked owned strings via `Box::leak`. MCP tools are registered once at REPL startup, so the leak is bounded (a few hundred bytes per tool × the configured server set). Documented in the McpTool docstring (`mcp.rs:867-870`).

---

## 6. JSON-RPC layer

`McpClient` implements a tiny JSON-RPC 2.0 client over an `AsyncRead + AsyncWrite` pair.

### 6.1 Wire format

**Request:**
```json
{"jsonrpc":"2.0","id":42,"method":"tools/list","params":{}}
```

**Response (success):**
```json
{"jsonrpc":"2.0","id":42,"result":{"tools":[…]}}
```

**Response (error):**
```json
{"jsonrpc":"2.0","id":42,"error":{"code":-32601,"message":"method not found"}}
```

**Notification (no `id`, no response expected):**
```json
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
```

### 6.2 Pending map + correlation

Numeric ids minted by `AtomicU64::fetch_add(1, SeqCst)`. Each request inserts an `id → oneshot::Sender<Result<Value>>` entry into `pending: Arc<Mutex<HashMap<u64, …>>>`. The reader task parses incoming lines and dispatches:

```rust
fn handle_incoming(msg: Value, pending: &Pending) {
    let Some(id) = msg.get("id").and_then(Value::as_u64) else { return };  // ignore notifications
    let Some(tx) = pending.lock().unwrap().remove(&id) else { return };
    let result = if let Some(error) = msg.get("error") {
        Err(format!("mcp error {}: {}", error["code"], error["message"]))
    } else if let Some(result) = msg.get("result") {
        Ok(result.clone())
    } else {
        Err("mcp response missing both `result` and `error`".into())
    };
    let _ = tx.send(result);
}
```

`request()` waits on the `oneshot::Receiver` with a timeout, removing its own pending entry when that fires so the slot doesn't leak.

**Two budgets, not one.** `request()` uses `request_timeout_secs()` (30 s); `initialize()` goes through `request_within()` with `init_timeout_secs()` (300 s). A stdio server launched via `uvx`/`npx` resolves and downloads its whole dependency tree before it can answer anything — the Qwen `qwen-mm-plugins-core` capability pulls 93 packages and takes ~38 s on a cold cache, then 0.5 s once warm. Under a single 30 s constant that first launch failed with `mcp request timed out: initialize`, and raising the shared constant to cover it would have blunted stall detection on every tool call for the rest of the session. Both are overridable — `THCLAWS_MCP_TIMEOUT_SECS` and `THCLAWS_MCP_INIT_TIMEOUT_SECS` — and a zero or unparseable value falls back to the default rather than disabling the deadline, since an unbounded wait is what these exist to prevent.

The timeout error names the budget that expired (`mcp request timed out after 300s: initialize`); the earlier message said only which method, which read as a hang when it was really a cold install.

### 6.3 Transport-closed flag (M6.15 BUG 4 — `dev-log/133`)

`closed: Arc<AtomicBool>` shared between `McpClient` and the reader task. When the reader observes EOF / read error:

```rust
closed_for_reader.store(true, Ordering::SeqCst);
let pending: Vec<_> = pending_for_reader.lock().unwrap().drain().map(|(_, tx)| tx).collect();
for tx in pending {
    let _ = tx.send(Err(Error::Provider("mcp transport closed".into())));
}
```

`request()` checks `closed` on entry and short-circuits with "transport closed" before allocating an id or writing. Pre-fix, a request after EOF would write into the still-alive writer (data went nowhere), insert into pending, and wait the full 30 s timeout before failing with the misleading "mcp request timed out".

---

## 7. MCP-Apps in detail

MCP-Apps lets a server ship interactive HTML widgets that render alongside its tool results. The widget acts as an MCP client (running the `@modelcontextprotocol/ext-apps` SDK); the host (thClaws) acts as the server, communicating over `postMessage` JSON-RPC.

### 7.1 Trust model

Two distinct "trust" gates apply to MCP servers. Don't conflate them.

| Field | Set when | Gates |
|---|---|---|
| stdio allowlist | First-time user approval (or `THCLAWS_MCP_ALLOW_ALL=1`) | Whether thClaws will spawn the binary at all |
| `McpServerConfig::trusted` | `true` only via marketplace install (or manual `trusted: true` in `.thclaws/mcp.json`) | Whether the server's tools can ship MCP-Apps widgets |

A trusted server's widget can render arbitrary HTML in chat. An untrusted server still works as a normal MCP — the model sees its tool results as text — but no inline iframe is mounted, even if the tool advertises a `ui_resource` URI. Diagnostic message logged once per ignored fetch (`mcp.rs:1012-1020`):

```
[mcp] <server>: ignoring widget resource ui://… (server not trusted; install via
marketplace or set `trusted: true` in mcp.json to enable)
```

**Important: trust does NOT mean "tools run without approval".** This was the M6.15 BUG 2 fix — see §7.6 for the approval gate on widget tool-calls.

### 7.2 Resource URI discovery

Widget-bearing tools advertise their resource URI in the `_meta` object on each tool definition. Per the current MCP-Apps spec, the canonical key is the nested `ui.resourceUri`; older servers (including pinn.ai) also set the legacy flat key `ui/resourceUri`. The host accepts both.

```json
// tools/list response — widget-bearing tool
{
  "name": "text2image",
  "description": "Generate an image from a prompt",
  "inputSchema": { … },
  "_meta": {
    "ui": { "resourceUri": "ui://pinn/image-viewer" },
    "ui/resourceUri": "ui://pinn/image-viewer"
  }
}
```

`extract_ui_resource_uri` (`mcp.rs:252-264`) prefers the nested current-spec key over the legacy flat key. If both are set with different values, the nested form wins — defending against a future server that drifts the legacy key away from the canonical value.

`McpToolInfo` carries `ui_resource_uri: Option<String>`. `McpTool::new` leaks it to `&'static str` (same lifetime trick as name/description) and exposes it via `McpTool::ui_resource_uri()` and `McpTool::fetch_ui_resource()`.

### 7.3 Resource fetch (`fetch_ui_resource`)

When the agent loop calls a tool that has a `ui_resource_uri`, after a successful result it calls `tool.fetch_ui_resource()` (`agent.rs:1165-1169`). For MCP tools, this fetches the resource via `resources/read`:

```json
// resources/read request
{"jsonrpc":"2.0","id":43,"method":"resources/read","params":{"uri":"ui://pinn/image-viewer"}}

// response
{
  "jsonrpc": "2.0",
  "id": 43,
  "result": {
    "contents": [{
      "uri": "ui://pinn/image-viewer",
      "mimeType": "text/html;profile=mcp-app",
      "text": "<html>…widget HTML…</html>"
    }]
  }
}
```

`McpClient::read_resource` returns `(html, mime)`. `McpTool::fetch_ui_resource` wraps it in a `UiResource { uri, html, mime }` after the trust gate.

### 7.4 Wire envelope to the frontend

`ToolCallResult` events carry an optional `ui_resource` field. The GUI's chat dispatcher (`gui.rs:176-200`) inlines it into the `chat_tool_result` JSON envelope:

```json
{
  "type": "chat_tool_result",
  "name": "pinn_ai__text2image",
  "output": "image: https://pinn.ai/img/abc.png",
  "ui_resource": {
    "uri": "ui://pinn/image-viewer",
    "html": "<html>…</html>",
    "mime": "text/html;profile=mcp-app"
  }
}
```

The full HTML ships over IPC. Typical pinn.ai widget is a few KB — simpler than an asset URL we'd have to also serve, and the iframe gets an opaque origin from `srcdoc` so the page can't reach back into our app context.

### 7.5 The host: `McpAppIframe.tsx`

The host iframe is sandboxed (`sandbox="allow-scripts allow-popups allow-forms"` — deliberately NO `allow-same-origin`). Its srcdoc origin is opaque, so the widget can't read cookies/localStorage of the host or any other origin.

**Three display modes:**

| Mode | Surface | Geometry | Trigger |
|---|---|---|---|
| `inline` | In-bubble slot in chat | Fixed 480 px height | Default; restore button on lifted modes |
| `fullscreen` | `position:fixed inset-0 z-[55]` overlay | Full viewport | User toolbar OR widget `request-display-mode` |
| `pip` | Floating draggable panel | 360×260 default; resized with sessionStorage persistence | User toolbar OR widget request |

**Mode lifts preserve the iframe.** Naively re-rendering the iframe in a different parent (via `createPortal` whose target changes) would tear it out and re-mount it — re-running the SDK handshake, re-fetching unpkg, losing the tool-result push. Solution: render the iframe ONCE into a stable detached `<div>` (`useState(() => document.createElement("div"))`), and use `appendChild` to MOVE that div between mount slots when the mode changes. `appendChild` of an attached node moves it without recreating; the iframe's `contentWindow` and message listeners stay intact.

```ts
useEffect(() => {
    const target =
      mode === "inline" ? inlineSlotRef.current
      : mode === "fullscreen" ? fullscreenSlotRef.current
      : pipSlotRef.current;
    if (target && iframeContainer.parentElement !== target) {
      target.appendChild(iframeContainer);   // MOVE, not re-mount
    }
  }, [mode, iframeContainer]);
```

**Tool-bubble opacity bypass (`dev-log/133`).** The chat bubble dims to `opacity: 0.7` once `toolDone` fires, as a visual "this tool finished" cue. The widget inherits that opacity, washing out its content (most visible in light mode). Fullscreen + PIP escape via `createPortal(…, document.body)`, so they render at full opacity. Fix: skip the dim when there's a widget — the widget IS the visible cue.

### 7.6 The host↔widget JSON-RPC protocol

All messages match JSON-RPC 2.0 shape. `event.source !== iframe.contentWindow` is hard-checked so a sibling widget (or any other postMessage) can't cross-talk. `*` is correct as the `targetOrigin` for srcdoc opaque origins.

**Widget → host requests handled:**

| Method | Behavior |
|---|---|
| `ui/initialize` | Reply with `{protocolVersion: "0.4.0", hostInfo, hostCapabilities, hostContext}` |
| `ui/open-link` | Validate `http(s)` URL, route through `open_external` IPC → OS browser |
| `ui/request-display-mode` | If mode ∈ `{inline, fullscreen, pip}` → `setMode(requested)` + reply `{mode: requested}`; otherwise -32602 |
| `tools/call` | §7.7 — forward to backend with approval gate |
| `ui/message` | §7.8 — inject text into the agent as a user message |
| `ui/update-model-context` | -32601 method-not-found (no widget needs it yet) |

**Host → widget notifications sent:**

| Method | Sent when |
|---|---|
| `ui/notifications/tool-result` | After every `ui/notifications/initialized` from the widget — re-pushed on every init so a WebKit reload during a mode lift catches up |
| `ui/notifications/host-context-changed` | Mode change OR theme change |

**`hostCapabilities` shape gotcha.** McpUiHostCapabilities uses **empty-object flags**, NOT booleans:

```js
hostCapabilities: { serverTools: {}, openLinks: {} }   // ✅ "I implement these"

// vs

hostCapabilities: { serverTools: true, openLinks: true }  // ❌ Zod rejects, app.connect() throws silently
```

A truthy non-object value fails the SDK's Zod schema on the widget side, causing `app.connect()` to throw silently and stranding the widget in its spinner state. This was the failure mode that prompted the explicit comment at `McpAppIframe.tsx:316-323`.

### 7.7 Widget tool-call: `app.callServerTool`

The widget calls `app.callServerTool({name: "image2image", arguments: {…}})`. This emits:

```json
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"image2image","arguments":{…}}}
```

Frontend handler (`McpAppIframe.tsx:368-422`):

```ts
case "tools/call": {
    const params = msg.params as { name?: string; arguments?: unknown };
    const bareName = params?.name ?? "";
    const args = params?.arguments ?? {};
    if (!bareName) { respondError(id, -32602, "tools/call: missing 'name'"); break; }
    if (!serverPrefix) { respondError(id, -32603, "cannot determine originating server"); break; }

    // Server prefix is extracted from the parent tool's qualified name.
    // Widget can ONLY call tools on its OWN server — it can't address
    // another MCP server, can't reach Bash, etc.
    const qualifiedName = `${serverPrefix}__${bareName}`;
    const requestId = crypto.randomUUID();
    const timeoutId = setTimeout(() => {
      const stale = pendingCallsRef.current.get(requestId);
      if (!stale) return;
      pendingCallsRef.current.delete(requestId);
      respondError(stale.iframeMessageId, -32000, "tools/call: timed out after 60s");
    }, 60_000);
    pendingCallsRef.current.set(requestId, { iframeMessageId: id, timeoutId });
    send({ type: "mcp_call_tool", requestId, qualifiedName, arguments: args });
    // Don't respond now — the resolver fires when Rust dispatches mcp_call_tool_result.
    break;
}
```

**Approval gate on the worker side (M6.15 BUG 2 fix).** The `ShellInput::McpAppCallTool` handler in `shared_session.rs` mirrors the agent loop's approval pipeline:

```rust
let mode = crate::permissions::current_mode();
let needs_approval = matches!(mode, PermissionMode::Ask | PermissionMode::Plan)
    && t.requires_approval(&arguments);
if needs_approval {
    let req = ApprovalRequest {
        tool_name: qualified_name.clone(),
        input: arguments.clone(),
        summary: Some(format!("MCP-App widget requested `{qualified_name}`. Allow?")),
    };
    let denied = matches!(state.approver.approve(&req).await, ApprovalDecision::Deny);
    if denied { /* return text-only error */ }
}
let result = t.call_multimodal(arguments).await;   // only if approved
```

Pre-fix, widget tool-calls bypassed approval entirely. The trust flag (`McpServerConfig::trusted`) was being implicitly extended from "can render HTML" to "can run tools without approval" — letting a trusted server's widget invoke `delete_account`-style tools silently inside the iframe, even when the user had set `permission_mode = "ask"` or `Plan`. The fix re-routes through the same approver the agent loop uses; the summary line attributes the call to the widget so the user knows the LLM didn't choose this.

**Auto mode unchanged.** When `permission_mode = "auto"` (the GUI default), widget tool-calls execute without prompting — same as agent-initiated tool-calls in auto mode.

### 7.8 Widget message injection: `app.sendMessage`

The widget calls `app.sendMessage({content: [{type:"text", text:"hello"}]})`. This emits a `ui/message` request. Frontend handler:

```ts
case "ui/message": {
    const blocks = (msg.params?.content) ?? [];
    const text = blocks
      .filter(b => b?.type === "text")
      .map(b => b?.text ?? "")
      .join("");
    if (text.trim()) {
      send({ type: "shell_input", text });           // M6.15 BUG 1 fix
      respond(id, { content: [], isError: false }); // M6.15 BUG 3 fix
    } else {
      respond(id, { isError: true, content: [{type:"text", text:"no text content"}] });
    }
    break;
}
```

Two M6.15 bug fixes baked into this handler:

- **BUG 1:** the message type was `"chat_user_message"` — which is an OUTBOUND backend→frontend event with no inbound IPC handler. The default arm at `gui.rs:3075` (`_ => {}`) silently dropped it. The widget got `respond(id, {isError: false})` and thought the send succeeded; the agent never saw the message. Fixed by routing through `"shell_input"` — the same IPC ChatView's composer uses.

- **BUG 3:** the success response was `{isError: false}` alone — not a valid CallToolResult. The MCP-Apps SDK's Zod validator on the widget side rejects this and throws inside `app.sendMessage`'s promise. Fixed by including `content: []`.

### 7.9 Widget→host result correlation

Widget tool-call responses come back via the `mcp_call_tool_result` IPC dispatch. Each `McpAppIframe` instance subscribes and matches by `requestId`:

```ts
return subscribe((msg) => {
    if (msg.type !== "mcp_call_tool_result") return;
    const requestId = msg.requestId as string | undefined;
    if (!requestId) return;
    const pending = pendingCallsRef.current.get(requestId);
    if (!pending) return;
    pendingCallsRef.current.delete(requestId);
    window.clearTimeout(pending.timeoutId);
    iframeRef.current?.contentWindow?.postMessage(
      { jsonrpc: "2.0", id: pending.iframeMessageId, result: { content: msg.content, isError: Boolean(msg.isError) } },
      "*",
    );
});
```

**Fan-out trade-off (M6.15 BUG 7 — deferred).** Every widget runs the lookup on every dispatch. With realistic widget counts (1-3 per chat) the cost is 1-3 HashMap lookups per result — negligible. A global router would be more code than the problem deserves; revisit if widget counts ever grow into the dozens.

**60 s timeout per call.** Generative tools (image2image) routinely run for tens of seconds. Anything longer is a stuck call we want to fail loudly rather than wait on indefinitely.

---

## 8. Wire-format end-to-end

Pinn.ai's `text2image` widget request, end-to-end.

```
USER (chat composer):  "make me an image of a cat"
  │
  ▼
LLM picks pinn_ai__text2image, emits tools/call
  │
  ▼
Agent loop  →  approval gate (auto: skip; ask: modal)  →  tool.call_multimodal
  │
  ▼
McpClient.request("tools/call", {name: "text2image", arguments: {prompt: "a cat"}})
  ├── stdio: write JSON line to child stdin
  └── HTTP: bridge POST → server → response
  │
  ▼
Server returns:
  {"content":[{"type":"text","text":"https://pinn.ai/img/abc.png"}], "isError": false}
  │
  ▼
Tool result text:  "https://pinn.ai/img/abc.png"
  │
  ▼
agent.rs:1165:  if Ok(_) { tool.fetch_ui_resource().await }
  │  Trust gate passes (server marked trusted via marketplace install)
  │  McpClient.request("resources/read", {uri: "ui://pinn/image-viewer"})
  │
  ▼
Server returns widget HTML:
  {"contents":[{"uri":"…","mimeType":"text/html;profile=mcp-app","text":"<html>…</html>"}]}
  │
  ▼
ViewEvent::ToolCallResult { name, output, ui_resource: Some(UiResource { uri, html, mime }) }
  │
  ▼
gui.rs renders chat_tool_result with ui_resource envelope (full HTML inlined)
  │
  ▼
ChatView.tsx mounts <McpAppIframe uri html parentToolName toolResult={…} />
  │
  ▼
McpAppIframe creates iframe with srcDoc=html, sandbox="allow-scripts allow-popups allow-forms"
  │
  ▼
Widget loads, runs SDK, posts ui/initialize → host responds with hostInfo + capabilities
Widget posts ui/notifications/initialized → host posts ui/notifications/tool-result with the URL
Widget displays the image
  │
  ▼ (optional follow-up — widget calls back into the server)
User clicks "regenerate" in widget → widget posts tools/call {name: "image2image", arguments: {…}}
  │
  ▼
Frontend builds qualifiedName = "pinn_ai__image2image", sends mcp_call_tool IPC
  │
  ▼
Worker's ShellInput::McpAppCallTool handler:
  ├── lookup tool in registry
  ├── M6.15: approval gate via state.approver.approve()
  ├── if approved: t.call_multimodal(arguments)
  └── ViewEvent::McpAppCallToolResult { request_id, content, is_error }
  │
  ▼
gui.rs renders mcp_call_tool_result IPC dispatch
  │
  ▼
Frontend matches request_id → posts response back into iframe → widget's promise resolves
```

---

## 9. Code organization

```
crates/core/src/
├── mcp.rs                       ── ~1850 LOC, the whole MCP subsystem
│   ├── McpServerConfig          (json-deserialized server entry — name, transport, command, args, env, url, headers, trusted)
│   ├── McpAllowlist             (~/.config/thclaws/mcp_allowlist.json — atomic write)
│   ├── check_stdio_command_allowed   (first-spawn approval gate)
│   ├── McpToolInfo + extract_ui_resource_uri   (tools/list parsing, dual-key support)
│   ├── McpClient                (writer + reader_task + pending map + closed flag)
│   │   ├── from_streams         (build over arbitrary AsyncRead/Write — used by tests + HTTP bridge)
│   │   ├── spawn_with_approver  (stdio path: allowlist → spawn child → handshake)
│   │   ├── connect_http         (HTTP path: token resolve → duplex bridge → handshake)
│   │   ├── request / notify     (JSON-RPC primitives)
│   │   ├── initialize / list_tools / call_tool / read_resource
│   │   └── is_trusted           (gates fetch_ui_resource)
│   ├── McpTool                  (Tool trait adapter; sanitized qualified name + raw bare name + ui_resource_uri)
│   ├── write_body_to_pipe       (SSE → JSON-RPC line conversion)
│   ├── write_response_lines     (manual 307/308 redirect handling, session-id capture)
│   ├── resolve_token_upfront    (pre-bridge OAuth probe)
│   ├── resolve_oauth_token      (cache check → refresh → AS discovery → browser flow)
│   ├── mcp_debug! macro         (gated behind THCLAWS_MCP_DEBUG=1; M6.15 BUG 8)
│   └── tests                    (~18: handshake, list/call, error surfacing, sanitization, transport-close, dual-key)
│
├── oauth.rs                     ── ~900 LOC, OAuth 2.1 + PKCE
│   ├── TokenStore               (JSONL-ish file at ~/.config/thclaws/oauth_tokens.json, 0600/0700)
│   ├── TokenEntry               (access/refresh, expires_at, AS origin, client_id, client_secret)
│   ├── OAuthMetadata            (endpoints + AS origin)
│   ├── discover                 (RFC 9728 + RFC 8414)
│   ├── register_dynamic_client  (RFC 7591)
│   ├── authorize                (PKCE + browser flow + state validation)
│   ├── refresh                  (RFC 6749 §6)
│   ├── wait_for_callback        (loop-read with 5 s per-read timeout, 8 KiB cap; M6.15 BUG 6)
│   └── tests                    (token storage, AS-binding, state entropy, fragmented callback)
│
├── shared_session.rs
│   ├── WorkerState.mcp_clients              (Vec<Arc<McpClient>> kept alive; drop kills children)
│   ├── ShellInput::McpReady / McpFailed     (background spawn task results)
│   ├── ShellInput::McpAppCallTool           (widget → tool-call route with approval gate)
│   └── ViewEvent::McpAppCallToolResult      (response back to the widget)
│
├── gui.rs
│   ├── "mcp_call_tool" IPC handler          (widget tool-call from frontend)
│   ├── ViewEvent::McpAppCallToolResult dispatch  (mcp_call_tool_result IPC)
│   ├── ToolCallResult.ui_resource envelope inlining
│   ├── is_safe_external_url + open_external_url   (used by widget's ui/open-link)
│   └── pty_spawn → SendInitialState         (initial state including MCP servers list)
│
├── permissions.rs
│   ├── ApprovalSink trait + ApprovalRequest
│   ├── current_mode (PermissionMode::Auto / Ask / Plan)
│   └── set_current_mode_and_broadcast
│
└── policy/mod.rs
    ├── external_mcp_disallowed              (EE: HTTP MCP allowlist gate)
    ├── external_scripts_disallowed          (used by skills, mentioned for parity)
    └── check_url                            (allowlist for marketplace install_url)

frontend/src/components/
├── McpAppIframe.tsx             ── ~980 LOC, the host-side widget shell
│   ├── Stable detached iframe container + appendChild mode lift trick
│   ├── postMessage host loop (initialize / open-link / request-display-mode / tools/call / ui/message)
│   ├── streamingRef + pendingCallsRef       (widget→host call correlation, 60 s timeout)
│   ├── InlineSurface / FullscreenSurface / PipSurface
│   ├── PIP drag handler with sessionStorage rect persistence
│   └── Esc-to-restore in fullscreen
│
└── ChatView.tsx
    ├── chat_tool_result handler → builds widget envelope on msg.uiResource
    ├── McpAppIframe mount with parentToolName + toolResult
    └── opacity bypass when widget is present (dev-log/133)
```

---

## 10. Testing

`mcp::tests` ships ~18 unit tests covering:

**Handshake + lifecycle:**
- `initialize_handshake_sends_initialize_and_initialized`
- `transport_closed_fails_pending_requests_cleanly` — drops both stream halves mid-request
- `request_after_transport_close_fails_fast_without_30s_timeout` — M6.15 BUG 4 regression: closed flag fast-path

**Tool discovery + invocation:**
- `list_tools_parses_inputSchema`
- `list_tools_extracts_ui_resource_uri_from_meta` — both `_meta` and legacy `meta` keys
- `read_resource_returns_text_and_mime`
- `call_tool_returns_joined_text_content`
- `call_tool_surfaces_is_error_as_tool_error`
- `jsonrpc_error_response_becomes_provider_error`

**Adapter:**
- `mcp_tool_impls_tool_trait_and_calls_through`
- `extract_ui_resource_uri_handles_dual_keys` — current-spec nested wins over legacy flat
- `sanitize_tool_name_segment_replaces_disallowed_chars`
- `qualified_name_sanitizes_server_segment_but_call_uses_raw_bare` — pinn.ai bug regression (server name has dot)

**OAuth (`oauth::tests`):**
- `origin_of_rejects_unparseable_urls`
- `state_is_high_entropy_and_unique`
- `save_writes_file_with_0600_permissions` (Unix only)
- `save_tightens_parent_dir_to_0700`
- `get_validated_rejects_mismatched_authorization_server`
- `get_validated_rejects_legacy_entries_without_issuer`
- `wait_for_callback_handles_fragmented_request` — M6.15 BUG 6 regression: GET sent in two TCP segments

**Test harness:** `paired_streams()` builds an `McpClient` against two `tokio::io::duplex` pairs (one per direction) so reader and writer halves are NOT coupled — allows clean EOF signaling on either side. `run_mock_server` drives a closure-based server matching JSON-RPC requests by method + id.

The MCP-Apps approval gate (BUG 2) and the IPC plumbing (BUG 1) are GUI-flow changes and are covered by manual verification against pinn.ai widgets. Adding a unit test for the McpAppCallTool handler would require refactoring the inlined `match` arm into a callable function — deferred.

---

## 11. Migration / known limitations

### M6.15 fixes (`dev-log/133`)

| # | Severity | What | Where |
|---|---|---|---|
| 1 | HIGH | `ui/message` silently dropped (`chat_user_message` → `shell_input`) | `McpAppIframe.tsx:439` |
| 2 | HIGH | Widget tool-calls bypassed approval gate | `shared_session.rs::McpAppCallTool` |
| 3 | MED | `ui/message` success not a valid CallToolResult (added `content: []`) | `McpAppIframe.tsx:440` |
| 4 | MED | New requests after transport close waited 30 s instead of failing fast | `mcp.rs::McpClient` (closed flag) |
| 5 | MED | `McpAllowlist::save` not atomic (now tmp + rename) | `mcp.rs:108-117` |
| 6 | LOW | OAuth callback single-read could lose query string on fragmented packets | `oauth.rs::wait_for_callback` |
| 8 | LOW | Stderr noise + bearer-token-prefix leak (now gated behind `THCLAWS_MCP_DEBUG=1`) | `mcp.rs::mcp_debug!` |

### Deferred / not addressed

- **BUG 7** (broadcast fan-out to all McpAppIframe instances). With realistic widget counts the cost is negligible; a global router would be more code than the problem.
- **Resources/list, resources/subscribe, prompts/*, sampling/***. Not needed for the tool-routing + MCP-Apps use case. If a server requires them, it'll fail at handshake-time or silently no-op.
- **Bidirectional notifications without IDs.** `handle_incoming` ignores anything without an `id`; if a future server uses notifications for state push, the host won't observe them.
- **`$/cancelRequest`.** No way to cancel an in-flight `tools/call` — the per-request 30 s timeout is the only escape hatch.
- **Image / multimodal content blocks in widget tool-call results.** Widget→host `tools/call` currently text-flattens via `result.to_text()` (`shared_session.rs:1080`). Pinn.ai's image2image returns a URL string so text-only suffices today.
- **Per-tool allowlist for MCP tools.** `McpTool::requires_approval` always returns `true`. A future enhancement could surface per-tool annotations from `tools/list` (read-only flag, side-effect classification) so safe operations like `list_files` skip the approval prompt under `Ask` mode.

### Sprint chronology

| Sprint | Dev-log | What shipped |
|---|---|---|
| Phase 15a | (initial) | Stdio MCP client, JSON-RPC layer, McpTool adapter, qualified-name sanitization |
| HTTP transport | `~120` | Streamable HTTP, OAuth 2.1 + PKCE, RFC 7591 DCR, AS-binding, session-id handling, manual 307/308 |
| MCP-Apps Phase 1 | `112` | `_meta.ui.resourceUri` discovery, trust flag, ChatView iframe mount, postMessage host loop |
| MCP-Apps modes | (recent) | Inline / fullscreen / PIP; stable iframe container + appendChild mode lift; theme + locale propagation; symmetric mode change via `request-display-mode` |
| pinn.ai e2e | (recent) | Dual-key meta fallback, dotted server-name sanitization, redirect handling, session expiry retry |
| M6.15 audit | `133` | Widget→host plumbing fix, approval gate, atomic allowlist, transport-close fast-fail, OAuth fragmented-callback, debug gate + token-prefix leak removal |
