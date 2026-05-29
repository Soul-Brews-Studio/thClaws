# Chapter 25 — Workflows

Workflows are thClaws's **fourth orchestration tier**: Claude writes a
JavaScript script that fans work out across many subagents, and a
sandboxed JS engine runs that script deterministically on your
machine. Unlike subagents (Chapter 15), `/agent` side-channels, or
Agent Teams (Chapter 17), the orchestrator here is **code**, not the
model — which means rerunning the same workflow gives the same shape
of work every time and a long-running job leaves a checkpoint on disk.

Workflows are **Tier 1** in v0.23 — fan-out works, schema validation
and resume land in Tier 2 (see "What's missing in Tier 1" below).

## When to use workflows

Use workflows for **bulk, deterministic, mostly-independent work**:

- "rewrite all 800 test files to use the new fixture"
- "for every `.md` page under `kms/bug/`, translate it to Thai"
- "audit each crate's `Cargo.toml` and flag deprecated deps"

Use the `Task` tool (Chapter 15) for **one-shot model-driven
side-quests** the agent decides to spawn during a normal turn — that's
what subagents are still for.

Use `/agent` (Chapter 15) when **you** know exactly what a specialist
should do and want it running concurrently with your main session.

Use Agent Teams (Chapter 17) when teammates need to **collaborate**
— exchange messages, debate hypotheses, coordinate on a shared task
list. Workflows are for stateless fan-out; teams are for stateful
collaboration.

## Quick start

```text
/workflow run summarize each .rs file under src/ in one line
```

What happens, in order:

