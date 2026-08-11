//! An embedded terminal pane: a child process running in a pseudo-terminal,
//! emulated with vt100 and composited into a ratatui buffer.
//!
//! This is the PTY-passthrough core (see MIGRATION.md): a [`Pane`] owns a PTY,
//! spawns a command in it (e.g. interactive `claude`), pumps its output through a
//! vt100 parser on a background thread, forwards keystrokes back, and blits the
//! emulated screen. It is the app's only session view; the reader thread signals
//! repaints via the wake callback passed to [`Pane::spawn_with_wake`].

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use kommand0_core::PROFILE_ENV;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// Lines of history kept per pane for the local scrollback view (wheel
/// scrolling over a child that never enabled mouse reporting, e.g. a shell
/// running a dev server). Bounds memory: cells are 32 bytes, so a full
/// 200-col history caps near 13 MB per pane, typical widths well under that.
const SCROLLBACK_LINES: usize = 2000;

/// Rows moved per wheel tick, matching common terminal defaults.
const WHEEL_STEP: usize = 3;

/// A child process running in a pseudo-terminal, with its screen emulated.
///
/// Termination note: portable-pty's cloned `ChildKiller` only delivers SIGHUP
/// to the child pid (no escalation), so a child that ignores SIGHUP (e.g. a
/// Node-based `claude`) would survive, and the child's *descendants* (a shell's
/// foreground dev server, a server claude spawned) live in other process groups
/// the kernel won't clean up either. The pane therefore guarantees teardown
/// itself: it captures the child's process group and the PTY's foreground group
/// while the child is alive, sends SIGHUP, waits a brief grace poll, then
/// SIGKILLs the child pid AND those groups (see [`Pane::terminate`]). SIGKILL
/// closes the PTY slave, which gives the reader thread EOF so the detached
/// thread ends. Teardown capture may first SIGTERM a capture-kind child
/// ([`Pane::signal_term`]) and drain the reader for its exit hint; the
/// SIGHUP-then-SIGKILL guarantee still runs after.
pub struct Pane {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    reader: Option<JoinHandle<()>>,
    /// Bumped on every chunk of child output; lets callers cheaply detect that a
    /// pane produced output since they last looked (poll-based activity signal).
    output_seq: Arc<AtomicU64>,
    /// Whether the child opted into focus reporting (`CSI ?1004h`). Set by the
    /// reader thread's byte scan (vt100 0.16 doesn't track this mode), read on
    /// the UI thread by [`Pane::sync_focus`].
    focus_reporting: Arc<AtomicBool>,
    /// The focus state last SENT to the child (`CSI I`/`CSI O`) — edge trigger
    /// for [`Pane::sync_focus`]. `None` until the first report after opt-in.
    focus_sent: Option<bool>,
    /// Instant of the last bytes written to the child. Every write is direct
    /// user interaction (keys, scroll, paste, focus reports), so output arriving
    /// right after is the child redrawing in response; the activity tracker
    /// uses this to not count it as work.
    last_input: Option<Instant>,
    /// Process groups captured at teardown start (child's own group + the PTY's
    /// foreground group); SIGKILLed when teardown escalates so a dev server
    /// running in the pane can't outlive it. Empty until a teardown begins.
    kill_groups: Vec<i32>,
    /// Test seam: while `true`, [`Pane::reader_finished`] reports `false`.
    /// The "child exited but the reader hasn't drained" window cannot be
    /// held open with real processes on macOS: the kernel revokes the pty
    /// when the session-leader child exits, so a grandchild's open slave fd
    /// neither blocks EOF nor reliably delivers buffered output. Tests that
    /// pin the reap's drain-defer flip this instead; everything else about
    /// the pane stays real.
    #[cfg(test)]
    pub(crate) force_reader_unfinished: bool,
    rows: u16,
    cols: u16,
}

/// Detect the focus-reporting DECSET (`CSI ? 1004 h` / `l`) in a child's raw
/// output stream. `tail` carries the last few bytes of the previous chunk so
/// a sequence straddling a read boundary still matches. Returns the LAST
/// occurrence's state in this chunk, `None` when the chunk doesn't touch the
/// mode.
fn scan_focus_reporting(tail: &mut Vec<u8>, chunk: &[u8]) -> Option<bool> {
    const ON: &[u8] = b"\x1b[?1004h";
    const OFF: &[u8] = b"\x1b[?1004l";
    let mut data = std::mem::take(tail);
    data.extend_from_slice(chunk);
    let mut latest = None;
    for w in data.windows(ON.len()) {
        if w == ON {
            latest = Some(true);
        } else if w == OFF {
            latest = Some(false);
        }
    }
    let keep = data.len().min(ON.len() - 1);
    *tail = data.split_off(data.len() - keep);
    latest
}

impl Pane {
    /// Spawn `program` (with `args`) in a fresh PTY of `rows`×`cols`, running in
    /// `cwd`, and start pumping its output into a vt100 emulator.
    pub fn spawn(program: &str, args: &[&str], cwd: &Path, rows: u16, cols: u16) -> Result<Pane> {
        Self::spawn_with_wake(program, args, cwd, rows, cols, None, None)
    }

    /// Like [`Pane::spawn`], but `wake` is invoked (off the UI thread) after each
    /// chunk of child output, so an event loop can schedule a coalesced repaint
    /// instead of polling — keystroke echo stays responsive. A `profile` is
    /// exported to the child as `KOMMAND0_PROFILE` (see the spawn body).
    pub fn spawn_with_wake(
        program: &str,
        args: &[&str],
        cwd: &Path,
        rows: u16,
        cols: u16,
        wake: Option<Box<dyn Fn() + Send>>,
        profile: Option<&str>,
    ) -> Result<Pane> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("openpty failed")?;

        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        // Don't let an embedded CLI think it's nested inside its parent (a claude
        // launched from kommand0-run-under-claude must start a real session).
        cmd.env_remove("CLAUDECODE");
        cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
        // Hand a non-default profile down so a nested `kmd`/`kommand0` inside
        // this session targets the same profile. `None` must REMOVE any
        // inherited value (the CLAUDECODE pattern above): a default-profile
        // TUI launched inside a profiled session — or under a shell-exported
        // KOMMAND0_PROFILE — must not leak the ancestor's profile to its
        // children. An env-mode parent still isolates children via the
        // inherited KOMMAND0_STATE_DIR either way.
        if let Some(p) = profile {
            cmd.env(PROFILE_ENV, p);
        } else {
            cmd.env_remove(PROFILE_ENV);
        }

