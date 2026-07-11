//! PTY-based end-to-end tests.
//!
//! Spawns the real `kommand0` binary in a pseudo-terminal, sends
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

/// vt100 0.16 moved the audible-bell counter off `Screen` onto the `Callbacks`
/// trait; this counts BEL for `bell_count()`/`wait_for_bell()`.
#[derive(Default)]
struct BellCounter {
    count: usize,
}

impl vt100::Callbacks for BellCounter {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.count += 1;
    }
}

struct Tui {
    parser: Arc<Mutex<vt100::Parser<BellCounter>>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>, // kept for resize()
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
        Self::launch_impl(state_json, extra_env, false)
    }

    /// Like [`Tui::launch`] but WITHOUT `KOMMAND0_STATE_DIR`: the (debug)
    /// binary resolves its base dir to `<cwd>/.kommand0-dev`, and the optional
    /// state seeds a legacy pre-profiles layout at that root — for the
    /// migration test. `_state_dir` is then the cwd tempdir, not the state dir.
    #[cfg(debug_assertions)]
    fn launch_legacy_layout(state_json: Option<String>) -> Self {
        Self::launch_impl(state_json, &[], true)
    }

    fn launch_impl(
        state_json: Option<String>,
        extra_env: &[(&str, &str)],
        legacy_layout: bool,
    ) -> Self {
        let state_dir = tempfile::tempdir().unwrap();
        let seed_root = if legacy_layout {
            let base = state_dir.path().join(".kommand0-dev");
            std::fs::create_dir_all(&base).unwrap();
            base
        } else {
            state_dir.path().to_path_buf()
        };
        if let Some(json) = state_json {
            std::fs::write(seed_root.join("state.json"), json).unwrap();
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

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_kommand0"));
        // Don't let a developer's exported env leak into tests; each test sets
        // exactly what it needs via extra_env below.
        cmd.env_remove("KOMMAND0_CONFIG");
        cmd.env_remove("KOMMAND0_CLAUDE_BIN");
        cmd.env_remove("KOMMAND0_GH_BIN");
        if legacy_layout {
            // No exact-dir override: state resolves relative to the cwd set
            // below (debug base `.kommand0-dev`), where the layout is seeded.
            cmd.env_remove("KOMMAND0_STATE_DIR");
        } else {
            cmd.env("KOMMAND0_STATE_DIR", state_dir.path());
        }
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
        let master = pair.master; // keep the master alive so we can resize()
        drop(pair.slave); // the child owns its end; release ours

        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            ROWS,
            COLS,
            0,
            BellCounter::default(),
        )));
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
            master,
            _state_dir: state_dir,
        }
    }

    /// Resize the PTY (and the vt100 parser tracking its screen). The child gets
    /// a SIGWINCH and re-renders at the new geometry.
    fn resize(&self, rows: u16, cols: u16) {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
    }

    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// The text of screen row `y` (0-based).
    fn row(&self, y: u16) -> String {
        self.screen()
            .lines()
            .nth(y as usize)
            .map(str::to_string)
            .unwrap_or_default()
    }

    /// Wait until screen row `y` contains `needle`.
    fn wait_for_row(&self, y: u16, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.row(y).contains(needle) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {needle:?} in row {y}; screen:\n{}",
                    self.screen()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Read the persisted state.json (after the app has written it).
    fn read_state(&self) -> serde_json::Value {
        let raw = std::fs::read_to_string(self._state_dir.path().join("state.json"))
            .unwrap_or_else(|_| "{}".to_string());
        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null)
    }

    /// Contents of the app's log file (empty if not written).
    fn log_contents(&self) -> String {
        std::fs::read_to_string(self._state_dir.path().join("kommand0.log")).unwrap_or_default()
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

    /// Number of audible bells (BEL) the app has emitted, per vt100's counter.
    fn bell_count(&self) -> usize {
        self.parser.lock().unwrap().callbacks().count
    }

    /// Wait until the app has rung the terminal bell at least once.
    fn wait_for_bell(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.bell_count() > 0 {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for the terminal bell; screen:\n{}",
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
    // Bottom status bar is present.
    tui.wait_for("TREE");
    tui.wait_for("q quit");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn first_run_shows_welcome_and_add_repo_hint() {
    // Empty state (no repos): the tree leads with an in-TUI call to action
    // (press a), not a CLI command.
    let mut tui = Tui::launch(None);
    tui.wait_for("Welcome to kommand0");
    tui.wait_for("add a repo");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn survives_a_resize_drag_without_wedging_input() {
    // A real drag-resize fires a rapid stream of SIGWINCH (one per frame, tens of
    // events incl. transient degenerate sizes). We reproduce that cadence and
    // assert the app keeps re-rendering AND still responds to input afterwards —
    // a regression that wedges the input loop would time out at `wait_exit`.
    //
    // Input is read on a dedicated blocking thread (see `run` in main.rs).
    // crossterm's input layer can wedge only when SIGWINCH arrives in a
    // *microsecond*-tight burst (0ms apart — 9/10 of the time in a stress sweep);
    // any spacing >= 1ms is 100% clean. No real terminal emits SIGWINCH that
    // fast, so we drive the drag at a realistic (10ms) cadence.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch(Some(state));
    tui.wait_for("demo");
    // Drag a corner: shrink down to a degenerate size, then grow back.
    let sizes = [
        (28, 90),
        (22, 76),
        (16, 58),
        (10, 36),
        (5, 18),
        (3, 12), // degenerate
        (8, 28),
        (15, 52),
        (24, 84),
        (32, 104),
        (40, 120),
    ];
    for (rows, cols) in sizes {
        tui.resize(rows, cols);
        std::thread::sleep(Duration::from_millis(10));
    }
    // Re-rendered at the final size (a panic on any resize would have killed the
    // process and timed this out instead).
    tui.wait_for("demo");
    // Input still flows after the drag — the wedge symptom was `q` being ignored.
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn writes_a_log_file_on_startup() {
    // Background warnings/errors can't go to stderr (alt-screen); they're logged
    // to <state_dir>/kommand0.log. The startup line proves the sink is wired.
    let mut tui = Tui::launch(None);
    tui.wait_for(" Repos ");
    tui.send("q");
    tui.wait_exit();
    assert!(
        tui.log_contents().contains("kommand0 started"),
        "log: {:?}",
        tui.log_contents()
    );
}

#[test]
fn help_overlay_opens_scrolls_and_closes() {
    let mut tui = Tui::launch(None);
    tui.wait_for(" Repos ");

    tui.send("?");
    tui.wait_for("Tree Pane");
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
fn review_diff_overlay_opens_and_closes() {
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("v"); // review the selected workspace's diff
    // The two-pane dialog opens with a titled border and (for this
    // worktree-less fallback workspace) the "no branch to review" note — which
    // proves the key opened it and open_diff ran and picked the right state.
    tui.wait_for("Review —");
    tui.wait_for("branch to review");

    tui.send_esc(); // Esc closes
    tui.wait_gone("Review —");

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
    tui.wait_for("Ctrl+A:"); // the pane border title

    // Keys go to the embedded child, which echoes them.
    tui.send("hi");
    tui.wait_for("hi");

    tui.send("\x1d"); // Ctrl+] leaves the embedded pane (back to the tree)
    tui.send("q"); // quit (kills the embedded child on teardown)
    tui.wait_exit();
}

#[test]
fn palette_jumps_to_and_opens_a_collapsed_repo_workspace() {
    // `:` opens the command palette; typing fuzzy-matches a workspace hidden
    // under a collapsed repo, and Enter jumps to + opens it.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    assert!(!tui.screen().contains("demo-ws"), "repo starts collapsed");

    tui.send(":");
    tui.wait_for("Command palette");
    tui.send("demo-ws");
    tui.send("\r"); // Enter jumps + opens

    tui.wait_for("EMBED-STUB-READY"); // the previously-hidden workspace opened
    tui.send("\x1d"); // Ctrl+] back to the tree
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn palette_runs_an_action_new_session() {
    // The palette runs actions, not just jumps: "new session" narrows to the
    // New session action and Enter opens a session for the workspace.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send(":");
    tui.wait_for("Command palette");
    tui.send("new session"); // only the New session action is a subsequence match
    tui.send("\r");

    tui.wait_for("EMBED-STUB-READY"); // the action opened a session
    tui.send("\x1d"); // Ctrl+] back to the tree
    tui.send("q");
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

    // A click must not knock focus off the embedded pane: typed keys still reach
    // the child (the stub only echoes what it actually receives).
    tui.send("MARKER");
    tui.wait_for("MARKER");

    tui.send("\x1d"); // Ctrl+] leaves
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn mouse_click_is_forwarded_to_embedded_pane() {
    // When claude (the stub) has enabled mouse reporting, a click inside the pane
    // is re-encoded into the pane's coordinate space and forwarded to the child.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e");
    tui.wait_for("EMBED-STUB-READY");

    // SGR left-click at absolute 60;12. With the 100x30 / 30-70 split the right
    // pane's inner origin is (31,1); the active pane content starts below the
    // 1-row tab strip at (31,2), so the forwarded report is pane-relative:
    // 0-based (59,11) -> content (28,9) -> 1-based wire 29;10.
    tui.send("\x1b[<0;60;12M");
    tui.wait_for("[<0;29;10"); // exact translated coords reached the stub

    tui.send("\x1d"); // Ctrl+] leaves
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn clicking_panes_switches_focus() {
    // Click-to-focus: from the embedded pane a click in the (mostly empty) tree
    // pane returns focus to the tree, and a click back in the content pane
    // returns focus to claude. The focus-dependent footer hint is the signal.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e");
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("Ctrl+A t tree"); // embedded footer → focus is on claude

    // Left-click (press+release) in empty tree space, well below the few rows.
    tui.send("\x1b[<0;5;20M");
    tui.send("\x1b[<0;5;20m");
    tui.wait_for("Enter open"); // tree footer → the click moved focus to the tree

    // Click back inside the content pane → focus returns to the embedded session.
    tui.send("\x1b[<0;60;12M");
    tui.send("\x1b[<0;60;12m");
    tui.wait_for("Ctrl+A t tree"); // embedded footer again → focus back on claude

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
fn ctrl_a_s_opens_a_shell_tab() {
    // The "shell" is the embed-stub too (via KOMMAND0_SHELL), so the new tab is
    // deterministic and needs no real $SHELL.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(
        Some(state),
        &[("KOMMAND0_CLAUDE_BIN", "embed-stub"), ("KOMMAND0_SHELL", "embed-stub")],
    );

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e");
    tui.wait_for("EMBED-STUB-READY"); // Claude tab (1)

    // Ctrl+A s opens a shell tab; the tab strip marks it with `$`.
    tui.send("\x01");
    tui.send("s");
    tui.wait_for("2$"); // a second, shell-marked tab

    tui.send("\x01");
    tui.send("t");
    tui.send("q");
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
    tui.wait_for("Ctrl+A:"); // embedded pane border
    // The session tab strip (top inner row) shows the first tab and the [+].
    tui.wait_for_row(1, " 1 ");
    tui.wait_for_row(1, "+");

    tui.send("\x01"); // Ctrl+A
    tui.send("q"); // quit
    tui.wait_exit();
}

#[test]
fn reopen_resumes_all_persisted_sessions_as_tabs() {
    // A workspace with two stored session ids reopens as two tabs, each resumed
    // in order, with the first tab active.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let state = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": d }],
        "workspaces": [{
            "id": "w1", "name": "demo-ws", "repo_id": "r1",
            "working_dir": d, "active": true, "created_at": 0
        }],
        "sessions": [],
        "embedded_sessions": {
            "w1": [
                "11111111-1111-1111-1111-111111111111",
                "22222222-2222-2222-2222-222222222222"
            ]
        }
    })
    .to_string();
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e");
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("2 live"); // both stored sessions resumed as tabs
    tui.wait_for("--resume"); // resumed, not freshly created
    tui.wait_for("11111111"); // the first tab is active and shows the first id

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn failed_resume_shows_error_and_forgets_the_id() {
    // A stored session whose binary can't start: the error is shown and the id is
    // forgotten so it doesn't desync / keep failing.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let state = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": d }],
        "workspaces": [{
            "id": "w1", "name": "demo-ws", "repo_id": "r1",
            "working_dir": d, "active": true, "created_at": 0
        }],
        "sessions": [],
        "embedded_sessions": { "w1": ["dead-session-id"] }
    })
    .to_string();
    let mut tui = Tui::launch_with(
        Some(state),
        &[("KOMMAND0_CLAUDE_BIN", "/nonexistent/kommand0-claude")],
    );

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open -> the only persisted session fails to spawn
    tui.wait_for("Failed to start claude");

    tui.send("q");
    tui.wait_exit();
    // The unspawnable id was forgotten, keeping persistence coherent.
    let st = tui.read_state();
    assert!(
        st["embedded_sessions"]
            .get("w1")
            .map(|v| v.as_array().map(|a| a.is_empty()).unwrap_or(true))
            .unwrap_or(true),
        "the failed resume id should be forgotten: {st}"
    );
}

#[test]
fn closing_the_last_tab_returns_to_tree_and_reopens_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open -> tab 1 (a fresh --session-id)
    tui.wait_for("1 live");

    // Ctrl+A x on the only tab returns to the tree.
    tui.send("\x01");
    tui.send("x");
    tui.wait_for("no live sessions");
    tui.wait_for("Press Enter to open Claude"); // detail view, back on the tree

    // Reopening starts a fresh session (the closed id was forgotten).
    tui.send("\r");
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("--session-id");

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn stale_resume_auto_heals_to_a_fresh_session() {
    // The real bug: `claude --resume <gone-id>` prints "No conversation found"
    // and STAYS ALIVE, so the exit-code net never fires and the dead id is never
    // cleared (reopen keeps re-resuming the gone session). kommand0 detects the
    // miss from the pane output, then AUTO-HEALS: it forgets the gone id and
    // replaces the stuck tab in place with a fresh session (no manual reopen).
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let state = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": d }],
        "workspaces": [{
            "id": "w1", "name": "demo-ws", "repo_id": "r1",
            "working_dir": d, "active": true, "created_at": 0
        }],
        "sessions": [],
        "embedded_sessions": { "w1": ["gone-session-id"] }
    })
    .to_string();
    let mut tui = Tui::launch_with(
        Some(state),
        &[("KOMMAND0_CLAUDE_BIN", "embed-stub-resume-miss")],
    );

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open -> resume the gone session (stub stays alive on the error)
    tui.wait_for("started a fresh one"); // detected, healed in place with a new session
    tui.wait_for("1 live"); // still embedded — the slot now holds the fresh session

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();
    // The gone id was forgotten and replaced by exactly one fresh id, so a future
    // open resumes the healthy session instead of re-resuming the gone one.
    let st = tui.read_state();
    let ids = st["embedded_sessions"]
        .get("w1")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(ids.len(), 1, "exactly one fresh session persisted: {st}");
    assert_ne!(
        ids[0].as_str(),
        Some("gone-session-id"),
        "the stale resume id should be replaced, not re-persisted: {st}"
    );
}

#[test]
fn ctrl_a_c_adds_a_session_tab_and_x_closes_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open -> tab 1
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("1 live"); // status bar counts session tabs

    // Ctrl+A then c opens a second session tab.
    tui.send("\x01");
    tui.send("c");
    tui.wait_for("2 live");
    tui.wait_for_row(1, " 2 "); // second tab shows in the strip

    // Ctrl+A then x closes the active tab, back to one.
    tui.send("\x01");
    tui.send("x");
    tui.wait_for("1 live");

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn ctrl_a_r_renames_a_tab_and_persists_the_title() {
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open -> tab 1
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("1 live");

    // Ctrl+A r opens the rename modal; typing goes to the modal, not claude.
    tui.send("\x01");
    tui.send("r");
    tui.wait_for("Rename Session");
    tui.send("auth");
    tui.send("\r");
    tui.wait_for_row(1, "auth"); // title shows in the tab strip once the modal closes

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();

    // The title persisted, keyed by the tab's session id.
    let st = tui.read_state();
    let titled = st["embedded_titles"]["w1"]
        .as_object()
        .map(|m| m.values().any(|v| v == "auth"))
        .unwrap_or(false);
    assert!(titled, "a session tab is persisted as titled 'auth': {st}");
}

fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} in {cwd:?} failed");
}

#[test]
fn c_cleans_up_a_merged_workspace() {
    // A real repo + worktree on a LEGACY `kommand0/`-prefixed branch (regression
    // guard: workspaces created before the prefix was dropped must keep cleaning
    // up); `gh` (stubbed) reports the PR merged, so confirming cleanup removes
    // the worktree + branch and drops the workspace from the tree.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repo = root.join("repo");
    let wt = root.join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "init"]);
    run_git(&repo, &["worktree", "add", wt.to_str().unwrap(), "-b", "kommand0/demo-ws"]);

    let state = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": repo.to_str().unwrap() }],
        "workspaces": [{
            "id": "w1", "name": "demo-ws", "repo_id": "r1",
            "working_dir": wt.to_str().unwrap(), "active": true, "created_at": 0,
            "worktree_path": wt.to_str().unwrap(), "branch_name": "kommand0/demo-ws"
        }],
        "sessions": []
    })
    .to_string();

    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_GH_BIN", "gh-stub-merged")]);
    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j"); // select the workspace
    tui.send("c"); // cleanup -> confirmation modal
    tui.wait_for("Clean Up Workspace");
    tui.send("y"); // confirm
    tui.wait_gone("demo-ws"); // the workspace is removed from the tree

    assert!(!wt.exists(), "worktree directory removed");

    tui.send("q");
    tui.wait_exit();
}