1. **Author phase.** Claude writes a JavaScript script using the
   `thclaws.*` API (the API is detailed in the model's system prompt,
   so the script you get back already knows what's available).
2. **Review.** The script is printed with line numbers. You're
   prompted:
   ```text
   [a]pprove · [c]ancel · [r]e-author:
   ```
   - `a` — run the script as written.
   - `c` — drop the workflow.
   - `r` — give a one-line revision note ("use the read tool not bash
     cat") and Claude rewrites the script with that feedback. Loop
     until you `a` or `c`.
3. **Execute.** A workflow id (`wf-…`) prints, then each subagent
   invocation shows a progress line:
   ```text
   ✓ w0  List every .rs file under src/, recursively. Return o…   2s
   ✓ w1  Read crates/core/src/agent.rs and write ONE sentence …   3s
   ✓ w2  Read crates/core/src/repl.rs and write ONE sentence d…   4s
   …
   workflow done — 47 workers, total 1m 12s
   crates/core/src/agent.rs — the streaming agent loop
   crates/core/src/repl.rs — REPL command parser + rustyline I/O
   …
   ```

If a worker errors, you see `✗ wN  …` for that line and the script
typically catches and continues (depending on what Claude wrote).

## The `thclaws.*` API

Your script gets exactly one global — `thclaws` — with these
fields:

```js
thclaws.subagent({
  prompt: string,           // required — the worker's task
  // schema?, budget?, retry?, model? — Tier 2; ignored in Tier 1
}) → string                 // worker's final assistant text
```

That's it for Tier 1. Workers inherit the parent session's provider,
model, system prompt, tool registry, memory, KMS, and permission mode
— so a worker can `Bash`, `Read`, `Edit`, search KMS, use MCP servers,
etc. Subagent recursion (a worker calling Task itself) is bounded by
the same `DEFAULT_MAX_DEPTH = 3` ceiling sub-agents already honour.

**`thclaws.subagent` is synchronous in Tier 1** — it returns the
worker's text directly, no Promise. Don't write `await
thclaws.subagent(...)`; the sandbox runs Boa in Script mode, where
top-level `await` is a syntax error. Real async + `Promise.all`
parallelism lands in Tier 2 with Module-mode execution.

### What you can write in the script

Vanilla JS control flow: `for`, `while`, `if`/`else`, `try`/`catch`,
destructuring, template literals, `Array` and `String` methods, regex,
JSON parsing.

### What you can't write

- `await`, `async` functions, `Promise.*` (Tier 2)
- `eval`, `Function` (stripped from the sandbox)
- `fetch`, `require`, `process`, DOM, `console.log`

Anything I/O-flavoured must go through a subagent.

### A two-line example

```js
// Workflow: list .rs files, summarise each
const list = thclaws.subagent({
  prompt: "List every .rs file under src/, recursively. Paths only."
});
const paths = list.split("\n").map(s => s.trim()).filter(Boolean);

const summaries = paths.map(p => thclaws.subagent({
  prompt: `Read ${p} and write ONE sentence describing what it does.`
}));

paths.map((p, i) => `${p} — ${summaries[i]}`).join("\n");
```

The script's **final expression** is what becomes the assistant's
output — here the joined list.

## State on disk

Every run writes a JSONL log to:

```text
.thclaws/workflows/wf-<id>/state.jsonl
```

One event per line, flushed after each write so a Ctrl-C leaves the
file in a recoverable shape. Event shapes:

```jsonl
{"ts":"…","kind":"start","id":"wf-…","prompt":"…","script_sha":"…","script_chars":234}
{"ts":"…","kind":"worker_start","id":"wf-…","worker":"w0","prompt":"…"}
{"ts":"…","kind":"worker_done","id":"wf-…","worker":"w0","output":"…"}
{"ts":"…","kind":"worker_error","id":"wf-…","worker":"w1","error":"…"}
{"ts":"…","kind":"done","id":"wf-…","result":"…"}
```

You can `cat`, `grep`, or `jq` the file at any time — it's plain
JSONL, never opaque. Tier 2 will add `/workflow list`, `/workflow
inspect <id>`, and `/workflow rm <id>` so you don't have to navigate
to the directory yourself.

If `.thclaws/` can't be written (read-only volume, permissions), the
workflow runs anyway and prints:
```text
/workflow run: state.jsonl unavailable — proceeding without checkpoint
```
You lose the audit trail but not the run.

## Headless mode

`thclaws -p "/workflow run <goal>"` is **refused**. The author phase
produces a script that needs your review before execution; `-p` mode
has no surface for that review and default-approving an arbitrary
script is dangerous.

A pre-authored script can run headless via `thclaws --workflow
<file.js>` — that's Tier 2 (it needs the file-input plumbing and the
`--resume` machinery), so for now keep workflows in interactive REPL
mode.

## What's missing in Tier 1

These are documented gaps, not bugs — they land in Tier 2 / 3 per
[dev-plan/32](../dev-plan/32-dynamic-workflows.md) (workspace-only):

- **Synchronous subagent calls; no `await` or `Promise`.** Boa runs
  scripts in Script mode where top-level `await` is a syntax error, so
  `thclaws.subagent(...)` is exposed as a synchronous function that
  returns the worker's text directly. Calls fan out sequentially in
  source order — wall-clock time is the sum of subagent latencies, not
  the max. Tier 2 ships Module mode + a tokio-integrated job executor
  so `await`, `async`, and `Promise.all` come back with genuine
  parallelism behind them; until then, write workflows assuming serial.
- **No schema validation.** The `schema:` option is accepted but
  ignored. Workers return free-form text. Tier 2 wires `jsonschema`
  validation + auto-retry on shape failure.
- **No `--resume`.** The state.jsonl log is written but not read back
  yet. A crash partway through a 200-worker run currently means
  starting over. Tier 2 implements log-replay resume with call-site
  matching so already-completed workers aren't re-spawned.
- **No budget caps.** Per-worker `budget: { tokens, time }` is
  ignored. Tier 2 enforces both.
- **No verification phase.** `thclaws.verify({...})` doesn't exist
  yet — Tier 3.
- **No GUI worker grid.** From the chat tab `/workflow run` is
  explicitly refused with a one-line explanation. The interactive
  review UX doesn't fit a single chat bubble, and a real-time grid of
  worker progress is a Tier 3 frontend deliverable.

## Cost awareness

Each `thclaws.subagent` call is a separate model turn — typically a
few seconds and a few hundred to a few thousand tokens. A 200-worker
workflow can easily burn $5–$20 of API tokens depending on the model.
Two practical guardrails:

- **Cap the fan-out before writing the script.** If the goal is
  unbounded ("every file"), have a *discovery* subagent return the
  list first so you see the cardinality before approving the script.
- **Watch the close-out summary.** `workflow done — N workers, total
  Xm Ys` tells you how much you spent on wall time; Tier 2 will add a
  rolled-up token + dollar figure to that line.

## Quick reference

| | Subagent (`Task`) | `/agent` | Agent Teams | Workflow |
|---|---|---|---|---|
| Who orchestrates | The model | You (one-shot) | Team-lead model | Code |
| Number of workers | 1 (blocking) | 1 (concurrent) | 3–5 collaborators | Tens to hundreds |
| Inter-worker chat | No | No | Yes (mailbox) | No (stateless) |
| Determinism | Model-driven | Model-driven | Model-driven | Deterministic execution |
| Resumable | No | No | Limited | Logged (Tier 2 reads it back) |
| Best for | Side-quest during a turn | Specialist running in parallel | Debate / collaboration | Bulk fan-out |

## Troubleshooting

**"workflow: state.jsonl unavailable — proceeding without checkpoint"**
— `.thclaws/workflows/` can't be created or written. Check
permissions on `.thclaws/` in the project root.

**Script error: `ReferenceError: thclaws is not defined`** — you're
probably running a script outside `/workflow run`. The `thclaws.*`
global only exists inside the workflow sandbox.

**Workflow hangs after `⠋ wN  …` line** — that worker is taking a
while. Subagent calls have no timeout in Tier 1; Ctrl-C cancels the
whole run.

**Re-author loop keeps producing the same script** — Claude may be
ignoring your revision note. Try cancelling and re-running with a
sharper goal phrasing rather than relying on `r`-loops.
