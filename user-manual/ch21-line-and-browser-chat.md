# Chapter 21 — LINE chat & web browser bridge

Drive thClaws from your phone — either as a LINE conversation
(via the `@thClaws` OA bot) or as a chat surface in any web
browser. Both routes share the same Rust agent loop on your
desktop; only the surface changes. Added in v0.9.0+ across the
plan-07 / plan-08 / plan-10 series.

## Why bother

- Approve `Bash` commands from your phone while the desktop runs
  unattended.
- Continue a chat away from your laptop — type on your phone, the
  desktop's full tool registry (Bash, Edit, KMS, MCP, skills)
  executes locally.
- Drive long-running tasks without leaving your machine docked at
  the desk.

The desktop never goes away — your code, secrets, and tools stay
local. The phone / browser surfaces are read+input bridges only.

## How it works (one paragraph)

A small Axum service at `line.thclaws.ai` (and `chat.thclaws.ai`
for the browser variant) holds a WebSocket connection from your
desktop and routes LINE inbound messages / browser keystrokes to
it. The desktop runs the agent unchanged and fans every assistant
delta, tool call, and approval prompt back through the same WS so
the phone or browser sees the conversation as it streams. Sessions
in LINE are pinned to your LINE user id; sessions in the browser
are authenticated with a one-time magic link the LINE bot mints.

## Pairing your phone (LINE)

One-time setup:

1. **Add the LINE OA** — scan the QR code at
   [`thclaws.ai/line`](https://thclaws.ai/line) (or search for
   `@thClaws` in LINE).
2. In thClaws, open Settings → **LINE** → **Pair phone**. The
   modal shows a 6-character code (e.g. `KJ4-9P2`).
3. Send that code to the LINE OA. It replies "Paired ✓ as
   *<your-line-display-name>*".
4. The sidebar's LINE chip lights up green. You're connected.

After pairing, every message you send to `@thClaws` flows into
thClaws's chat session on the desktop. The agent runs there,
streams responses back, and the LINE bot relays them as bubbles.
Tool calls that need approval (Bash, Edit, Write) trigger LINE
Quick Reply chips — tap **[Approve]** or **[Deny]** from the
phone.

## LINE OA commands

Once paired, the LINE bot recognizes a small set of text
commands. Anything else is treated as a chat message.

| You type | What happens |
|---|---|
| `/chat` | Mints a magic link to the browser chat (see below) and replies with it. The link is single-use, 10-min TTL |
| `/pair` | Re-issues a pairing code — useful if you disconnect thClaws then want a new session |
| `/unpair` | Forgets this LINE user id. Next message gets a fresh pairing code, not a chat |
| `/status` | Prints whether thClaws is reachable from the relay right now |
| anything else | Routed to the desktop's chat session as a normal user message |

If thClaws is paired but the desktop is offline (laptop closed,
network dropped), the bot replies "thClaws is offline" rather
than swallowing your message silently.

## Browser chat (the `/chat` path)

LINE bubbles are great for short approvals and quick prompts but
get awkward for code blocks, long responses, and markdown
rendering. Send `/chat` to the OA and you get back a magic link:

```
https://chat.thclaws.ai/launch?token=...
```

Open it in any browser — the link auto-redirects through a splash
page (which exists to dodge LINE's URL-preview crawler that would
otherwise burn the token before you tapped). After the redirect
you land on a full-fidelity chat surface:

- Sidebar shows your session id, sign-out button, and a live
  "browser connected" indicator on the desktop side.
- Assistant responses render as markdown with syntax-highlighted
  code blocks (via vendored marked.js + DOMPurify — all
  rendering stays in the browser; no remote loaders, no eval).
- History replays automatically on connect — even mid-session
  reconnects pick up where you left off (last ~50 messages,
  served from a Redis stream on the relay).
- Tool approvals open an inline modal with **[Approve] [Deny]**
  buttons instead of routing to LINE Quick Replies.
