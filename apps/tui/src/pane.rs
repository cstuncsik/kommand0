//! An embedded terminal pane: a child process running in a pseudo-terminal,
//! emulated with vt100 and composited into a ratatui buffer.
//!
//! This is the foundation of the PTY-passthrough architecture (see MIGRATION.md):
//! a [`Pane`] owns a PTY, spawns a command in it (e.g. interactive `claude`),
//! pumps its output through a vt100 parser on a background thread, forwards
//! keystrokes back, and renders the emulated screen.
//!
//! Phase 1 deliverable: a standalone, tested module. It is not yet wired into
//! the app event loop (that is Phase 2, which also adds a reader→repaint wakeup;
//! for now callers can poll [`Pane::output_seq`] to detect activity).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// A child process running in a pseudo-terminal, with its screen emulated.
///
/// Termination note: portable-pty does not put the child in its own process
/// group, and its cloned `ChildKiller` only delivers SIGHUP (no escalation), so
/// a child that ignores SIGHUP (e.g. a Node-based `claude`) would survive. The
/// pane therefore guarantees teardown itself: SIGHUP, a brief grace poll, then
/// SIGKILL by pid (see [`Pane::terminate`]). SIGKILL closes the PTY slave, which
/// gives the reader thread EOF so the detached thread ends.
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
    rows: u16,
    cols: u16,
}

impl Pane {
    /// Spawn `program` (with `args`) in a fresh PTY of `rows`×`cols`, running in
    /// `cwd`, and start pumping its output into a vt100 emulator.
    pub fn spawn(program: &str, args: &[&str], cwd: &Path, rows: u16, cols: u16) -> Result<Pane> {
        Self::spawn_with_wake(program, args, cwd, rows, cols, None)
    }

    /// Like [`Pane::spawn`], but `wake` is invoked (off the UI thread) after each
    /// chunk of child output, so an event loop can schedule a coalesced repaint
    /// instead of polling — keystroke echo stays responsive.
    pub fn spawn_with_wake(
        program: &str,
        args: &[&str],
        cwd: &Path,
        rows: u16,
        cols: u16,
        wake: Option<Box<dyn Fn() + Send>>,
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

        let child = pair.slave.spawn_command(cmd).context("spawn in pty failed")?;
        drop(pair.slave); // close our handle to the slave so EOF propagates on exit
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("clone pty reader failed")?;
        let writer = pair.master.take_writer().context("take pty writer failed")?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let output_seq = Arc::new(AtomicU64::new(0));
        let parser_t = parser.clone();
        let seq_t = output_seq.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF: child exited / pty closed
                    Ok(n) => {
                        if let Ok(mut p) = parser_t.lock() {
                            p.process(&buf[..n]);
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
            p.set_size(rows, cols);
        }
        self.rows = rows;
        self.cols = cols;
        Ok(())
    }

    /// Write raw bytes to the child's stdin.
    pub fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).context("pty write failed")?;
        self.writer.flush().context("pty flush failed")?;
        Ok(())
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

    /// Monotonic counter of output chunks received; compare against a previous
    /// value to tell whether the child produced output in the interim.
    pub fn output_seq(&self) -> u64 {
        self.output_seq.load(Ordering::Relaxed)
    }

    /// Current emulated size (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// Plain-text snapshot of the emulated screen (for tests/logging).
    pub fn screen_contents(&self) -> String {
        self.parser
            .lock()
            .map(|p| p.screen().contents())
            .unwrap_or_default()
    }

    /// Screen-relative cursor position, or `None` when hidden.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let p = self.parser.lock().ok()?;
        let screen = p.screen();
        if screen.hide_cursor() {
            None
        } else {
            let (row, col) = screen.cursor_position();
            Some((col, row))
        }
    }

    /// Composite the emulated screen into `area` of `buf`. The visible cursor is
    /// drawn as a reversed cell. Clamped to both the screen and `buf` bounds.
    pub fn blit(&self, buf: &mut Buffer, area: Rect) {
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        let (srows, scols) = screen.size();
        let buf_area = buf.area;
        let cursor = if screen.hide_cursor() {
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
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                // XOR so the cursor stays visible on an already-inverse cell.
                if cell.inverse() ^ (cursor == Some((row, col))) {
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
                    &contents
                };
                let target = &mut buf[(x, y)];
                target.set_symbol(symbol);
                target.set_style(style);
            }
        }
    }

    /// Non-blocking check for child exit; returns the exit code (if known) once
    /// the child has terminated.
    pub fn try_wait(&mut self) -> Option<Option<i32>> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(Some(status.exit_code() as i32)),
            _ => None,
        }
    }

    /// Guaranteed teardown: SIGHUP, a bounded grace poll, then SIGKILL by pid for
    /// a child that ignores the hangup. Returns once the child is gone (or the
    /// grace+SIGKILL has been delivered).
    pub fn terminate(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return; // already exited
        }
        let pid = self.child.process_id();
        let _ = self.killer.kill(); // SIGHUP (gentle)
        for _ in 0..5 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
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
    }

    /// Terminate the child (alias for [`Pane::terminate`]).
    pub fn kill(&mut self) {
        self.terminate();
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
        pane.blit(&mut buf, Rect::new(0, 0, 20, 5));
        let row0: String = (0..5).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row0, "HELLO");
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
        pane.blit(&mut buf, Rect::new(0, 0, 20, 5));
        let row0: String = (0..6).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row0, "SECOND");
    }

    #[test]
    fn blit_respects_offset_and_bounds() {
        let pane = Pane::spawn("sh", &["-c", "printf XY"], &tmp(), 3, 10).unwrap();
        assert!(wait_until(&pane, "XY"));
        // Buffer larger than the pane area; render into an offset sub-rect.
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
        pane.blit(&mut buf, Rect::new(5, 2, 10, 3));
        assert_eq!(buf[(5, 2)].symbol(), "X");
        assert_eq!(buf[(6, 2)].symbol(), "Y");
        // Untouched cell stays default.
        assert_eq!(buf[(0, 0)].symbol(), " ");
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
        pane.send_key(k(KeyCode::Char('h'))).unwrap();
        pane.send_key(k(KeyCode::Char('i'))).unwrap();
        assert!(wait_until(&pane, "hi"), "screen:\n{}", pane.screen_contents());
        pane.kill();
    }
}