#[test]
fn slash_filters_the_tree_and_capital_a_archives() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let state = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": d }],
        "workspaces": [
            { "id": "w1", "name": "auth-ws", "repo_id": "r1", "working_dir": d, "active": true, "created_at": 0 },
            { "id": "w2", "name": "docs-ws", "repo_id": "r1", "working_dir": d, "active": true, "created_at": 0 }
        ],
        "sessions": []
    })
    .to_string();
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l"); // expand the repo
    tui.wait_for("auth-ws");
    tui.wait_for("docs-ws");

    // Filter to "auth" — only the matching workspace remains.
    tui.send("/");
    tui.send("auth");
    tui.wait_for("auth-ws");
    tui.wait_gone("docs-ws");

    // Esc clears the filter — both are back.
    tui.send_esc();
    tui.wait_for("docs-ws");

    // Select the first workspace and archive it with `A`.
    tui.send("j");
    tui.send("A");

    tui.send("q");
    tui.wait_exit();
    // The archive persisted.
    let st = tui.read_state();
    let archived = st["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["active"] == false);
    assert!(archived, "a workspace was archived: {st}");
}

#[test]
fn unseen_background_output_raises_the_attention_flag() {
    // Open a workspace (its session is "viewed"), navigate back to the tree, and
    // let the session emit output while you're away. Once it goes quiet, the
    // status bar must surface that the workspace needs you.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(
        Some(state),
        &[("KOMMAND0_CLAUDE_BIN", "embed-stub-attention")],
    );

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open -> the tab is viewed; its initial marker is seen
    tui.wait_for("READY-DELAYED");

    // Back to the tree before the late output lands.
    tui.send("\x01");
    tui.send("t");
    tui.wait_for("TREE");

    // The late output arrives unseen, then the pane goes quiet -> attention.
    tui.wait_for("waiting");
    // No `notify` config => default Off: the same edge must NOT ring the bell.
    assert_eq!(tui.bell_count(), 0, "notify defaults to off — no bell");

    tui.send("q");
    tui.wait_exit();
}