        let child = pair.slave.spawn_command(cmd).context("spawn in pty failed")?;
        drop(pair.slave); // close our handle to the slave so EOF propagates on exit
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("clone pty reader failed")?;
        let writer = pair.master.take_writer().context("take pty writer failed")?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK_LINES)));
        let output_seq = Arc::new(AtomicU64::new(0));
        let focus_reporting = Arc::new(AtomicBool::new(false));
        let parser_t = parser.clone();
        let seq_t = output_seq.clone();
        let focus_t = focus_reporting.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut focus_tail: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF: child exited / pty closed
                    Ok(n) => {
                        if let Ok(mut p) = parser_t.lock() {
                            // A scrolled-back view stays anchored on its own:
                            // vt100 moves the offset with each row that enters
                            // history (clamped at capacity), so plain process()
                            // is correct even while the user is reading back.
                            p.process(&buf[..n]);
                        }
                        if let Some(on) = scan_focus_reporting(&mut focus_tail, &buf[..n]) {
                            focus_t.store(on, Ordering::Relaxed);
                        }
                        seq_t.fetch_add(1, Ordering::Relaxed);
                        if let Some(w) = &wake {
                            w();
                        }
                    }
                    // A signal (e.g. SIGWINCH on resize) can interrupt the read;
                    // resume rather than treating it as EOF.
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break, // unrecoverable read error
                }
            }
        });

        Ok(Pane {
            parser,
            writer,
            child,
            killer,
            _master: pair.master,
            reader: Some(handle),
            output_seq,
            focus_reporting,
            focus_sent: None,
            last_input: None,
            kill_groups: Vec::new(),
            #[cfg(test)]
            force_reader_unfinished: false,
            rows,
            cols,
        })
    }

    /// Resize the PTY and emulator to a new size (no-op if unchanged). Sends the
    /// child a SIGWINCH so a TUI child re-renders to fit.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if (rows, cols) == (self.rows, self.cols) {
            return Ok(());
        }
        self._master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("pty resize failed")?;
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
        self.rows = rows;
        self.cols = cols;
        Ok(())
    }

    /// Write user input to the child's stdin. Input while scrolled back snaps
    /// the view to the live screen first, like a real terminal (a no-op at
    /// offset 0, which covers mouse-mode and alt-screen children).
    pub fn send(&mut self, bytes: &[u8]) -> Result<()> {
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(0);
        }
        self.send_raw(bytes)
    }

    /// Write bytes to the child's stdin without touching the view: synthesized
    /// reports (focus in/out) must not yank a scrolled-back reader to the
    /// bottom the way real user input does.
    fn send_raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.last_input = Some(Instant::now());
        self.writer.write_all(bytes).context("pty write failed")?;
        self.writer.flush().context("pty flush failed")?;
        Ok(())
    }

    /// Instant of the last user input written to the child, if any. Output that
    /// follows within a short grace of this is interaction (a redraw), not work.
    pub fn last_input_at(&self) -> Option<Instant> {
        self.last_input
    }

    /// Send the synthesized focus state (`CSI I` / `CSI O`) to the child —
    /// edge-triggered, so callers can invoke this every frame. A no-op until
    /// the child opts into focus reporting (`CSI ?1004h`); the first report
    /// after opt-in carries the CURRENT state, like a real terminal enabling
    /// mode 1004 (and opting back out resets that memory, so a later re-opt-in
    /// gets a fresh report).
    pub fn sync_focus(&mut self, focused: bool) {
        if !self.focus_reporting.load(Ordering::Relaxed) {
            self.focus_sent = None;
            return;
        }
        if self.focus_sent != Some(focused) {
            let _ = self.send_raw(if focused { b"\x1b[I" } else { b"\x1b[O" });
            self.focus_sent = Some(focused);
        }
    }

    /// Test hook: flip the opt-in flag as the reader thread's scan would.
    #[cfg(test)]
    pub(crate) fn set_focus_reporting(&self, on: bool) {
        self.focus_reporting.store(on, Ordering::Relaxed);
    }

    /// Test hook: the focus state last sent to the child.
    #[cfg(test)]
    pub(crate) fn focus_sent(&self) -> Option<bool> {
        self.focus_sent
    }

    /// Encode a key event and forward it to the child. Returns `Ok(false)` if the
    /// key has no byte encoding (nothing sent).
    pub fn send_key(&mut self, key: KeyEvent) -> Result<bool> {
        match encode_key(key) {
            Some(bytes) => {
                self.send(&bytes)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Forward a mouse event to the child if it has enabled mouse reporting
    /// (and a mode that wants this event). `col`/`row` are 0-based cells within
    /// the child's screen. A wheel tick over a child with mouse mode off (a
    /// plain shell) scrolls the pane's local view instead, see
    /// [`Pane::scroll_view`]. Returns whether the event was consumed.
    pub fn send_mouse(
        &mut self,
        kind: MouseEventKind,
        mods: KeyModifiers,
        col: u16,
        row: u16,
    ) -> bool {
        let (mode, encoding) = {
            let Ok(parser) = self.parser.lock() else {
                return false;
            };
            let screen = parser.screen();
            (
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding(),
            )
        };
        match encode_mouse(mode, encoding, kind, mods, col, row) {
            Some(bytes) => {
                let _ = self.send(&bytes);
                true
            }
            // Only a child with mouse mode OFF gets the local-scroll fallback:
            // encode_mouse also declines for a mouse-mode child it can't
            // encode for (utf8 encoding, X10 coords past 222), and stealing
            // that child's wheel would scroll its view out from under it.
            None if mode == MouseProtocolMode::None => self.scroll_view(kind),
            None => false,
        }
    }

    /// Local scrolling for a child that never enabled mouse reporting: wheel
    /// ticks move the view through the emulator's scrollback instead of being
    /// dropped (the shell-running-a-dev-server case). In the alternate screen
    /// there is no scrollback, so the wheel converts to cursor keys instead
    /// (the alternateScroll convention, honoring DECCKM), and pagers in a
    /// shell tab still scroll. Callers gate on mouse mode being off.
    fn scroll_view(&mut self, kind: MouseEventKind) -> bool {
        let up = match kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return false,
        };
        // Decide and (for the normal screen) apply under one lock, so a mode
        // flip between the check and the action can't misroute the tick. The
        // alt-screen write happens after the guard drops: send() re-locks.
        let alt_app_cursor = {
            let Ok(mut p) = self.parser.lock() else {
                return false;
            };
            let screen = p.screen_mut();
            if screen.alternate_screen() {
                Some(screen.application_cursor())
            } else {
                let next = if up {
                    screen.scrollback() + WHEEL_STEP
                } else {
                    screen.scrollback().saturating_sub(WHEEL_STEP)
                };
                screen.set_scrollback(next); // clamped to the stored history
                None
            }
        };
        if let Some(app_cursor) = alt_app_cursor {
            let arrow: &[u8] = match (app_cursor, up) {
                (false, true) => b"\x1b[A",
                (false, false) => b"\x1b[B",
                (true, true) => b"\x1bOA", // DECCKM: SS3 cursor keys
                (true, false) => b"\x1bOB",
            };
            let _ = self.send(&arrow.repeat(WHEEL_STEP));
        }
        true
    }

    /// Whether the child has enabled ANY mouse reporting mode. Drives the
    /// selection arbitration (tmux rule): a mouse-mode child keeps receiving
    /// every mouse event exactly as today; only a mouse-less child's pane is
    /// drag-selectable.
    pub fn wants_mouse(&self) -> bool {
        self.parser
            .lock()
            .map(|p| p.screen().mouse_protocol_mode() != MouseProtocolMode::None)
            .unwrap_or(false)
    }

    /// Monotonic counter of output chunks received; compare against a previous
    /// value to tell whether the child produced output in the interim.
    pub fn output_seq(&self) -> u64 {
        self.output_seq.load(Ordering::Relaxed)
    }

    /// Whether a foreground command is running in this PTY: the terminal's
    /// foreground process group differs from the child's own group. For a shell
    /// this means a command is executing (build, server, `sleep`) — a *busy*
    /// signal that holds even when the command produces no output, which the
    /// output-based [`Self::output_seq`] activity check would miss.
    ///
    /// Returns `None` when it can't be determined (no child pid, or the platform
    /// reports no foreground group), so callers can fall back to the
    /// output-based signal rather than treat "unknown" as "idle".
    pub fn foreground_busy(&self) -> Option<bool> {
        let child_pid = self.child.process_id()? as i32;
        // The terminal's current foreground process group.
        let fg = self._master.process_group_leader()?;
        // The child's own group via `getpgid`. If it can't be read (e.g. the
        // child just exited), return `None` so the caller keeps the output-based
        // signal rather than guessing a stale group as "busy".
        let child_pgid = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(child_pid)))
            .ok()?
            .as_raw();
        Some(fg != child_pgid)
    }

    /// Current emulated size (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// Plain-text snapshot of the current VIEW (for tests/logging): follows
    /// the scrollback offset, i.e. it shows whatever the user has wheeled to.
    /// Machine scans that need the real bottom use [`Pane::live_contents`].
    pub fn screen_contents(&self) -> String {
        self.parser
            .lock()
            .map(|p| p.screen().contents())
            .unwrap_or_default()
    }

    /// Plain-text snapshot of the LIVE screen regardless of any scrollback
    /// offset: exit hints and session-id scans read the tail of the real
    /// screen, which must not change because the user wheeled up to read
    /// history on a dying pane. Saves and restores the offset under the lock,
    /// so a scrolled-back view is undisturbed.
    pub fn live_contents(&self) -> String {
        let Ok(mut p) = self.parser.lock() else {
            return String::new();
        };
        let offset = p.screen().scrollback();
        if offset == 0 {
            return p.screen().contents();
        }
        let s = p.screen_mut();
        s.set_scrollback(0);
        let out = s.contents();
        s.set_scrollback(offset);
        out
    }

    /// Text of the inclusive selection between two pane-local `(row, col)`
    /// cells (already normalized to reading order). The range is clamped
    /// against the CURRENT grid first: a resize mid-drag can leave stored
    /// cells past the new size, and vt100's `contents_between` does a bare
    /// `cols - start_col` subtraction (a debug-build panic otherwise).
    pub fn selection_text(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let Ok(parser) = self.parser.lock() else {
            return String::new();
        };
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        if rows == 0 || cols == 0 {
            return String::new();
        }
        let start = (start.0.min(rows - 1), start.1.min(cols - 1));
        let end = (end.0.min(rows - 1), end.1.min(cols - 1));
        // end col is exclusive in vt100; +1 makes our inclusive head count.
        screen.contents_between(start.0, start.1, end.0, end.1.saturating_add(1))
    }

    /// Screen-relative cursor position, or `None` when hidden. The cursor
    /// lives on the live screen, so it is also hidden while scrolled back:
    /// its coordinates would land on unrelated history rows.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let p = self.parser.lock().ok()?;
        let screen = p.screen();
        if screen.hide_cursor() || screen.scrollback() > 0 {
            None
        } else {
            let (row, col) = screen.cursor_position();
            Some((col, row))
        }
    }

    /// Composite the emulated screen into `area` of `buf`. The visible cursor is
    /// drawn as a reversed cell. Clamped to both the screen and `buf` bounds.
    /// `selection` is a normalized inclusive pane-local `(row, col)` range
    /// (reading order) rendered reversed, like a terminal's own highlight.
    pub fn blit(&self, buf: &mut Buffer, area: Rect, selection: Option<((u16, u16), (u16, u16))>) {
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        let (srows, scols) = screen.size();
        let buf_area = buf.area;
        // The cursor belongs to the live screen; scrolled back it would land
        // on unrelated history rows, so treat it as hidden (like cursor()).
        let cursor = if screen.hide_cursor() || screen.scrollback() > 0 {
            None
        } else {
            Some(screen.cursor_position()) // (row, col)
        };

        for row in 0..area.height.min(srows) {
            for col in 0..area.width.min(scols) {
                let x = area.x + col;
                let y = area.y + row;
                if x < buf_area.x || y < buf_area.y || x >= buf_area.right() || y >= buf_area.bottom()
                {
                    continue;
                }
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                // The second column of a wide glyph is owned by the wide cell
                // (ratatui reserves it from the glyph's display width); skip it
                // so we never stamp a blank over the right half.
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut style = Style::default()
                    .fg(map_color(cell.fgcolor()))
                    .bg(map_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                // SGR 2 / faint — e.g. Claude Code's ghosted input suggestions,
                // which should render dimmed rather than solid.
                if cell.dim() {
                    style = style.add_modifier(Modifier::DIM);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                // Tuple lexicographic order IS reading order for (row, col).
                let selected =
                    selection.is_some_and(|(s, e)| (row, col) >= s && (row, col) <= e);
                // XOR so the cursor stays visible on an already-inverse cell
                // (and a cursor inside a selection reads as a single-cell hole).
                if cell.inverse() ^ (cursor == Some((row, col))) ^ selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                let contents = cell.contents();
                // Blank for empty cells, and clamp a wide glyph that would
                // straddle the right edge (blit bypasses ratatui's width clip).
                let straddles_edge =
                    cell.is_wide() && (col + 2 > area.width.min(scols) || x + 1 >= buf_area.right());
                let symbol = if contents.is_empty() || straddles_edge {
                    " "
                } else {
                    contents
                };
                let target = &mut buf[(x, y)];
                target.set_symbol(symbol);
                target.set_style(style);
            }
        }
    }

    /// Non-blocking check for child exit; returns the exit code (if known) once
    /// the child has terminated.
    /// `None` while running; `Some(code)` once exited. A signal-killed child
    /// (Ctrl+C, SIGKILL, crash) yields `Some(None)` — there is no meaningful exit
    /// code, and callers must not treat it as a clean non-zero failure.
    pub fn try_wait(&mut self) -> Option<Option<i32>> {
        match self.child.try_wait() {
            Ok(Some(status)) if status.signal().is_some() => Some(None),
            Ok(Some(status)) => Some(Some(status.exit_code() as i32)),
            _ => None,
        }
    }

    /// Capture the process groups that must not outlive the pane: the child's
    /// own group and the PTY's current foreground group (a shell's running job,
    /// e.g. a dev server). Both kernel facts vanish once the child exits, so
    /// this runs at the START of a teardown, while the child is still alive.
    /// Never capture earlier (e.g. at spawn): the recycled-pgid race between
    /// capture and SIGKILL is accepted BECAUSE the window is one short
    /// teardown; pgids held for the pane's whole life would make it unbounded.
    /// Known ceilings: a job the user backgrounded (`Ctrl+Z`, `&`) has its own
    /// pgid and is not sampled here, and a descendant that `setsid`s itself
    /// escapes entirely; both would need a process-table walk.
    /// No-op if already captured (`signal_hangup` before `force_kill_and_reap`).
    fn capture_kill_groups(&mut self) {
        if !self.kill_groups.is_empty() {
            return;
        }
        let mut groups = Vec::new();
        if let Some(pid) = self.child.process_id()
            && let Ok(pgid) = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid as i32)))
        {
            groups.push(pgid.as_raw());
        }
        if let Some(fg) = self._master.process_group_leader() {
            groups.push(fg);
        }
        // Never signal pgid 0/1/-1 (self, init, or "everything").
        groups.retain(|&g| g > 1);
        groups.dedup();
        self.kill_groups = groups;
    }

    /// Best-effort signal to every captured group (a group already gone is
    /// simply ESRCH).
    fn signal_captured_groups(&self, sig: nix::sys::signal::Signal) {
        for &pgid in &self.kill_groups {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), sig);
        }
    }

    /// Gentle hangup to every captured group, so a job that handles SIGHUP
    /// (and one under a shell that doesn't forward it, like bash) gets a
    /// chance to shut down before the SIGKILL escalation.
    fn hangup_captured_groups(&self) {
        self.signal_captured_groups(nix::sys::signal::Signal::SIGHUP);
    }

    /// SIGKILL every captured process group, then forget them: the kill is
    /// single-fire per capture, so a later teardown pass over the same pane
    /// (Drop after the quit path) can never re-signal a pgid the kernel may
    /// have recycled since.
    fn kill_captured_groups(&mut self) {
        self.signal_captured_groups(nix::sys::signal::Signal::SIGKILL);
        self.kill_groups.clear();
    }

    /// Guaranteed teardown: SIGHUP, a bounded grace poll, then SIGKILL for a
    /// child that ignores the hangup, delivered to the child pid AND to the
    /// captured process groups, so a shell's foreground job (a dev server)
    /// dies with the pane instead of surviving the shell. Returns once the
    /// child is gone (or the grace+SIGKILL has been delivered).
    pub fn terminate(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            // Already exited: only groups captured by an earlier teardown are
            // safe to signal; fresh kernel state died with the child.
            self.kill_captured_groups();
            return;
        }
        let pid = self.child.process_id();
        self.capture_kill_groups();
        let _ = self.killer.kill(); // SIGHUP (gentle)
        self.hangup_captured_groups();
        for _ in 0..5 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Still alive after ~250ms — force it. Guarded on Ok(None) so we never
        // signal a pid that has already been reaped/recycled.
        if matches!(self.child.try_wait(), Ok(None))
            && let Some(pid) = pid
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        // The shell obeying the HUP doesn't save its job: kill the groups even
        // when the child exited during the grace poll.
        self.kill_captured_groups();
    }

    /// Terminate the child (alias for [`Pane::terminate`]).
    pub fn kill(&mut self) {
        self.terminate();
    }

    /// Whether the child has exited (non-blocking).
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Whether the reader thread has finished. PTY EOF arrives only after the
    /// child has exited AND every buffered chunk of its output was read, so a
    /// finished reader guarantees the final chunk is already parsed into the
    /// grid: a caller scanning a dead pane's screen (e.g. exit-hint capture)
    /// can trust it is fully drained. `true` also after `Drop` took the handle.
    pub fn reader_finished(&self) -> bool {
        #[cfg(test)]
        if self.force_reader_unfinished {
            return false;
        }
        self.reader.as_ref().is_none_or(|h| h.is_finished())
    }

    /// Send the gentle hangup (SIGHUP) without waiting. Pairs with
    /// [`Pane::force_kill_and_reap`] for a batched quit teardown that signals
    /// every pane first and waits once, instead of a full grace poll per pane.
    pub fn signal_hangup(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            // A capture-kind child already obeyed the SIGTERM: its groups (a
            // server it spawned) have not seen a gentle signal yet, and the
            // exit hint is drained by now, so pass the HUP on before the
            // force-kill escalation.
            self.hangup_captured_groups();
            return;
        }
        self.capture_kill_groups();
        let _ = self.killer.kill(); // SIGHUP (gentle)
        self.hangup_captured_groups();
    }

    /// Send SIGTERM by pid without waiting (no reap loop). portable-pty's
    /// cloned killer only delivers SIGHUP, and opencode prints its resumable
    /// session id on SIGTERM, not SIGHUP, so the teardown exit-hint capture
    /// needs the explicit signal. The caller drains the reader afterwards;
    /// the SIGHUP-then-SIGKILL guarantee ([`Pane::terminate`]) still runs
    /// later for a child that ignores this.
    pub fn signal_term(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return; // already exited
        }
        // This is the teardown start for capture-kind panes: the child may obey
        // the SIGTERM and exit during the capture grace, taking the group facts
        // with it; capture now so the later escalation can still clean up.
        self.capture_kill_groups();
        if let Some(pid) = self.child.process_id() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }

    /// SIGKILL a child that's still alive (by pid) and briefly reap it, so a
    /// later `Drop`/`terminate` sees an exited child and skips a second grace
    /// poll. Pairs with [`Pane::signal_hangup`].
    pub fn force_kill_and_reap(&mut self) {
        // Group kill first and unconditionally: on the quit path the shell
        // often exits within the shared grace while its HUP-ignoring job
        // survives; the job must die even though the child is already gone.
        self.kill_captured_groups();
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return; // already gone
        }
        if let Some(pid) = self.child.process_id() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        // SIGKILL can't be caught, so the child dies promptly; a short reap loop
        // collects it so the subsequent Drop's try_wait returns Ok(Some(_)).
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // Guarantee the child dies (terminate escalates to SIGKILL). We do NOT
        // join the reader thread — that would hang the TUI if teardown somehow
        // failed; SIGKILL closes the PTY slave, the reader gets EOF, and the
        // detached thread ends on its own.
        self.terminate();
        self.reader.take();
    }
}