- Sessions expire after 10 minutes of idle — three reconnect
  failures in a row trigger a "session expired" splash that
  points you back to `/chat` in LINE for a fresh link.

The browser link is **per-session, single-use, HTTPS only,
HttpOnly cookie**. Sharing it is identical to handing someone
your desktop session — don't.

## Rich-menu shortcut (v0.9.3+)

If your phone shows the LINE OA's rich menu (the bottom toolbar
with custom buttons), it has two pinned buttons:

- **Chat** — equivalent to typing `/chat`. One tap to get a
  magic link to the browser chat.
- **Pair** — equivalent to typing `/pair`. Quick re-issue of a
  pairing code if you disconnected.

Operators who deploy their own LINE OA can install the rich menu
with the `dev-plan/08-line-server-k3s/rich-menu-setup.sh` script —
see [`docs/line-rich-menu-setup.md`](../../docs/line-rich-menu-setup.md)
for the full setup walk-through.

## Approvals from the phone or browser

When LINE is bridged, the runtime permission mode is `linegated`
(see [Chapter 5](ch05-permissions.md)) and **every approval
request routes through LINE regardless of which surface you typed
the original request on** — Terminal tab, Chat tab, REPL, or LINE
itself. The approver is a process-wide singleton with no
awareness of the originating surface; while paired, your phone is
the single approval inbox.

- **Browser chat (`/chat`) open:** the approval modal pops up in
  the browser with the tool name, a full argument preview, and
  **[Approve] [Deny]** buttons — better UX than LINE Quick Reply
  chips for long arg previews. Approving in either surface
  dismisses both.
- **Browser chat closed (or never minted):** falls back to LINE
  OA Quick Reply. The bot pushes a bubble like:

  ```
  thClaws wants to run:
    bash -c "ls -la ~/Downloads"

  [Approve]  [Deny]
  ```

  Tap a chip; the answer flows back to the desktop within ~1 s.

**Bypass while paired**, if you don't want approvals routing to
your phone:

- `/permissions auto` — overrides `linegated`; mutating tools then
  run without prompting anywhere. Persists to `settings.json` and
  survives LINE disconnect / reconnect.
- Disconnect LINE from Settings → LINE Connect — restores your
  pre-LINE mode (typically `auto` or `ask`) immediately.

When LINE is **not** paired, the desktop's own approval modal
pops up as usual — phone/browser routing is only active while the
bridge is connected.

## Uploading files from the phone or browser

You can attach files from either surface — the desktop saves them
into `<workspace>/uploads/` and an `AGENT.md` in that directory
tells the agent what to do with the file. Added in v0.9.6.

**Caps:**
- **25 MB per file** (`UPLOAD_MAX_BYTES`).
- **5 files per message** (`UPLOAD_MAX_FILES`).
- Any MIME type — text, image, PDF, archive. The desktop doesn't
  unpack or transform; it just lands the bytes in the workspace.

**Filename collisions** are resolved by appending `_n` before the
extension. If you upload `notes.md` and `notes.md` already exists,
the second lands as `notes_1.md`; a third as `notes_2.md`. Original
filenames are preserved (modulo path-traversal sanitisation —
`../../etc/passwd` lands as `passwd`).

**From the browser chat** (`chat.thclaws.ai`): drag-and-drop files
onto the chat surface, or click the paperclip icon next to the
composer. The desktop sends a synthetic chat message describing
the upload (filename + size) followed by a `Read the file and
respond.` directive line, so the agent treats the drop as a
request to act on the contents — not just an FYI. (Pre-v0.9.7,
the synth was purely informational and some models would reply
"what would you like me to do with this?". Project-level
`AGENT.md` / `CLAUDE.md` can override the directive if that
behavior was actually what you wanted.)

