//! PTY-backed live shell for the GUI's `Shell` tab.
//!
//! Spawns a child process (defaulting to `$SHELL`) under a pseudo-tty
//! so the GUI can host a real terminal alongside the agent-rendered
//! `Terminal` tab. The wry webview talks to this module via IPC
//! handlers in `ipc.rs` (`pty_open`, `pty_input`, `pty_resize`,
//! `pty_close`); bytes flowing back from the child reach the
//! frontend as `pty_data` events.
//!
//! Ported from the (otherwise-unused) Tauri sibling
//! `src-tauri/src/pty.rs`. The lifecycle here is identical:
//! `spawn` returns a `PtySession` that owns the master/writer/child
//! plus a background reader thread; `write` injects keystrokes,
//! `resize` propagates window changes via `TIOCSWINSZ`, `kill` ends
//! the child.

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};
use std::thread;

pub enum PtyEvent {
    Data(Vec<u8>),
    Exit,
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    /// Spawn `cmd args…` under a fresh pty sized `cols × rows`. Each
    /// chunk read from the master fires `on_event(PtyEvent::Data)`;
    /// EOF / read error fires `PtyEvent::Exit` once. The reader runs
    /// on a dedicated `std::thread` so the caller's runtime stays
    /// free.
    pub fn spawn<F>(
        cmd: &str,
        args: &[String],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        mut on_event: F,
    ) -> Result<Self, String>
    where
        F: FnMut(PtyEvent) + Send + 'static,
    {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty: {e}"))?;

        let mut builder = CommandBuilder::new(cmd);
        for a in args {
            builder.arg(a);
        }
        if let Some(cwd) = cwd {
            builder.cwd(cwd);
        }
        // The Shell tab renders through xterm.js, which speaks 256 colours —
        // but portable_pty inherits whatever TERM the host handed the app, and
        // on a GUI launch that is often plain `xterm` or unset, so tools like
        // ls/grep/bat fall back to 8 colours or none. Naming the terminal we
        // actually are is what makes their colour output correct.
        // (public issue #196)
        builder.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| format!("spawn: {e}"))?;
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("take_writer: {e}"))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone_reader: {e}"))?;

        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => on_event(PtyEvent::Data(buf[..n].to_vec())),
                    Err(_) => break,
                }
            }
            on_event(PtyEvent::Exit);
        });

        Ok(PtySession {
            master: pair.master,
            writer,
            child,
        })
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer
            .write_all(data)
            .map_err(|e| format!("write: {e}"))?;
        self.writer.flush().map_err(|e| format!("flush: {e}"))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("resize: {e}"))
    }

    pub fn kill(&mut self) -> Result<(), String> {
        self.child.kill().map_err(|e| format!("kill: {e}"))
    }
}

/// Pick the default shell: `$SHELL` when set + non-empty, otherwise a
/// reasonable per-OS fallback. The IPC layer uses this when the
/// frontend's `pty_open` payload doesn't specify a command.
pub fn default_shell() -> String {
    if let Ok(v) = std::env::var("SHELL") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if cfg!(windows) {
        // ConPTY-backed via portable-pty. PowerShell is preferred for
        // an interactive feel over cmd.exe.
        "powershell.exe".to_string()
    } else {
        "/bin/sh".to_string()
    }
}

// ── Global session manager ───────────────────────────────────────────
//
// The GUI hosts one PTY at a time (single Shell tab). Multi-tab
// support is a later step; for now a single slot keeps the IPC
// surface trivial.

use base64::Engine;
use std::sync::{Mutex, OnceLock};

fn slot() -> &'static Mutex<Option<PtySession>> {
    static SLOT: OnceLock<Mutex<Option<PtySession>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Open (or replace) the global PTY session. `dispatch` is the IPC
/// dispatch closure — each chunk of bytes from the child fires
/// `{"type":"pty_data","data": <base64>}`; child exit fires
/// `{"type":"pty_exit"}`.
pub fn open(
    cmd: &str,
    args: &[String],
    cwd: Option<&str>,
    cols: u16,
    rows: u16,
    dispatch: crate::ipc::DispatchFn,
) -> Result<(), String> {
    let mut guard = slot()
        .lock()
        .map_err(|e| format!("pty slot poisoned: {e}"))?;
    if let Some(mut prev) = guard.take() {
        let _ = prev.kill();
    }
    let dispatch_for_event = dispatch.clone();
    let session = PtySession::spawn(cmd, args, cwd, cols, rows, move |event| match event {
        PtyEvent::Data(bytes) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let payload = serde_json::json!({ "type": "pty_data", "data": b64 }).to_string();
            (dispatch_for_event)(payload);
        }
        PtyEvent::Exit => {
            let payload = serde_json::json!({ "type": "pty_exit" }).to_string();
            (dispatch_for_event)(payload);
        }
    })?;
    *guard = Some(session);
    Ok(())
}