fn map_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn mouse_button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Encode a crossterm mouse event into the bytes a terminal would send to a
/// child that has enabled mouse reporting, honoring the negotiated `mode` and
/// `encoding`. `col`/`row` are 0-based cells within the child's screen.
///
/// Returns `None` when the child wants no mouse input, or this specific event
/// isn't reported by the active mode (e.g. drag under press-only mode).
pub fn encode_mouse(
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
    kind: MouseEventKind,
    mods: KeyModifiers,
    col: u16,
    row: u16,
) -> Option<Vec<u8>> {
    if mode == MouseProtocolMode::None {
        return None;
    }
    // (base button code, is_release)
    let (mut cb, release): (u32, bool) = match kind {
        MouseEventKind::Down(b) => (mouse_button_code(b), false),
        MouseEventKind::Up(b) => {
            if mode == MouseProtocolMode::Press {
                return None; // X10 reports presses only
            }
            (mouse_button_code(b), true)
        }
        MouseEventKind::Drag(b) => {
            if !matches!(
                mode,
                MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion
            ) {
                return None;
            }
            (mouse_button_code(b) | 32, false) // + motion bit
        }
        MouseEventKind::Moved => {
            if mode != MouseProtocolMode::AnyMotion {
                return None;
            }
            (3 | 32, false) // no button held + motion bit
        }
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    if mods.contains(KeyModifiers::SHIFT) {
        cb |= 4;
    }
    if mods.contains(KeyModifiers::ALT) {
        cb |= 8;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        cb |= 16;
    }
    let x = col as u32 + 1; // mouse coords are 1-based
    let y = row as u32 + 1;
    match encoding {
        MouseProtocolEncoding::Sgr => {
            let final_byte = if release { b'm' } else { b'M' };
            let mut out = format!("\x1b[<{cb};{x};{y}").into_bytes();
            out.push(final_byte);
            Some(out)
        }
        // X10 "normal" encoding: ESC [ M  <cb+32> <x+32> <y+32>, with a release
        // marked by button bits 0b11. Cells past index 222 can't be represented
        // (the +32 byte would overflow), so drop rather than report a wrong cell.
        MouseProtocolEncoding::Default => {
            if x > 223 || y > 223 {
                return None;
            }
            let enc_cb = if release { (cb & !0b11) | 0b11 } else { cb };
            Some(vec![0x1b, b'[', b'M', (enc_cb + 32) as u8, (x + 32) as u8, (y + 32) as u8])
        }
        // ?1005 UTF-8 mouse encoding would multibyte-encode coords ≥ 95; claude
        // never negotiates it (it uses SGR), so rather than emit the corrupt
        // single-byte form, decline — the caller treats this as "nothing sent".
        MouseProtocolEncoding::Utf8 => None,
    }
}

/// OSC 52 clipboard-copy sequence for `text` (clipboard `c`) — the terminal
/// does the actual copying. Raw input is clamped to 64 KiB at a char boundary
/// first: most terminals cap the whole OSC sequence around ~100 KB, and a
/// selection that big isn't a copy-paste use case — truncate rather than
/// chunk (chunked OSC 52 is the upgrade path if anyone ever asks).
pub fn encode_osc52_copy(text: &str) -> Vec<u8> {
    use base64::Engine as _;
    const OSC52_MAX_RAW: usize = 64 * 1024;
    // `floor_char_boundary` is unstable on MSRV 1.88 — walk back by hand.
    let mut cut = text.len().min(OSC52_MAX_RAW);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend_from_slice(
        base64::engine::general_purpose::STANDARD.encode(&text[..cut]).as_bytes(),
    );
    out.push(0x07);
    out
}

/// Encode a crossterm key event into the bytes a terminal would send to the
/// child. Legacy encoding: the probe (see MIGRATION.md) confirmed interactive
/// `claude` honors these even when it negotiates the Kitty keyboard protocol, so
/// this is the working baseline. Returns `None` for keys with no encoding.
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    use ratatui::crossterm::event::KeyEventKind;
    // Only act on key presses — Release/Repeat (delivered under the enhanced
    // keyboard protocol the host enables) must not double-send.
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let bytes = match key.code {
        KeyCode::Char(c) if ctrl => {
            // Control bytes only exist for @A-Z[\]^_ (0x40..=0x5F -> 0x00..=0x1F)
            // plus Ctrl+Space (NUL). Anything else has no control encoding.
            let u = c.to_ascii_uppercase() as u8;
            if (0x40..=0x5F).contains(&u) {
                vec![u - 0x40]
            } else if c == ' ' {
                vec![0x00]
            } else {
                return None;
            }
        }
        KeyCode::Char(c) if alt => {
            // Alt+key sends ESC then the character.
            let mut v = vec![0x1b];
            v.extend_from_slice(c.to_string().as_bytes());
            v
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // Plain Enter submits (CR); Shift/Alt+Enter inserts a newline (LF), the
        // form claude's composer treats as a soft break, so it must differ.
        KeyCode::Enter if shift || alt => vec![b'\n'],
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    // A dev server started from the embedded shell must not outlive the pane:
    // the shell dies on the HUP, but its foreground job sits in its OWN process
    // group and may ignore SIGHUP (node CLIs do); signalling only the shell
    // pid used to leave it running. `set -m` gives the job its own group +
    // the terminal, like an interactive shell; the trailing `:` stops bash
    // exec-ing the job in place of itself.
    #[test]
    fn terminate_kills_hup_ignoring_foreground_job() {
        let mut pane = Pane::spawn(
            "bash",
            &["-c", "set -m; sh -c 'trap \"\" HUP; echo UP $$ OK; sleep 300'; :"],
            &tmp(),
            24,
            200,
        )
        .unwrap();
        let pid = wait_for_pid(&pane);
        pane.kill();
        let died = wait_for_death(pid, Duration::from_secs(5));
        kill_leftovers(pid);
        assert!(died, "HUP-ignoring foreground job survived pane teardown");
    }

    // The claude-tab shape: the pane child spawns a server that shares the
    // child's own process group. Both ignore SIGHUP, so the child needs the
    // SIGKILL escalation, which must take the whole group with it, not just
    // the child pid.
    #[test]
    fn terminate_kills_grandchild_in_childs_group() {
        let mut pane = Pane::spawn(
            "sh",
            &["-c", "trap '' HUP; sh -c 'trap \"\" HUP; echo UP $$ OK; sleep 300' & wait"],
            &tmp(),
            24,
            200,
        )
        .unwrap();
        let pid = wait_for_pid(&pane);
        pane.kill();
        let died = wait_for_death(pid, Duration::from_secs(5));
        kill_leftovers(pid);
        assert!(died, "grandchild in the child's group survived pane teardown");
    }

    // The quit path (signal_hangup -> shared grace -> force_kill_and_reap) must
    // kill the surviving job even though the shell itself exited during the
    // grace window.
    #[test]
    fn quit_path_kills_hup_ignoring_foreground_job() {
        let mut pane = Pane::spawn(
            "bash",
            &["-c", "set -m; sh -c 'trap \"\" HUP; echo UP $$ OK; sleep 300'; :"],
            &tmp(),
            24,
            200,
        )
        .unwrap();
        let pid = wait_for_pid(&pane);
        pane.signal_hangup();
        // Shared grace: wait for the shell itself to exit (it obeys the HUP).
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pane.has_exited() {
            assert!(Instant::now() < deadline, "shell did not exit on SIGHUP");
            std::thread::sleep(Duration::from_millis(10));
        }
        pane.force_kill_and_reap();
        let died = wait_for_death(pid, Duration::from_secs(5));
        kill_leftovers(pid);
        assert!(died, "HUP-ignoring job survived the quit teardown");
    }

    // The capture-kind quit shape: the child obeys SIGTERM and exits during
    // the capture grace while its HUP-and-TERM-ignoring server (sharing the
    // child's group; sh scripts don't give `&` jobs their own) lives on.
    // Returns once the child is gone, with the server still up.
    fn capture_kind_after_sigterm() -> (Pane, i32) {
        let mut pane = Pane::spawn(
            "sh",
            &["-c", "trap 'exit 0' TERM; sh -c 'trap \"\" HUP TERM; echo UP $$ OK; sleep 300' & wait"],
            &tmp(),
            24,
            200,
        )
        .unwrap();
        let pid = wait_for_pid(&pane);
        pane.signal_term();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pane.has_exited() {
            assert!(Instant::now() < deadline, "child did not exit on SIGTERM");
            std::thread::sleep(Duration::from_millis(10));
        }
        (pane, pid)
    }

    // The capture-kind quit path (signal_term -> capture grace -> signal_hangup
    // -> force_kill_and_reap): the groups captured by signal_term must still
    // die even though the child was already gone at every later stage.
    #[test]
    fn quit_path_kills_group_after_child_obeys_sigterm() {
        let (mut pane, pid) = capture_kind_after_sigterm();
        pane.signal_hangup();
        pane.force_kill_and_reap();
        let died = wait_for_death(pid, Duration::from_secs(5));
        kill_leftovers(pid);
        assert!(died, "server survived the capture-kind quit teardown");
    }

    // Same capture, torn down by Drop's terminate(): the already-exited branch
    // must SIGKILL the captured groups too.
    #[test]
    fn terminate_after_capture_exit_kills_captured_group() {
        let (mut pane, pid) = capture_kind_after_sigterm();
        pane.terminate();
        let died = wait_for_death(pid, Duration::from_secs(5));
        kill_leftovers(pid);
        assert!(died, "server survived terminate() after the capture exit");
    }

    fn wait_for_pid(pane: &Pane) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = pane.screen_contents();
            // Whole-line shape "UP <pid> OK": the trailing OK proves the grid
            // wasn't sampled mid-render with the pid cut short.
            for line in s.lines() {
                let mut words = line.split_whitespace();
                if words.next() == Some("UP")
                    && let Some(pid) = words.next().and_then(|p| p.parse().ok())
                    && words.next() == Some("OK")
                {
                    return pid;
                }
            }
            assert!(Instant::now() < deadline, "server never printed pid; screen:\n{s}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_for_death(pid: i32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    // Failure-path cleanup: SIGKILL the leaked job's whole group first (a
    // shell may fork `sleep` rather than exec it), then the pid itself.
    fn kill_leftovers(pid: i32) {
        let p = nix::unistd::Pid::from_raw(pid);
        if let Ok(pgid) = nix::unistd::getpgid(Some(p)) {
            let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
        }
        let _ = nix::sys::signal::kill(p, nix::sys::signal::Signal::SIGKILL);
    }

    #[test]
    fn scan_focus_reporting_detects_opt_in_out_and_straddled_sequences() {
        let mut tail = Vec::new();
        assert_eq!(scan_focus_reporting(&mut tail, b"hello world"), None);
        assert_eq!(scan_focus_reporting(&mut tail, b"x\x1b[?1004hy"), Some(true));
        assert_eq!(scan_focus_reporting(&mut tail, b"\x1b[?1004l"), Some(false));
        // Both in one chunk: the LAST occurrence wins.
        assert_eq!(scan_focus_reporting(&mut tail, b"\x1b[?1004h..\x1b[?1004l"), Some(false));
        // A sequence split across two reads still matches (the tail carry).
        let mut tail = Vec::new();
        assert_eq!(scan_focus_reporting(&mut tail, b"out\x1b[?10"), None);
        assert_eq!(scan_focus_reporting(&mut tail, b"04h more"), Some(true));
        // Split at every boundary, for good measure.
        let seq = b"\x1b[?1004h";
        for cut in 1..seq.len() {
            let mut tail = Vec::new();
            assert_eq!(scan_focus_reporting(&mut tail, &seq[..cut]), None, "cut {cut}");
            assert_eq!(scan_focus_reporting(&mut tail, &seq[cut..]), Some(true), "cut {cut}");
        }
    }

    #[test]
    fn sync_focus_gates_on_opt_in_and_reports_current_state_first() {
        let mut p = Pane::spawn("sh", &["-c", "sleep 30"], &tmp(), 24, 80).unwrap();
        // Not opted in: nothing is ever sent.
        p.sync_focus(true);
        assert_eq!(p.focus_sent(), None, "no report before the child opts in");
        // Opt in (as the reader's scan would): the first sync reports the
        // CURRENT state, focused or not.
        p.set_focus_reporting(true);
        p.sync_focus(false);
        assert_eq!(p.focus_sent(), Some(false), "initial state reported on opt-in");
        p.sync_focus(true);
        assert_eq!(p.focus_sent(), Some(true), "edge to focused");
        // Opting out clears the memory, so a re-opt-in reports fresh.
        p.set_focus_reporting(false);
        p.sync_focus(true);
        assert_eq!(p.focus_sent(), None, "opt-out resets the sent state");
        p.set_focus_reporting(true);
        p.sync_focus(true);
        assert_eq!(p.focus_sent(), Some(true), "re-opt-in reports again");
    }

    fn sgr(kind: MouseEventKind, mods: KeyModifiers, col: u16, row: u16) -> Option<String> {
        encode_mouse(
            MouseProtocolMode::ButtonMotion,
            MouseProtocolEncoding::Sgr,
            kind,
            mods,
            col,
            row,
        )
        .map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn encode_mouse_none_when_disabled() {
        assert_eq!(
            encode_mouse(
                MouseProtocolMode::None,
                MouseProtocolEncoding::Sgr,
                MouseEventKind::Down(MouseButton::Left),
                KeyModifiers::NONE,
                0,
                0,
            ),
            None,
        );
    }

    #[test]
    fn encode_mouse_sgr_press_release_and_coords() {
        // Left press at the top-left cell -> button 0, 1-based coords.
        assert_eq!(
            sgr(MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE, 0, 0).as_deref(),
            Some("\x1b[<0;1;1M")
        );
        // Release uses the trailing 'm'.
        assert_eq!(
            sgr(MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE, 0, 0).as_deref(),
            Some("\x1b[<0;1;1m")
        );
        // Right button at (col 5, row 3) -> button 2, coords 6;4.
        assert_eq!(
            sgr(MouseEventKind::Down(MouseButton::Right), KeyModifiers::NONE, 5, 3).as_deref(),
            Some("\x1b[<2;6;4M")
        );
        // Ctrl modifier adds 16 to the button code.
        assert_eq!(
            sgr(MouseEventKind::Down(MouseButton::Left), KeyModifiers::CONTROL, 0, 0).as_deref(),
            Some("\x1b[<16;1;1M")
        );
        // Scroll up is button 64.
        assert_eq!(
            sgr(MouseEventKind::ScrollUp, KeyModifiers::NONE, 0, 0).as_deref(),
            Some("\x1b[<64;1;1M")
        );
    }

    #[test]
    fn encode_mouse_mode_gating() {
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let moved = MouseEventKind::Moved;
        // Press-only (X10) reports no release and no motion.
        assert_eq!(
            encode_mouse(
                MouseProtocolMode::Press,
                MouseProtocolEncoding::Sgr,
                MouseEventKind::Up(MouseButton::Left),
                KeyModifiers::NONE,
                0,
                0
            ),
            None
        );
        // Drag needs ButtonMotion; Moved needs AnyMotion.
        let pr = MouseProtocolMode::PressRelease;
        assert_eq!(
            encode_mouse(pr, MouseProtocolEncoding::Sgr, drag, KeyModifiers::NONE, 0, 0),
            None
        );
        assert_eq!(
            sgr(drag, KeyModifiers::NONE, 0, 0).as_deref(),
            Some("\x1b[<32;1;1M") // button 0 + motion bit (32)
        );
        assert_eq!(
            encode_mouse(
                MouseProtocolMode::ButtonMotion,
                MouseProtocolEncoding::Sgr,
                moved,
                KeyModifiers::NONE,
                0,
                0
            ),
            None
        );
        assert_eq!(
            encode_mouse(
                MouseProtocolMode::AnyMotion,
                MouseProtocolEncoding::Sgr,
                moved,
                KeyModifiers::NONE,
                0,
                0
            )
            .map(|b| String::from_utf8(b).unwrap())
            .as_deref(),
            Some("\x1b[<35;1;1M") // no button (3) + motion bit (32)
        );
    }

    fn default_enc(kind: MouseEventKind, col: u16, row: u16) -> Option<Vec<u8>> {
        encode_mouse(
            MouseProtocolMode::PressRelease,
            MouseProtocolEncoding::Default,
            kind,
            KeyModifiers::NONE,
            col,
            row,
        )
    }

    #[test]
    fn encode_mouse_default_encoding_bytes() {
        // X10 normal encoding: ESC [ M  <cb+32> <x+32> <y+32>.
        assert_eq!(
            default_enc(MouseEventKind::Down(MouseButton::Left), 0, 0),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        // Release sets the low two button bits to 0b11 -> cb 3 -> 3+32 = 35.
        assert_eq!(
            default_enc(MouseEventKind::Up(MouseButton::Left), 0, 0),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        // Last representable cell (1-based 223 -> byte 255).
        assert_eq!(
            default_enc(MouseEventKind::Down(MouseButton::Left), 222, 0),
            Some(vec![0x1b, b'[', b'M', 32, 255, 33])
        );
        // Beyond that, drop rather than report a wrong cell.
        assert_eq!(default_enc(MouseEventKind::Down(MouseButton::Left), 223, 0), None);
    }

    #[test]
    fn encode_mouse_utf8_encoding_declines() {
        // ?1005 UTF-8 mouse mode is not supported (claude uses SGR) — never emit
        // the corrupt single-byte form.
        assert_eq!(
            encode_mouse(
                MouseProtocolMode::PressRelease,
                MouseProtocolEncoding::Utf8,
                MouseEventKind::Down(MouseButton::Left),
                KeyModifiers::NONE,
                100,
                0,
            ),
            None
        );
    }

    fn wait_until(pane: &Pane, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if pane.screen_contents().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn spawns_and_captures_output() {
        let cwd = tmp();
        let pane = Pane::spawn("sh", &["-c", "printf PANE-READY"], &cwd, 24, 80).unwrap();
        assert!(wait_until(&pane, "PANE-READY"), "screen:\n{}", pane.screen_contents());
        assert!(pane.output_seq() > 0);
        assert_eq!(pane.size(), (24, 80));
    }

    fn busy_within(pane: &Pane, want: Option<bool>, tries: u32) -> bool {
        for _ in 0..tries {
            if pane.foreground_busy() == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        pane.foreground_busy() == want
    }

    #[test]
    fn foreground_busy_tracks_running_command() {
        // An interactive shell enables job control, so a foreground command runs
        // in its own process group — exactly what `foreground_busy` keys off.
        let mut pane =
            Pane::spawn("bash", &["--noprofile", "--norc", "-i"], &tmp(), 24, 80).unwrap();
        // At the prompt the shell is the foreground group → idle.
        assert!(
            busy_within(&pane, Some(false), 60),
            "idle at prompt, got {:?}\n{}",
            pane.foreground_busy(),
            pane.screen_contents()
        );
        // Run a command that produces no output: a pure foreground-process signal.
        pane.send(b"sleep 3\n").unwrap();
        assert!(
            busy_within(&pane, Some(true), 60),
            "busy while sleep runs, got {:?}\n{}",
            pane.foreground_busy(),
            pane.screen_contents()
        );
        // Command exits → control returns to the prompt → idle again.
        assert!(
            busy_within(&pane, Some(false), 120),
            "idle after the command exits, got {:?}\n{}",
            pane.foreground_busy(),
            pane.screen_contents()
        );
        pane.kill();
    }

    #[test]
    fn resize_updates_size() {
        let mut pane = Pane::spawn("sh", &["-c", "sleep 3"], &tmp(), 24, 80).unwrap();
        pane.resize(10, 40).unwrap();
        assert_eq!(pane.size(), (10, 40));
        pane.resize(10, 40).unwrap(); // idempotent, no error
        assert_eq!(pane.size(), (10, 40));
        pane.kill();
    }

    #[test]
    fn kill_terminates_child() {
        let mut pane = Pane::spawn("sh", &["-c", "sleep 60"], &tmp(), 24, 80).unwrap();
        assert!(pane.try_wait().is_none(), "should be running");
        pane.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if pane.try_wait().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("child did not exit after kill");
    }

    #[test]
    fn kill_escalates_to_sigkill_for_sighup_ignoring_child() {
        // Child traps (ignores) SIGHUP — only the SIGKILL escalation can kill it.
        let mut pane =
            Pane::spawn("sh", &["-c", "trap '' HUP; sleep 60"], &tmp(), 24, 80).unwrap();
        assert!(pane.try_wait().is_none(), "should be running");
        pane.terminate(); // blocks through the grace + SIGKILL
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if pane.try_wait().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("SIGHUP-ignoring child survived terminate()");
    }

    #[test]
    fn signal_term_delivers_sigterm() {
        // The teardown-capture contract: signal_term must deliver SIGTERM
        // (not SIGHUP/SIGKILL), the trap below only fires on TERM. `sleep 60
        // & wait $!` rather than a bare sleep: a foreground sleep defers the
        // trap until sleep exits, a waited background child runs it promptly.
        // READY gates the signal so it can't race the trap installation.
        let mut pane = Pane::spawn(
            "sh",
            &["-c", "trap 'printf TERMED; exit 0' TERM; printf READY; sleep 60 & wait $!"],
            &tmp(),
            24,
            80,
        )
        .unwrap();
        assert!(wait_until(&pane, "READY"), "screen:\n{}", pane.screen_contents());
        pane.signal_term();
        assert!(wait_until(&pane, "TERMED"), "screen:\n{}", pane.screen_contents());
    }

    #[test]
    fn try_wait_reports_signal_exit_as_none() {
        // A signal-killed child has no meaningful exit code: try_wait yields
        // Some(None), not Some(Some(1)) — callers rely on this to avoid treating
        // a Ctrl+C/SIGKILL as a clean non-zero failure.
        let mut pane = Pane::spawn("sh", &["-c", "sleep 60"], &tmp(), 24, 80).unwrap();
        assert!(pane.try_wait().is_none(), "should be running");
        pane.kill(); // SIGHUP -> (grace) -> SIGKILL
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(code) = pane.try_wait() {
                assert_eq!(code, None, "signal exit must be None, got {code:?}");
                return;
            }
            assert!(Instant::now() < deadline, "child did not exit after kill");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn reader_finished_means_exited_and_fully_drained() {
        // The contract exit-hint capture rests on: PTY EOF comes only after
        // the child exited and all buffered output was read, so a finished
        // reader implies the final chunk is already on the grid.
        let mut pane = Pane::spawn("sh", &["-c", "printf DRAINED"], &tmp(), 5, 20).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pane.reader_finished() {
            assert!(Instant::now() < deadline, "reader never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(pane.has_exited(), "EOF only after the child exited");
        assert!(
            pane.screen_contents().contains("DRAINED"),
            "finished reader implies a drained grid:\n{}",
            pane.screen_contents()
        );
    }

    #[test]
    fn drop_does_not_hang() {
        let start = Instant::now();
        {
            let _pane = Pane::spawn("sh", &["-c", "sleep 60"], &tmp(), 24, 80).unwrap();
        } // Drop here must terminate the child without joining the reader.
        assert!(start.elapsed() < Duration::from_secs(2), "Drop hung");
    }

    #[test]
    fn blit_renders_into_buffer() {
        let pane = Pane::spawn("sh", &["-c", "printf HELLO"], &tmp(), 5, 20).unwrap();
        assert!(wait_until(&pane, "HELLO"));
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        pane.blit(&mut buf, Rect::new(0, 0, 20, 5), None);
        let row0: String = (0..5).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row0, "HELLO");
        // Plain text emits no SGR 2, so it must not be dimmed — proves the dim
        // mapping is conditional on cell.dim(), not applied unconditionally.
        assert!(!buf[(0, 0)].style().add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn blit_reflects_clear_and_redraw() {
        // The dominant interactive-TUI case: clear screen + redraw. blit must
        // reflect the live grid, not stale content.
        let pane = Pane::spawn(
            "sh",
            &["-c", "printf FIRST; printf '\\033[2J\\033[HSECOND'"],
            &tmp(),
            5,
            20,
        )
        .unwrap();
        assert!(wait_until(&pane, "SECOND"), "screen:\n{}", pane.screen_contents());
        assert!(!pane.screen_contents().contains("FIRST"));
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        pane.blit(&mut buf, Rect::new(0, 0, 20, 5), None);
        let row0: String = (0..6).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row0, "SECOND");
    }

    #[test]
    fn blit_respects_offset_and_bounds() {
        let pane = Pane::spawn("sh", &["-c", "printf XY"], &tmp(), 3, 10).unwrap();
        assert!(wait_until(&pane, "XY"));
        // Buffer larger than the pane area; render into an offset sub-rect.
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
        pane.blit(&mut buf, Rect::new(5, 2, 10, 3), None);
        assert_eq!(buf[(5, 2)].symbol(), "X");
        assert_eq!(buf[(6, 2)].symbol(), "Y");
        // Untouched cell stays default.
        assert_eq!(buf[(0, 0)].symbol(), " ");
    }

    #[test]
    fn blit_maps_dim_cell_to_dim_modifier() {
        // SGR 2 (faint) is what Claude Code's ghosted input suggestions use;
        // blit must surface it as Modifier::DIM (and not conflate it with BOLD).
        let pane = Pane::spawn("sh", &["-c", "printf '\\033[2mD'"], &tmp(), 3, 10).unwrap();
        assert!(wait_until(&pane, "D"));
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        pane.blit(&mut buf, Rect::new(0, 0, 10, 3), None);
        let m = buf[(0, 0)].style().add_modifier;
        assert!(m.contains(Modifier::DIM));
        assert!(!m.contains(Modifier::BOLD)); // DIM must not be conflated with BOLD (SGR 1 vs 2)
    }

    #[test]
    fn wants_mouse_tracks_child_mode() {
        let plain = Pane::spawn("sh", &["-c", "printf P; sleep 30"], &tmp(), 3, 10).unwrap();
        assert!(wait_until(&plain, "P"));
        assert!(!plain.wants_mouse(), "plain sh never enables mouse reporting");
        let mousey =
            Pane::spawn("sh", &["-c", "printf '\\033[?1000hM'; sleep 30"], &tmp(), 3, 10)
                .unwrap();
        assert!(wait_until(&mousey, "M"));
        assert!(mousey.wants_mouse(), "?1000h child holds mouse mode");
    }

    fn wheel(pane: &mut Pane, kind: MouseEventKind, ticks: u32) {
        for _ in 0..ticks {
            pane.send_mouse(kind, KeyModifiers::NONE, 0, 0);
        }
    }

    /// A file the child polls for, gating its next output burst on the test
    /// (deterministic phasing, no sleep races). Pid-scoped; a stale file from
    /// a recycled pid is removed so the gate starts closed.
    fn flag_path(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("kmd-pane-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn wheel_scrolls_history_for_mouse_less_child_and_input_snaps_back() {
        // A plain shell never enables mouse reporting; the wheel must scroll a
        // local view into history instead of being dropped (the dev-server
        // case: long output, then trying to read the beginning).
        let mut pane = Pane::spawn("sh", &["-c", "seq 1 40; sleep 30"], &tmp(), 5, 20).unwrap();
        assert!(wait_until(&pane, "40"), "screen:\n{}", pane.screen_contents());
        // One tick moves exactly WHEEL_STEP rows (the live grid ends with
        // 37..40 plus the blank cursor row left by the trailing newline).
        wheel(&mut pane, MouseEventKind::ScrollUp, 1);
        assert_eq!(pane.screen_contents(), "34\n35\n36\n37\n38", "one tick = WHEEL_STEP rows");
        // Scroll well past the top: the offset clamps to the stored history,
        // so the view lands on the very first lines.
        wheel(&mut pane, MouseEventKind::ScrollUp, 40);
        let top = pane.screen_contents();
        assert_eq!(top, "1\n2\n3\n4\n5", "view at the top of history:\n{top}");
        // The live cursor is off-view while scrolled back: don't draw it, in
        // cursor() or in blit's render path (no reversed cursor cell).
        assert_eq!(pane.cursor(), None, "cursor hidden while scrolled back");
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        pane.blit(&mut buf, Rect::new(0, 0, 20, 5), None);
        assert_eq!(buf[(0, 0)].symbol(), "1", "blit renders the scrolled-back view");
        for y in 0..5u16 {
            for x in 0..20u16 {
                assert!(
                    !buf[(x, y)].style().add_modifier.contains(Modifier::REVERSED),
                    "no cursor cell while scrolled back (found at {x},{y})"
                );
            }
        }
        // Non-wheel events fall through untouched: nothing scrolls, nothing
        // is sent (a plain shell has had no input at all yet).
        assert!(!pane.send_mouse(MouseEventKind::ScrollLeft, KeyModifiers::NONE, 0, 0));
        assert_eq!(pane.screen_contents(), top, "tilt must not move the view");
        assert!(pane.last_input_at().is_none(), "tilt must not reach the child");
        // Scrolling down past the bottom clamps back to the live screen.
        wheel(&mut pane, MouseEventKind::ScrollDown, 100);
        assert!(pane.screen_contents().contains("40"));
        assert!(pane.cursor().is_some(), "cursor back on the live screen");
        // Typing while scrolled back snaps to the live screen first.
        wheel(&mut pane, MouseEventKind::ScrollUp, 40);
        assert_eq!(pane.screen_contents(), "1\n2\n3\n4\n5");
        // The machine-scan view is the live screen even while scrolled back.
        assert!(
            pane.live_contents().contains("40"),
            "live_contents reads the real bottom:\n{}",
            pane.live_contents()
        );
        assert_eq!(pane.screen_contents(), "1\n2\n3\n4\n5", "probe must not move the view");
        pane.send_key(k(KeyCode::Char('x'))).unwrap();
        assert!(
            pane.screen_contents().contains("40"),
            "input snaps the view to the bottom:\n{}",
            pane.screen_contents()
        );
        pane.kill();
    }

    /// Shared body: print `1..=first`, scroll back 2 ticks (an unclamped
    /// offset of 6 rows), release the child's file-gated second burst up to
    /// `last`, and assert the view held still while it landed. vt100 anchors
    /// natively (it moves the offset with each row entering history, clamped
    /// at capacity); the pin here is that the reader thread does nothing to
    /// disturb that. The file gate makes the phases deterministic: the first
    /// screen can't scroll away before wait_until sees it, and seq0 is
    /// captured before the burst can start.
    fn assert_anchored_during_burst(name: &str, first: u32, last: u32) {
        let flag = flag_path(name);
        let script = format!(
            "seq 1 {first}; while [ ! -e '{}' ]; do sleep 0.05; done; seq {} {last}; sleep 30",
            flag.display(),
            first + 1,
        );
        let mut pane = Pane::spawn("sh", &["-c", &script], &tmp(), 5, 20).unwrap();
        assert!(wait_until(&pane, &first.to_string()), "screen:\n{}", pane.screen_contents());
        wheel(&mut pane, MouseEventKind::ScrollUp, 2);
        // The live grid ends with first-3..=first plus the blank cursor row
        // from the trailing newline; 6 rows up shows first-9..=first-5.
        let before = pane.screen_contents();
        let expect: Vec<String> = (first - 9..=first - 5).map(|n| n.to_string()).collect();
        assert_eq!(before, expect.join("\n"), "2 ticks = 6 rows up");
        let seq0 = pane.output_seq();
        std::fs::write(&flag, b"").unwrap(); // release the second burst
        let deadline = Instant::now() + Duration::from_secs(5);
        while pane.output_seq() == seq0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(pane.output_seq() > seq0, "second burst never arrived");
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(pane.screen_contents(), before, "view anchored during output");
        // Snapping back shows the new tail.
        pane.send_key(k(KeyCode::Char('x'))).unwrap();
        assert!(wait_until(&pane, &last.to_string()), "screen:\n{}", pane.screen_contents());
        pane.kill();
        let _ = std::fs::remove_file(&flag);
    }

    #[test]
    fn output_while_scrolled_back_keeps_the_view_anchored() {
        assert_anchored_during_burst("anchor", 40, 80);
    }

    #[test]
    fn view_stays_anchored_at_scrollback_capacity() {
        // 2100 lines overfill the 2000-line cap first: at capacity every new
        // row pops the oldest, and vt100 still moves the offset so the view
        // holds. A manual offset fixup is exactly what would drift here (PR
        // #110's review); this also pins the crate's at-capacity behavior
        // against future vt100 bumps.
        assert_anchored_during_burst("anchor-cap", 2100, 2140);
    }

    #[test]
    fn wheel_in_alternate_screen_sends_arrow_keys() {
        // The alternate screen has no scrollback; the wheel converts to arrow
        // keys (the alternateScroll convention) so pagers in a shell tab
        // still scroll. The pty echoes our arrows back as ^[[A (ECHOCTL).
        let mut pane = Pane::spawn(
            "sh",
            &["-c", "printf '\\033[?1049hREADY'; sleep 30"],
            &tmp(),
            5,
            40,
        )
        .unwrap();
        assert!(wait_until(&pane, "READY"), "screen:\n{}", pane.screen_contents());
        wheel(&mut pane, MouseEventKind::ScrollUp, 1);
        assert!(
            wait_until(&pane, "^[[A"),
            "arrows echoed in the alt screen:\n{}",
            pane.screen_contents()
        );
        pane.kill();
    }

    #[test]
    fn wheel_in_alternate_screen_honors_application_cursor_keys() {
        // DECCKM (?1h) switches cursor keys to SS3; a pager/editor in that
        // mode expects ESC O A, not CSI A. ECHOCTL echoes it as ^[OA.
        let mut pane = Pane::spawn(
            "sh",
            &["-c", "printf '\\033[?1049h\\033[?1hREADY'; sleep 30"],
            &tmp(),
            5,
            40,
        )
        .unwrap();
        assert!(wait_until(&pane, "READY"), "screen:\n{}", pane.screen_contents());
        wheel(&mut pane, MouseEventKind::ScrollDown, 1);
        assert!(
            wait_until(&pane, "^[OB"),
            "SS3 arrows echoed under DECCKM:\n{}",
            pane.screen_contents()
        );
        pane.kill();
    }

    #[test]
    fn selection_text_trims_and_joins_rows() {
        let pane =
            Pane::spawn("sh", &["-c", "printf 'AB CD\\r\\nEF'"], &tmp(), 5, 20).unwrap();
        assert!(wait_until(&pane, "EF"));
        // Single row, inclusive head.
        assert_eq!(pane.selection_text((1, 0), (1, 1)), "EF");
        // Multi-row: first row runs to its (trimmed) end, rows join with \n.
        assert_eq!(pane.selection_text((0, 3), (1, 1)), "CD\nEF");
        // Inverted range extracts nothing.
        assert_eq!(pane.selection_text((1, 1), (0, 3)), "");
        // Cells past the grid clamp instead of panicking (resize mid-drag):
        // the range runs to the last grid row (empty rows join as newlines).
        assert_eq!(pane.selection_text((1, 0), (400, 400)), "EF\n\n\n");
    }

    #[test]
    fn blit_reverses_selected_cells() {
        let pane = Pane::spawn("sh", &["-c", "printf HELLO"], &tmp(), 5, 20).unwrap();
        assert!(wait_until(&pane, "HELLO"));
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        pane.blit(&mut buf, Rect::new(0, 0, 20, 5), Some(((0, 1), (0, 3))));
        for x in [1, 2, 3] {
            assert!(
                buf[(x, 0)].style().add_modifier.contains(Modifier::REVERSED),
                "cell {x} inside the selection is reversed"
            );
        }
        for x in [0, 4] {
            assert!(
                !buf[(x, 0)].style().add_modifier.contains(Modifier::REVERSED),
                "cell {x} outside the selection is untouched"
            );
        }

        // A two-row selection fills in READING ORDER: a first-row cell PAST
        // the head column is still inside (tuple order compares row first).
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        pane.blit(&mut buf, Rect::new(0, 0, 20, 5), Some(((0, 1), (1, 2))));
        assert!(
            buf[(4, 0)].style().add_modifier.contains(Modifier::REVERSED),
            "row 0 col 4 (past the head column) is inside the reading-order fill"
        );
        assert!(
            buf[(2, 1)].style().add_modifier.contains(Modifier::REVERSED),
            "row 1 up to the head column is inside"
        );
        assert!(
            !buf[(0, 0)].style().add_modifier.contains(Modifier::REVERSED),
            "row 0 before the anchor column stays outside"
        );
        assert!(
            !buf[(3, 1)].style().add_modifier.contains(Modifier::REVERSED),
            "row 1 past the head column stays outside"
        );
    }

    #[test]
    fn encode_osc52_copy_emits_exact_sequence() {
        assert_eq!(encode_osc52_copy("hello"), b"\x1b]52;c;aGVsbG8=\x07".to_vec());
        // The 64 KiB clamp cuts at a char boundary: with 3-byte € chars the
        // limit (65536) lands mid-char and must walk back to 65535 bytes.
        use base64::Engine as _;
        let big = "€".repeat(30_000); // 90_000 bytes
        let out = encode_osc52_copy(&big);
        let payload = &out[b"\x1b]52;c;".len()..out.len() - 1];
        let decoded = base64::engine::general_purpose::STANDARD.decode(payload).unwrap();
        assert_eq!(decoded.len(), 65_535, "cut walked back to the char boundary");
        assert!(std::str::from_utf8(&decoded).is_ok(), "no split char in the payload");
    }

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn encode_common_keys() {
        assert_eq!(encode_key(k(KeyCode::Enter)), Some(vec![b'\r']));
        assert_eq!(encode_key(k(KeyCode::Char('a'))), Some(b"a".to_vec()));
        assert_eq!(encode_key(k(KeyCode::Backspace)), Some(vec![0x7f]));
        assert_eq!(encode_key(k(KeyCode::Tab)), Some(vec![b'\t']));
        assert_eq!(encode_key(k(KeyCode::Esc)), Some(vec![0x1b]));
        assert_eq!(encode_key(k(KeyCode::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(k(KeyCode::Left)), Some(b"\x1b[D".to_vec()));
        assert_eq!(encode_key(k(KeyCode::BackTab)), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn encode_ctrl_and_alt() {
        // Ctrl+C -> 0x03 (the interrupt the probe verified claude honors).
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        // Ctrl+A -> 0x01.
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Some(vec![0x01])
        );
        // Alt+b -> ESC b.
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'b'])
        );
        // Ctrl+Space -> NUL; Ctrl+[ -> ESC (0x1b); Ctrl+digit has no encoding.
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            Some(vec![0x00])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::CONTROL)),
            Some(vec![0x1b])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn encode_release_is_ignored() {
        use ratatui::crossterm::event::{KeyEventKind, KeyEventState};
        let release = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        assert_eq!(encode_key(release), None);
    }

    #[test]
    fn shift_enter_is_newline_not_submit() {
        let plain = encode_key(k(KeyCode::Enter));
        let shifted = encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(plain, Some(vec![b'\r']));
        assert_eq!(shifted, Some(vec![b'\n']));
        assert_ne!(plain, shifted);
    }

    #[test]
    fn send_key_round_trips_into_child() {
        // `cat` echoes stdin to the pty; typed bytes appear on screen.
        let mut pane = Pane::spawn("cat", &[], &tmp(), 5, 20).unwrap();
        assert!(
            pane.last_input_at().is_none(),
            "no input sent yet, nothing to stamp"
        );
        pane.send_key(k(KeyCode::Char('h'))).unwrap();
        pane.send_key(k(KeyCode::Char('i'))).unwrap();
        assert!(
            pane.last_input_at().is_some(),
            "every write to the child must stamp last_input"
        );
        assert!(wait_until(&pane, "hi"), "screen:\n{}", pane.screen_contents());
        pane.kill();
    }
}