**From LINE**: send the file as a normal LINE attachment (photo,
video, file). The relay forwards the upload reference via the
broker channel; the desktop fetches the bytes from LINE's CDN
using the channel access token and saves them locally. The agent
then sees the same synthetic message shape as the browser path,
including the same `Read the file and respond.` directive.

**Where to control behavior:** drop an `AGENT.md` at
`<workspace>/uploads/AGENT.md` (or at the workspace root if you
prefer one rule for everything). The agent reads it as part of
the standard CLAUDE.md / AGENT.md cascade and applies whatever
directives it contains: "OCR every uploaded PDF and stash the
text under `kms/sources/`", "auto-rename screenshots from `Photo
2026-…` to a slug", etc. Without an `AGENT.md`, the file just
sits there waiting for you to tell the agent what to do next.

## Privacy and trust boundary

- **Desktop never proxies upstream LLM calls through the relay.**
  Your prompts go from the desktop straight to Anthropic / OpenAI
  / etc. The relay only carries the user-facing messages between
  the surfaces and the desktop.
- **The relay can see message content** in transit (it has to
  route it). Host it yourself if you don't want a third party
  reading your prompts — the relay binary is `crates/line-server/`
  in the workspace fork; the public OSS distribution doesn't ship
  it. See plan-08 in the workspace `dev-plan/` for the k3s
  deployment shape.
- **Tokens / API keys never leave the desktop.** The relay holds
  one LINE channel secret (for signature verification) and a
  Postgres-stored user profile cache (name + LINE user id) per
  paired user — nothing more.
- **LINE pairing tokens are single-use, 10-min TTL, hashed
  server-side.** A stolen pairing code is useless once the OA
  has emitted the "Paired ✓" reply.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `/chat` link shows "expired" on first tap | LINE's URL preview crawler consumed the token | Open the link from the LINE chat directly, not by tapping a forwarded copy |
| LINE bot replies "thClaws is offline" | Desktop's WS disconnected (sleep, network) | Bring the desktop online; pairing persists |
| Browser chat freezes "Opening thClaws Chat…" | Browser blocked the inline auto-submit script | Confirm the CSP allows `script-src 'self' 'unsafe-inline'` on `/launch` |
| LINE Quick Reply buttons don't appear on approval | Browser chat is also open — approval went there instead | Either approve in the browser or close the browser tab and the next approval falls back to LINE |
| Pairing code stays "(none)" after typing it | Code was older than 10 min, or already used | Open the Pair modal again to mint a fresh code |
| "browser connected" pill doesn't appear on the desktop | Magic link token TTL elapsed before you opened it | Send `/chat` again from LINE for a fresh link |

## Status command on the desktop

`make line-status` (from the workspace root) prints a per-user
status table joining Postgres profiles with Redis presence
flags — useful for operators running their own LINE relay:

```
$ make line-status
user_id            paired  present  browser  last_seen
U1a2b3...          ✓       ✓        -        2 min ago
U9z8y7...          ✓       -        -        3 days ago
```

`paired` = ever-paired, `present` = WS connected right now,
`browser` = `/chat` browser session active, `last_seen` =
most recent webhook activity.

## What's NOT in this chapter

- Internal architecture (broker channel multiplex, WS protocol,
  Redis stream layout) — see the technical manual's
  [`line-bridge.md`](../../thclaws-technical-manual/line-bridge.md).
- LINE OA setup from scratch (channel secret, webhook URL, rich
  menu install) — operator-side work documented in
  [`docs/line-rich-menu-setup.md`](../../docs/line-rich-menu-setup.md)
  and the plan-08 workspace docs.
- Cloud gateway (paid SaaS proxy) — shipped in v0.9.6 as
  `gateway.thclaws.ai`. See [Chapter 6](ch06-providers-models-api-keys.md)
  for the user-facing sign-in + per-provider toggle, and the
  technical manual's
  [`provider-thclaws-gateway.md`](../../thclaws-technical-manual/provider-thclaws-gateway.md)
  for the wire shape.