/// Feed bytes into the running PTY's stdin (xterm keystrokes).
pub fn write(data: &[u8]) -> Result<(), String> {
    let mut guard = slot()
        .lock()
        .map_err(|e| format!("pty slot poisoned: {e}"))?;
    match guard.as_mut() {
        Some(s) => s.write(data),
        None => Err("pty not open".into()),
    }
}

/// Propagate a window-size change. No-op if no session is open.
pub fn resize(cols: u16, rows: u16) -> Result<(), String> {
    let guard = slot()
        .lock()
        .map_err(|e| format!("pty slot poisoned: {e}"))?;
    match guard.as_ref() {
        Some(s) => s.resize(cols, rows),
        None => Err("pty not open".into()),
    }
}

/// Kill the child and drop the session. Idempotent.
pub fn close() {
    if let Ok(mut guard) = slot().lock() {
        if let Some(mut s) = guard.take() {
            let _ = s.kill();
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    enum TestEvent {
        Data(Vec<u8>),
        Exit,
    }

    fn forward(tx: mpsc::Sender<TestEvent>) -> impl FnMut(PtyEvent) + Send + 'static {
        move |e| {
            let _ = tx.send(match e {
                PtyEvent::Data(d) => TestEvent::Data(d),
                PtyEvent::Exit => TestEvent::Exit,
            });
        }
    }

    fn collect_bytes_until<F: Fn(&[u8]) -> bool>(
        rx: &mpsc::Receiver<TestEvent>,
        timeout: Duration,
        done: F,
    ) -> Result<Vec<u8>, Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut buf = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Ok(TestEvent::Data(chunk)) => {
                    buf.extend(chunk);
                    if done(&buf) {
                        return Ok(buf);
                    }
                }
                Ok(TestEvent::Exit) => {
                    if done(&buf) {
                        return Ok(buf);
                    }
                    return Err(buf);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Err(buf)
    }

    #[test]
    fn spawn_echo_produces_output() {
        let (tx, rx) = mpsc::channel();
        let _session = PtySession::spawn(
            "/bin/echo",
            &["hello-pty".to_string()],
            None,
            80,
            24,
            forward(tx),
        )
        .expect("spawn echo");

        let result = collect_bytes_until(&rx, Duration::from_secs(3), |buf| {
            buf.windows(9).any(|w| w == b"hello-pty")
        });
        if let Err(buf) = result {
            panic!("echo output missing: {:?}", String::from_utf8_lossy(&buf));
        }
    }

    #[test]
    fn write_to_cat_echoes_back() {
        let (tx, rx) = mpsc::channel();
        let mut session =
            PtySession::spawn("/bin/cat", &[], None, 80, 24, forward(tx)).expect("spawn cat");

        session.write(b"ping-token\n").expect("write");

        let result = collect_bytes_until(&rx, Duration::from_secs(3), |buf| {
            String::from_utf8_lossy(buf).contains("ping-token")
        });
        let _ = session.kill();
        if let Err(buf) = result {
            panic!("cat did not echo: {:?}", String::from_utf8_lossy(&buf));
        }
    }

    #[test]
    fn resize_does_not_panic() {
        let mut session =
            PtySession::spawn("/bin/cat", &[], None, 80, 24, |_| {}).expect("spawn cat");
        session.resize(120, 40).expect("resize");
        session.resize(40, 12).expect("resize small");
        let _ = session.kill();
    }

    #[test]
    fn exit_event_fires_when_child_ends() {
        let (tx, rx) = mpsc::channel();
        let _session =
            PtySession::spawn("/bin/echo", &["bye".to_string()], None, 80, 24, forward(tx))
                .expect("spawn echo");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_exit = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(TestEvent::Exit) => {
                    saw_exit = true;
                    break;
                }
                Ok(TestEvent::Data(_)) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_exit, "expected PtyEvent::Exit after child terminated");
    }
}
