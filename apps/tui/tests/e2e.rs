//! PTY-based end-to-end tests.
//!
//! Spawns the real `kommand0-tui` binary in a pseudo-terminal, sends
//! keystrokes as raw bytes, and asserts against the rendered screen
//! (parsed with vt100). The embedded `claude` pane is driven by the
//! `embed-stub` fixture in `tests/fixtures/`, selected via the
//! `KOMMAND0_CLAUDE_BIN` env var (see the embedded-pane tests).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};

const COLS: u16 = 100;
const ROWS: u16 = 30;

struct Tui {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    // Kept alive for the duration of the test: state dir + fake-claude cwd
    _state_dir: tempfile::TempDir,
}

impl Tui {
    /// Launch the TUI in a fresh PTY with an isolated state dir.
    /// `state_json`: optional pre-seeded state.json content.
    fn launch(state_json: Option<String>) -> Self {
        Self::launch_with(state_json, &[])
    }

    /// Like [`Tui::launch`] but sets additional environment variables.
    fn launch_with(state_json: Option<String>, extra_env: &[(&str, &str)]) -> Self {
        let state_dir = tempfile::tempdir().unwrap();
        if let Some(json) = state_json {
            std::fs::write(state_dir.path().join("state.json"), json).unwrap();
        }

        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures");
        let path_env = format!(
            "{}:{}",
            fixtures.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_kommand0-tui"));
        cmd.env("KOMMAND0_STATE_DIR", state_dir.path());
        cmd.env("PATH", path_env);
        cmd.env("TERM", "xterm-256color");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.cwd(state_dir.path());

        let child = pair.slave.spawn_command(cmd).unwrap();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();

        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let parser_clone = parser.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                parser_clone.lock().unwrap().process(&buf[..n]);
            }
        });

        Self {
            parser,
            writer,
            child,
            killer,
            _state_dir: state_dir,
        }
    }

    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn send(&mut self, bytes: &str) {
        self.writer.write_all(bytes.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    /// Send Esc on its own, with a pause so the next byte isn't parsed as Alt+<key>.
    fn send_esc(&mut self) {
        self.send("\x1b");
        std::thread::sleep(Duration::from_millis(100));
    }

    /// Wait until the screen contains `needle`, panicking after 10s.
    fn wait_for(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.screen().contains(needle) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {needle:?}; screen:\n{}",
                    self.screen()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until the screen no longer contains `needle`.
    fn wait_gone(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !self.screen().contains(needle) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {needle:?} to disappear; screen:\n{}",
                    self.screen()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait for the process to exit, panicking after 10s.
    fn wait_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            if Instant::now() > deadline {
                let _ = self.killer.kill();
                panic!(
                    "timed out waiting for TUI to exit; screen:\n{}",
                    self.screen()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.killer.kill();
        }
    }
}

fn seeded_state(dir: &str) -> String {
    serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": dir }],
        "workspaces": [{
            "id": "w1",
            "name": "demo-ws",
            "repo_id": "r1",
            "working_dir": dir,
            "active": true,
            "created_at": 0
        }],
        "sessions": []
    })
    .to_string()
}

#[test]
fn launches_renders_panes_and_quits_on_q() {
    let mut tui = Tui::launch(None);
    tui.wait_for(" Repos ");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn help_overlay_opens_scrolls_and_closes() {
    let mut tui = Tui::launch(None);
    tui.wait_for(" Repos ");

    tui.send("?");
    tui.wait_for("Global");
    tui.wait_for("Current:");

    // G jumps to the bottom of the help content
    tui.send("G");
    tui.wait_for("to close");

    tui.send_esc(); // Esc closes
    tui.wait_gone("Current:");

    tui.send("q");
    tui.wait_exit();
}

#[test]
fn tree_navigation_expands_repo_with_l() {
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch(Some(state));

    tui.wait_for("demo");
    // Collapsed: workspace not visible
    assert!(!tui.screen().contains("demo-ws"));

    tui.send("l");
    tui.wait_for("demo-ws");

    tui.send("h");
    tui.wait_gone("demo-ws");

    tui.send("q");
    tui.wait_exit();
}

#[test]
fn embedded_pane_renders_real_terminal_and_forwards_keys() {
    // Phase 2: pressing 'e' embeds an interactive child (here a stub claude) in
    // the right pane; its terminal renders, and typed keys are forwarded to it.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l"); // expand repo
    tui.wait_for("demo-ws");
    tui.send("j"); // select the workspace
    tui.send("e"); // toggle embedded pane

    // The embedded child's own terminal output is composited into the pane.
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("Ctrl+A then"); // the pane border title

    // Keys go to the embedded child, which echoes them.
    tui.send("hi");
    tui.wait_for("hi");

    tui.send("\x1d"); // Ctrl+] leaves the embedded pane (back to the tree)
    tui.send("q"); // quit (kills the embedded child on teardown)
    tui.wait_exit();
}

#[test]
fn embedded_pane_not_stranded_by_mouse_click() {
    // A click inside the embedded pane must not flip focus out of it (which would
    // silently stop forwarding keys to the live child).
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e");
    tui.wait_for("EMBED-STUB-READY");

    // SGR left-click (press+release) well inside the right (embedded) pane.
    tui.send("\x1b[<0;60;12M");
    tui.send("\x1b[<0;60;12m");

    // If focus were stranded to Output, these keys would drive the scrollback
    // cursor and never reach the child; the stub only echoes what it receives.
    tui.send("MARKER");
    tui.wait_for("MARKER");

    tui.send("\x1d"); // Ctrl+] leaves
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn embedded_prefix_quits_and_returns_to_tree() {
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e");
    tui.wait_for("EMBED-STUB-READY");

    // Ctrl+A then 't' returns to the tree, where normal nav works again.
    tui.send("\x01"); // Ctrl+A (prefix)
    tui.send("t");
    // Back in the tree: 'q' quits (would otherwise be swallowed by the pane).
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn embedded_prefix_q_quits_directly() {
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e");
    tui.wait_for("EMBED-STUB-READY");

    tui.send("\x01"); // Ctrl+A (prefix)
    tui.send("q"); // quit directly from the embedded pane
    tui.wait_exit();
}

#[test]
fn enter_opens_embedded_claude_by_default() {
    // Phase 3: opening a workspace (Enter) launches the embedded claude — no more
    // old stream output+composer.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("\r"); // Enter opens the embedded claude (not the old stream view)
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("Ctrl+A then"); // embedded pane border

    tui.send("\x01"); // Ctrl+A
    tui.send("q"); // quit
    tui.wait_exit();
}
