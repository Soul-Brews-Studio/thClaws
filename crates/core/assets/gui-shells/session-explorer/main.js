// Session Explorer — GUI Shell for thClaws.
//
// Browsing sessions is a data question, so it goes through the sessions
// bridge (thclaws.sessions.list / .load) rather than the agent. The
// original version asked the model to `ls ./.thclaws/sessions/` and
// parsed its prose — which broke the day sessions moved under
// `.thclaws/state/sessions/`: the agent correctly reported an empty
// directory and the shell showed "no sessions found". It also spent a
// model turn, and a paragraph of commentary instead of the expected
// `file | snippet` lines produced the same empty list.
//
// The agent is still here for what it is actually good at: answering
// questions about a session (thclaws.run + on("text")).

/** Ask the agent about one session, by id. */
function summarisePrompt(id) {
  return `Read the session file .thclaws/state/sessions/${id}.jsonl and summarise it: what the user was trying to do, what you did, and how it ended. A short paragraph, no preamble.`;
}

const statusEl     = document.getElementById("status");
const askInput     = document.getElementById("ask-input");
const askBtn       = document.getElementById("ask-btn");
const cancelBtn    = document.getElementById("cancel-btn");
const answerEl     = document.getElementById("answer");
const sessionList  = document.getElementById("session-list");
const sessionCount = document.getElementById("session-count");
const welcomeEl    = document.getElementById("welcome");
const detailEl     = document.getElementById("detail");
const transportEl  = document.getElementById("transport-badge");

let activeRunId = null;
let activeAccumulator = "";
let activeTarget = null;     // element receiving streamed text
let activeOnDone = null;     // callback fired on done event for this run

transportEl.textContent = `transport: ${thclaws.transport}  ·  session: ${thclaws.shell.sessionId ?? "(new)"}`;

// ---- bridge wiring ----------------------------------------------------

// Host full-screen exit control — render our own button so the host
// hides its fallback chip (see thclaws.ui). Guarded for older engines.
(() => {
  const exitBtn = document.getElementById("exit-fullscreen");
  if (!exitBtn || !thclaws.ui) return;
  exitBtn.addEventListener("click", () => thclaws.ui.exitFullscreen());
  thclaws.ui.onFullscreen((active) => {
    exitBtn.hidden = !active;
    if (active) thclaws.ui.claimExitControl();
  });
})();

thclaws.on("text", (payload) => {
  const chunk = typeof payload === "string" ? payload : payload?.text ?? "";
  if (!chunk) return;
  activeAccumulator += chunk;
  if (activeTarget) activeTarget.textContent = activeAccumulator;
});

thclaws.on("done", () => {
  setRunning(false);
  const done = activeOnDone;
  const text = activeAccumulator;
  activeRunId = null;
  activeOnDone = null;
  activeAccumulator = "";
  activeTarget = null;
  if (done) done(text);
});

thclaws.on("error", (payload) => {
  setRunning(false);
  const msg = payload?.error ?? "agent error";
  if (activeTarget) activeTarget.textContent += `\n\n[error] ${msg}`;
  activeRunId = null;
  activeOnDone = null;
  activeAccumulator = "";
  activeTarget = null;
});

function setRunning(running) {
  askBtn.disabled = running;
  cancelBtn.disabled = !running;
  statusEl.textContent = running ? "running…" : "";
}

async function runPrompt({ prompt, targetEl, onDone }) {
  activeAccumulator = "";
  activeTarget = targetEl;
  activeOnDone = onDone || null;
  setRunning(true);
  if (targetEl) targetEl.textContent = "";
  try {
    const { runId } = await thclaws.run(prompt);
    activeRunId = runId;
  } catch (err) {
    setRunning(false);
    if (targetEl) targetEl.textContent = `[bridge error] ${err.message}`;
    throw err;
  }
}

// ---- UI handlers ------------------------------------------------------

askBtn.addEventListener("click", () => {
  const q = askInput.value.trim();
  if (!q) return;
  showWelcome();
  answerEl.innerHTML = "<div class='node assistant'><div class='node-header'><span class='node-kind'>answer</span></div><div class='node-body' id='answer-body'></div></div>";
  const body = document.getElementById("answer-body");
  runPrompt({ prompt: q, targetEl: body });
});

cancelBtn.addEventListener("click", () => {
  if (activeRunId != null) thclaws.cancel(activeRunId);
});

askInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    askBtn.click();
  }
});

// ---- session list (from the session store, not the model) -------------

let sessions = [];
let filterText = "";
let openId = null;