#[test]
fn notify_bell_rings_when_a_background_session_needs_you() {
    // With notify="bell", the rising edge of the attention flag must ring the
    // terminal bell. The app writes BEL to its stdout; the harness's vt100
    // parser counts it. (Same navigation as the attention test above.)
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let cfg = dir.path().join("config.json");
    std::fs::write(&cfg, r#"{ "notify": "bell" }"#).unwrap();
    let mut tui = Tui::launch_with(
        Some(state),
        &[
            ("KOMMAND0_CLAUDE_BIN", "embed-stub-attention"),
            ("KOMMAND0_CONFIG", cfg.to_str().unwrap()),
        ],
    );

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open -> the tab is viewed; its initial marker is seen
    tui.wait_for("READY-DELAYED");

    // Back to the tree before the late output lands (so it arrives unseen).
    tui.send("\x01");
    tui.send("t");
    tui.wait_for("TREE");

    // Unseen background output -> quiet -> attention rising edge -> bell.
    tui.wait_for("waiting");
    tui.wait_for_bell();

    tui.send("q");
    tui.wait_exit();
}

#[test]
fn config_rebinds_quit() {
    // Rebind quit to `Q`; the default `q` becomes a no-op, `Q` quits.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.json");
    std::fs::write(&cfg, r#"{ "keybindings": { "quit": ["Q"] } }"#).unwrap();
    let mut tui = Tui::launch_with(None, &[("KOMMAND0_CONFIG", cfg.to_str().unwrap())]);

    tui.wait_for(" Repos ");
    // The old `q` no longer quits — prove the app is still alive by opening the
    // filter afterwards (a dead process couldn't render it).
    tui.send("q");
    tui.send("/");
    tui.wait_for("Repos · /");
    tui.send_esc();

    // The rebound key quits.
    tui.send("Q");
    tui.wait_exit();
}

#[test]
fn config_claude_args_pass_through_to_the_pane() {
    // A config file's `claude_args` are appended to every embedded spawn — here
    // the embed-stub echoes its args, so we can see `--model sonnet` arrive.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let cfg = dir.path().join("config.json");
    // Drive BOTH the binary and the extra args from config (no KOMMAND0_CLAUDE_BIN
    // env), so this exercises config.claude_bin too.
    std::fs::write(
        &cfg,
        r#"{ "claude_bin": "embed-stub", "claude_args": ["--model", "sonnet"] }"#,
    )
    .unwrap();
    let state = seeded_state(d);
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CONFIG", cfg.to_str().unwrap())]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("e"); // open the embedded pane
    tui.wait_for("--model"); // the stub echoed its spawn args (incl. the config arg)

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn first_open_assigns_a_session_id() {
    // Opening a workspace with no stored session spawns `claude --session-id <uuid>`.
    let dir = tempfile::tempdir().unwrap();
    let state = seeded_state(dir.path().to_str().unwrap());
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("\r");
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("--session-id"); // a fresh id was assigned for this session

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn reopen_resumes_stored_session() {
    // A workspace with a stored session id is reopened with `claude --resume <id>`.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let state = serde_json::json!({
        "repos": [{ "id": "r1", "name": "demo", "path": d }],
        "workspaces": [{
            "id": "w1", "name": "demo-ws", "repo_id": "r1",
            "working_dir": d, "active": true, "created_at": 0
        }],
        "sessions": [],
        "embedded_sessions": { "w1": "11111111-1111-1111-1111-111111111111" }
    })
    .to_string();
    let mut tui = Tui::launch_with(Some(state), &[("KOMMAND0_CLAUDE_BIN", "embed-stub")]);

    tui.wait_for("demo");
    tui.send("l");
    tui.wait_for("demo-ws");
    tui.send("j");
    tui.send("\r");
    tui.wait_for("EMBED-STUB-READY");
    tui.wait_for("--resume"); // resumed, not freshly created
    tui.wait_for("11111111"); // the stored session id

    tui.send("\x01");
    tui.send("q");
    tui.wait_exit();
}

#[test]
fn profile_flag_with_state_dir_env_fails_before_the_alt_screen() {
    // The env conflict exits before ratatui::init, so a plain (non-PTY) spawn
    // captures the message on stderr and the terminal is never touched.
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kommand0"))
        .args(["--profile", "x"])
        .env("KOMMAND0_STATE_DIR", dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "env + --profile must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot be combined"), "conflict message on stderr: {err}");
}

#[cfg(debug_assertions)]
#[test]
fn legacy_state_migrates_before_logging_and_renders_the_tree() {
    // Pins the load-bearing main() ordering: migrate_legacy_profiles() must run
    // BEFORE init_logging() — init_logging create_dir_all's the state dir, which
    // would create profiles/… first, trip the migration guard, and silently
    // orphan the legacy state. If that ordering ever regresses, the tree renders
    // empty instead of the seeded repo and this times out. Debug-gated: a
    // release binary ignores the cwd and would migrate the REAL data dir.
    let repo_dir = tempfile::tempdir().unwrap();
    let state = seeded_state(repo_dir.path().to_str().unwrap());
    let mut tui = Tui::launch_legacy_layout(Some(state));

    tui.wait_for("demo"); // the migrated (not orphaned) state feeds the tree
    let base = tui._state_dir.path().join(".kommand0-dev");
    assert!(
        base.join("profiles").join("default").join("state.json").exists(),
        "legacy state.json moved under profiles/default/"
    );
    assert!(!base.join("state.json").exists(), "legacy root left clean");

    tui.send("q");
    tui.wait_exit();
}