async function loadSessions() {
  sessionList.className = "empty";
  sessionList.textContent = "loading…";
  try {
    const res = await thclaws.sessions.list();
    sessions = Array.isArray(res && res.sessions) ? res.sessions : [];
    renderSessions();
  } catch (err) {
    sessionList.className = "empty";
    sessionList.textContent = `could not read sessions: ${err.message}`;
  }
}

function visibleSessions() {
  const q = filterText.trim().toLowerCase();
  if (!q) return sessions;
  return sessions.filter(
    (s) =>
      String(s.id).toLowerCase().includes(q) ||
      String(s.title || "").toLowerCase().includes(q),
  );
}

function fmtWhen(updatedAt) {
  if (!updatedAt) return "";
  // The store keeps seconds; Date wants millis.
  const d = new Date(updatedAt * 1000);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString();
}

function renderSessions() {
  const rows = visibleSessions();
  sessionCount.textContent = `(${rows.length}${rows.length === sessions.length ? "" : ` of ${sessions.length}`})`;
  if (!rows.length) {
    sessionList.className = "empty";
    sessionList.textContent = sessions.length
      ? "nothing matches that filter"
      : "no sessions in this workspace yet";
    return;
  }
  sessionList.className = "";
  sessionList.innerHTML = "";
  for (const s of rows) {
    const el = document.createElement("div");
    el.className = "session-row" + (s.id === openId ? " active" : "");
    el.dataset.sessionId = s.id;

    const id = document.createElement("div");
    id.className = "id";
    id.textContent = s.title || s.id;
    id.title = s.id;
    el.appendChild(id);

    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = [
      `${s.messageCount ?? 0} msg`,
      s.model || null,
      fmtWhen(s.updatedAt),
    ]
      .filter(Boolean)
      .join(" · ");
    el.appendChild(meta);

    el.addEventListener("click", () => openSession(s));
    sessionList.appendChild(el);
  }
}

function showWelcome() {
  welcomeEl.hidden = false;
  detailEl.hidden = true;
}

async function openSession(meta) {
  openId = meta.id;
  renderSessions();
  welcomeEl.hidden = true;
  detailEl.hidden = false;
  detailEl.innerHTML = "";

  const head = document.createElement("div");
  head.className = "detail-head";
  const h1 = document.createElement("h1");
  h1.textContent = meta.title || meta.id;
  head.appendChild(h1);
  const sub = document.createElement("span");
  sub.className = "session-meta";
  sub.textContent = [meta.id, meta.model, fmtWhen(meta.updatedAt)].filter(Boolean).join(" · ");
  head.appendChild(sub);
  detailEl.appendChild(head);

  const actions = document.createElement("div");
  actions.className = "detail-actions";
  const summarise = document.createElement("button");
  summarise.textContent = "Summarise with the agent";
  summarise.addEventListener("click", () => {
    const box = document.createElement("div");
    box.className = "turn";
    const who = document.createElement("div");
    who.className = "who";
    who.textContent = "summary";
    const body = document.createElement("div");
    body.className = "body";
    body.textContent = "…";
    box.append(who, body);
    detailEl.appendChild(box);
    runPrompt({ prompt: summarisePrompt(meta.id), targetEl: body });
  });
  actions.appendChild(summarise);
  detailEl.appendChild(actions);

  const transcript = document.createElement("div");
  transcript.textContent = "loading transcript…";
  transcript.className = "session-meta";
  detailEl.appendChild(transcript);

  try {
    const res = await thclaws.sessions.load(meta.id);
    const msgs = Array.isArray(res && res.messages) ? res.messages : [];
    transcript.remove();
    if (!msgs.length) {
      const empty = document.createElement("div");
      empty.className = "session-meta";
      empty.textContent = "This session has no messages.";
      detailEl.appendChild(empty);
      return;
    }
    for (const m of msgs) {
      const turn = document.createElement("div");
      // `usage` rows are the per-turn cost footers the chat surfaces
      // show; they read as a caption here, not a speaker.
      turn.className = `turn ${m.role === "user" ? "user" : m.role === "usage" ? "usage" : m.role === "tool" ? "tool" : "assistant"}`;
      if (m.role !== "usage") {
        const who = document.createElement("div");
        who.className = "who";
        who.textContent = m.role === "user" ? "user" : m.role === "tool" ? "tool" : "assistant";
        turn.appendChild(who);
      }
      const body = document.createElement("div");
      body.className = "body";
      body.textContent = m.text ?? m.content ?? "";
      turn.appendChild(body);
      detailEl.appendChild(turn);
    }
  } catch (err) {
    transcript.textContent = `could not read that session: ${err.message}`;
  }
}

// ---- wiring -----------------------------------------------------------

document.getElementById("refresh").addEventListener("click", loadSessions);
document.getElementById("filter").addEventListener("input", (e) => {
  filterText = e.target.value;
  renderSessions();
});

loadSessions();
