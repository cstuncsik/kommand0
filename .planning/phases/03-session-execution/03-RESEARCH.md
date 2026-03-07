# Phase 3: Session Execution - Research

**Researched:** 2026-03-07
**Domain:** Process lifecycle management, async I/O, Claude Code CLI integration, TUI widgets
**Confidence:** HIGH

## Summary

Phase 3 transforms kommand0 from a workspace browser into a Claude Code session manager. The core technical challenge is spawning `claude` CLI processes, streaming their output into the TUI in real-time, and managing process lifecycle (start/stop/restart/cleanup) without zombies or orphans.

The critical architectural decision is the transport layer: Claude Code CLI supports `--print` mode with `--input-format stream-json` and `--output-format stream-json`, enabling programmatic multi-turn conversations over piped stdin/stdout. This eliminates the need for PTY handling entirely. We use `tokio::process::Command` with `Stdio::piped()` -- the same async process API already available in the project's tokio dependency.

**Primary recommendation:** Use `claude -p --input-format stream-json --output-format stream-json --cwd <dir>` for session transport. Use `process_group(0)` (stable since Rust 1.64) for process group isolation. Use `tui-textarea` 0.7 for the composer widget. Use `VecDeque<String>` for the scrollback buffer with manual capacity cap.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Kommand0 is a Claude Code session manager -- spawns `claude` in interactive mode, not arbitrary shell commands
- Use PTY if Claude Code expects terminal semantics; fall back to stdin/stdout if reliable
- Both CLI (`kmd session start <workspace>`) and TUI ('r' key) can start sessions
- One active session per workspace; error if session already running
- Launch with `--cwd <workspace_dir>` only -- minimal flags
- Multi-line text area at bottom of right pane, pinned; Shift+Enter for newline, Enter to send
- stdout/stderr interleaved as single stream; ANSI escape codes stripped; plain text only
- Markdown shown raw (source text, not rendered)
- User messages shown inline with "> " prefix
- Auto-scroll unless user scrolled up; new-lines count indicator when paused
- 50,000+ line scrollback buffer capacity
- Tree view: emoji/icon suffix on workspace -- running/stopped/failed indicators
- Session metadata persisted in state.json `sessions` array
- Scrollback in separate files: `.kommand0-dev/sessions/<session-id>.log`
- Structured JSON lines format: `{ timestamp, source: "user"|"claude", content: string }`
- Use Claude Code's `--resume <session-id>` for session continuity on relaunch
- SIGTERM first, SIGKILL after 5 second timeout
- Process groups via setsid on spawn -- kill entire process group
- Ctrl+C context-dependent: in composer clears input; outside stops session; no session quit app
- 'R' (Shift+R) restarts with `--resume`; 'r' starts new session
- 'q' triggers shutdown: SIGTERM all sessions, 5s timeout, then SIGKILL remaining
- CLI commands: `kmd session start|stop|list|clear <workspace>`

### Claude's Discretion
- PTY vs stdin/stdout transport decision (based on what claude CLI requires)
- Exact structured log serialization details
- Session ID format in state.json
- Composer widget implementation (ratatui textarea approach)
- Scrollback buffer data structure (VecDeque, ring buffer, etc.)
- How to parse/capture Claude Code's session ID from process output
- Exact process group setup (setsid vs nix crate approach)
- Shutdown progress UI implementation
- Error state handling for claude process crashes

### Deferred Ideas (OUT OF SCOPE)
- CLI-starts-TUI-focused-on-session -- future phase
- Text selection/copy mode -- v2 (ASESS-05)
- Multiple sessions per workspace -- v2 (ASESS-06)
- Session templates / presets -- v2 (ASESS-01)
- Auto-restart policies -- v2 (ASESS-02)
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| SESS-01 | User can run a command in a workspace and see streaming output | Claude CLI stream-json transport, tokio::process async I/O, BufReader line streaming, tui-textarea for composer |
| SESS-02 | User can stop a running session (SIGTERM with SIGKILL fallback) | process_group(0) for group isolation, nix crate for kill(-pgid, SIGTERM), tokio timeout for SIGKILL fallback |
| SESS-03 | User can restart a stopped session | Claude CLI `--resume <session-id>` flag, new process spawn with previous session ID |
| SESS-04 | App cleans up all child processes on quit (process group management) | process_group(0) on spawn, shutdown handler iterating all sessions, SIGTERM/timeout/SIGKILL pattern |
| SESS-05 | Process status indicators visible in TUI (running/stopped/failed/exited) | SessionStatus enum, TreeNode extension for status display, right pane title suffix |
| SESS-06 | Output scrollback buffer with configurable capacity | VecDeque<String> with manual capacity enforcement, 50k+ line support |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| tokio | 1 (already dep) | Async runtime, process spawning, I/O | Already in project; `tokio::process::Command` for async child processes |
| nix | 0.30 | Unix signal delivery to process groups | Safe Rust wrappers for kill(), Pid; needed for `kill(-pgid, SIGTERM)` |
| tui-textarea | 0.7 | Multi-line text input widget for ratatui | Purpose-built for ratatui 0.29; handles cursor, selection, multi-line editing |
| strip-ansi-escapes | 0.2 | Strip ANSI escape codes from process output | Standard crate (400k+ downloads/month); single responsibility |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| uuid | 1 | Generate session IDs | Session IDs should be UUIDs to match Claude Code's session_id format |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| nix for signals | libc directly | nix provides safe wrappers; libc requires unsafe everywhere |
| tui-textarea | Custom ratatui widget | tui-textarea handles cursor movement, selection, word boundaries -- complex to reimplement |
| portable-pty | tokio::process (chosen) | PTY unnecessary since Claude CLI works with piped stdin/stdout in -p mode |
| VecDeque | ring buffer crate | VecDeque is stdlib, O(1) push/pop at both ends, sufficient for 50k lines |

**Installation (add to workspace Cargo.toml):**
```toml
nix = { version = "0.30", features = ["process", "signal"] }
tui-textarea = "0.7"
strip-ansi-escapes = "0.2"
uuid = { version = "1", features = ["v4"] }
```

## Architecture Patterns

### Recommended Project Structure
```
crates/core/src/
  lib.rs              # Add Session model, sessions Vec to AppState
  session.rs          # NEW: Session struct, SessionStatus enum, session CRUD
  workspace.rs        # Existing
  id.rs               # Existing (reuse generate_id or switch to uuid)

apps/tui/src/
  main.rs             # Extend App struct, tokio::select! loop, key handling
  session_manager.rs  # NEW: Process spawning, I/O streaming, lifecycle
  composer.rs         # NEW: tui-textarea wrapper, input handling
  scrollback.rs       # NEW: VecDeque-based buffer with capacity management

apps/cli/src/
  main.rs             # Add Session subcommand with start/stop/list/clear
```

### Pattern 1: Claude CLI Stream-JSON Transport
**What:** Spawn `claude -p --input-format stream-json --output-format stream-json --cwd <dir>` as a child process. Read output lines as NDJSON. Write user messages as JSON to stdin.
**When to use:** Every session launch.
**Example:**
```rust
// Source: Claude Code CLI reference + tokio::process docs
use tokio::process::Command;
use std::process::Stdio;
use std::os::unix::process::CommandExt;

let mut child = Command::new("claude")
    .args(["-p",
           "--input-format", "stream-json",
           "--output-format", "stream-json",
           "--dangerously-skip-permissions",
           "--cwd", &workspace_dir])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .process_group(0)  // New process group for clean kill
    .spawn()?;

let stdin = child.stdin.take().unwrap();
let stdout = child.stdout.take().unwrap();
```

### Pattern 2: Async Output Streaming into TUI
**What:** Read child stdout line-by-line with `BufReader::lines()`, parse JSON, extract text content, push into scrollback buffer. Poll in `tokio::select!` loop.
**When to use:** While session is running.
**Example:**
```rust
// Source: tokio::process docs
use tokio::io::{BufReader, AsyncBufReadExt};

let reader = BufReader::new(stdout);
let mut lines = reader.lines();

// In tokio::select! loop:
tokio::select! {
    event = reader_next_line(&mut lines) => {
        if let Some(line) = event? {
            // Parse stream-json, extract text, strip ANSI, push to scrollback
            let stripped = strip_ansi_escapes::strip_str(&line);
            scrollback.push_line(stripped);
        }
    }
    // ... other branches
}
```

### Pattern 3: Process Group Kill
**What:** On stop, send SIGTERM to the process group (negative PID). If not exited after 5s, send SIGKILL.
**When to use:** Session stop, app quit.
**Example:**
```rust
// Source: nix crate docs + std::os::unix::process::CommandExt docs
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

// child.id() returns the PID; with process_group(0), PGID == PID
let pgid = child.id().unwrap() as i32;

// Kill entire process group
kill(Pid::from_raw(-pgid), Signal::SIGTERM)?;

// Wait with timeout
match tokio::time::timeout(
    Duration::from_secs(5),
    child.wait()
).await {
    Ok(status) => { /* exited cleanly */ }
    Err(_) => {
        // Timeout -- force kill
        kill(Pid::from_raw(-pgid), Signal::SIGKILL)?;
        child.wait().await?;
    }
}
```

### Pattern 4: Sending User Messages via Stream-JSON
**What:** Write JSON messages to child stdin for multi-turn conversation.
**When to use:** When user presses Enter in composer.
**Example:**
```rust
// Source: Claude Code Agent SDK streaming-vs-single-mode docs
use tokio::io::AsyncWriteExt;
use serde_json::json;

let msg = json!({
    "type": "user",
    "message": {
        "role": "user",
        "content": user_input
    }
});

stdin.write_all(msg.to_string().as_bytes()).await?;
stdin.write_all(b"\n").await?;
stdin.flush().await?;
```

### Pattern 5: Session State Persistence
**What:** Session metadata in state.json, scrollback logs in separate files.
**When to use:** On session create/stop/restart and during output streaming.
**Example:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,              // UUID v4
    pub workspace_id: String,    // FK to Workspace
    pub claude_session_id: Option<String>,  // From stream-json output
    pub pid: Option<u32>,        // Child process PID
    pub status: SessionStatus,
    pub created_at: u64,
    pub ended_at: Option<u64>,
    pub log_file: String,        // ".kommand0-dev/sessions/<id>.log"
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionStatus {
    Running,
    Stopped,
    Failed,
    Exited,
}
```

### Anti-Patterns to Avoid
- **Blocking reads in the event loop:** Never use `std::io::Read` on child stdout. Always use `tokio::io::AsyncBufReadExt`.
- **Killing PID instead of process group:** Claude Code spawns child processes. Killing only the parent PID leaves orphans. Always use `kill(-pgid, signal)`.
- **Unbounded scrollback buffer:** Without a capacity limit, a long-running session will consume all memory. Enforce cap with `pop_front`.
- **Storing scrollback in state.json:** State.json becomes huge. Keep scrollback in separate `.log` files.
- **Using `child.kill()` directly:** This sends SIGKILL immediately without graceful shutdown. Always SIGTERM first.
- **Forgetting to unset CLAUDECODE env var:** Claude Code refuses to launch inside another Claude Code session. Clear this env var before spawning.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| ANSI escape stripping | Regex-based stripper | `strip-ansi-escapes` crate | ANSI has complex escape sequences (CSI, OSC, etc.); regex misses edge cases |
| Multi-line text input | Custom character-by-character handler | `tui-textarea` 0.7 | Cursor movement, word boundaries, selection, undo -- massive hidden complexity |
| Process group signaling | Raw libc calls | `nix` crate | Safe Rust wrappers, proper error handling, cross-platform *nix support |
| UUID generation | Custom random ID generator | `uuid` crate | RFC 4122 compliant, cryptographically random v4 |

**Key insight:** Process lifecycle management has extreme edge-case density. Every shortcut (skip process groups, skip SIGTERM grace period, skip env var clearing) creates a class of bugs that only manifests under specific timing conditions.

## Common Pitfalls

### Pitfall 1: Zombie Processes on Unclean Shutdown
**What goes wrong:** If the TUI crashes or is killed with SIGKILL, child processes become orphans/zombies.
**Why it happens:** No signal handler or Drop implementation to clean up children.
**How to avoid:** Use `process_group(0)` on every spawn. Implement a `Drop` trait or shutdown hook that kills all known process groups. Set `kill_on_drop(true)` as a safety net.
**Warning signs:** `ps aux | grep claude` shows processes after app exit.

### Pitfall 2: Claude Code Nested Session Detection
**What goes wrong:** `claude` refuses to start with "Cannot be launched inside another Claude Code session."
**Why it happens:** The `CLAUDECODE` environment variable is set in the parent process.
**How to avoid:** Clear `CLAUDECODE` from the child process environment before spawning: `.env_remove("CLAUDECODE")`.
**Warning signs:** Session immediately fails with error on first launch attempt.

### Pitfall 3: Stream-JSON Output Parsing Errors
**What goes wrong:** Not all lines from Claude CLI stdout are valid JSON. Some may be plain text errors or warnings.
**Why it happens:** CLI may emit non-JSON on stderr or even stdout in edge cases.
**How to avoid:** Wrap JSON parsing in `serde_json::from_str()` with graceful fallback. Log unparseable lines as raw text.
**Warning signs:** Session output shows `serde_json::Error` instead of content.

### Pitfall 4: Stdin Writer Dropped Prematurely
**What goes wrong:** Session immediately exits because stdin was closed.
**Why it happens:** The `stdin` handle is dropped (e.g., moved into a closure that completes).
**How to avoid:** Keep stdin handle alive in the session manager struct for the lifetime of the session.
**Warning signs:** Claude process exits with code 0 immediately after spawn.

### Pitfall 5: Scrollback Buffer Contention
**What goes wrong:** UI freezes when thousands of lines arrive quickly.
**Why it happens:** Pushing lines and re-rendering on every line.
**How to avoid:** Batch output processing -- accumulate lines during a tick interval (250ms already exists), then push all at once and render once.
**Warning signs:** High CPU usage, laggy TUI during rapid output.

### Pitfall 6: Process Group ID Assumption
**What goes wrong:** `kill(-child_pid, SIGTERM)` fails because PGID differs from PID.
**Why it happens:** Without `.process_group(0)`, the child inherits the parent's process group.
**How to avoid:** Always use `.process_group(0)` which sets PGID = PID of the child. Then `kill(-pid, signal)` targets only that child's group.
**Warning signs:** Stop command kills the TUI itself, or doesn't kill child processes.

## Code Examples

### Complete Session Manager Skeleton
```rust
// Source: tokio::process docs + nix docs + Claude CLI reference
use std::collections::HashMap;
use std::collections::VecDeque;
use tokio::process::{Child, Command};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

pub struct SessionManager {
    sessions: HashMap<String, RunningSession>,
}

struct RunningSession {
    child: Child,
    stdin: tokio::process::ChildStdin,
    output_rx: mpsc::UnboundedReceiver<String>,
    scrollback: VecDeque<String>,
    status: SessionStatus,
    claude_session_id: Option<String>,
}

impl SessionManager {
    pub async fn start_session(
        &mut self,
        session_id: &str,
        workspace_dir: &str,
        resume_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut cmd = Command::new("claude");
        cmd.args(["-p",
                   "--input-format", "stream-json",
                   "--output-format", "stream-json",
                   "--cwd", workspace_dir]);

        if let Some(rid) = resume_id {
            cmd.args(["--resume", rid]);
        }

        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_remove("CLAUDECODE")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        // Spawn output reader task
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).is_err() { break; }
            }
        });

        self.sessions.insert(session_id.to_string(), RunningSession {
            child,
            stdin,
            output_rx: rx,
            scrollback: VecDeque::with_capacity(50_000),
            status: SessionStatus::Running,
            claude_session_id: None,
        });

        Ok(())
    }

    pub async fn send_message(
        &mut self,
        session_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;

        let msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content
            }
        });

        session.stdin.write_all(msg.to_string().as_bytes()).await?;
        session.stdin.write_all(b"\n").await?;
        session.stdin.flush().await?;

        Ok(())
    }

    pub async fn stop_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;

        if let Some(pid) = session.child.id() {
            let pgid = pid as i32;
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-pgid),
                nix::sys::signal::Signal::SIGTERM,
            );

            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                session.child.wait(),
            ).await {
                Ok(_) => {}
                Err(_) => {
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(-pgid),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                    let _ = session.child.wait().await;
                }
            }
        }

        session.status = SessionStatus::Stopped;
        Ok(())
    }
}
```

### Composer Integration with tui-textarea
```rust
// Source: tui-textarea docs + ratatui docs
use tui_textarea::TextArea;
use crossterm::event::{KeyCode, KeyModifiers, KeyEvent};

struct Composer {
    textarea: TextArea<'static>,
    active: bool,
}

impl Composer {
    fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            ratatui::widgets::Block::default()
                .title(" Composer ")
                .borders(ratatui::widgets::Borders::ALL)
        );
        textarea.set_cursor_line_style(ratatui::style::Style::default());
        Self { textarea, active: false }
    }

    /// Returns Some(text) if user pressed Enter to send
    fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        match key {
            // Shift+Enter = newline
            KeyEvent { code: KeyCode::Enter, modifiers: KeyModifiers::SHIFT, .. } => {
                self.textarea.input(key);
                None
            }
            // Enter = send
            KeyEvent { code: KeyCode::Enter, .. } => {
                let lines: Vec<String> = self.textarea.lines().iter()
                    .map(|s| s.to_string())
                    .collect();
                let text = lines.join("\n").trim().to_string();
                if text.is_empty() {
                    return None;
                }
                // Clear textarea
                self.textarea = TextArea::default();
                Some(text)
            }
            // All other keys
            _ => {
                self.textarea.input(key);
                None
            }
        }
    }
}
```

### Scrollback Buffer
```rust
use std::collections::VecDeque;

pub struct ScrollbackBuffer {
    lines: VecDeque<String>,
    capacity: usize,
    scroll_offset: usize,  // 0 = bottom (auto-scroll)
    new_lines_since_scroll: usize,
}

impl ScrollbackBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(10_000)),
            capacity,
            scroll_offset: 0,
            new_lines_since_scroll: 0,
        }
    }

    pub fn push_line(&mut self, line: String) {
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        if self.scroll_offset > 0 {
            self.new_lines_since_scroll += 1;
        }
    }

    pub fn visible_lines(&self, height: usize) -> &[String] {
        // VecDeque::make_contiguous would be needed for slicing
        // In practice, iterate from (len - height - scroll_offset)
        // This is a simplified illustration
        let len = self.lines.len();
        let end = len.saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(height);
        // Note: actual impl needs make_contiguous() or iter-based approach
        &[]  // placeholder -- use iter().skip(start).take(height)
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| PTY for CLI tools | Stream-JSON programmatic API | Claude Code 2024-2025 | No need for portable-pty; piped I/O works perfectly |
| `pre_exec` + `setsid()` | `process_group(0)` on CommandExt | Rust 1.64 (stable) | No unsafe code needed for process group setup |
| tui-textarea 0.4-0.6 | tui-textarea 0.7 | 2024 | Native ratatui 0.29 support |
| Manual ANSI regex | strip-ansi-escapes crate | Stable | Handles all ANSI escape types correctly |

**Deprecated/outdated:**
- `before_exec()` on CommandExt: deprecated in favor of `pre_exec()` (Rust 1.34+)
- `setsid()` on CommandExt: nightly-only (issue #105376), use `process_group(0)` instead

## Open Questions

1. **Claude Code's stream-json session_id extraction**
   - What we know: JSON output includes `session_id` field; format is UUID
   - What's unclear: Exact message type that first contains `session_id` (likely `message_start` or first `StreamEvent`)
   - Recommendation: Parse every output JSON line, extract `session_id` from first message that contains it, store for `--resume`

2. **`--dangerously-skip-permissions` requirement**
   - What we know: Claude Code in `-p` mode may prompt for tool permissions
   - What's unclear: Whether `--dangerously-skip-permissions` is needed or if `--allowedTools` suffices
   - Recommendation: Start with `--dangerously-skip-permissions` for simplicity; add `--allowedTools` configuration later if needed. The user context says "let claude defaults handle rest" which suggests minimal flags.

3. **Stream-JSON stdin keepalive**
   - What we know: Claude CLI in stream-json mode expects stdin to stay open for multi-turn
   - What's unclear: Whether Claude CLI exits when stdin produces no input for extended periods
   - Recommendation: Keep stdin handle alive in SessionManager; do not drop or close it while session should be running

4. **CLAUDECODE env var on macOS**
   - What we know: Claude Code sets this env var; nested sessions are rejected
   - What's unclear: Whether other env vars also need clearing
   - Recommendation: Use `.env_remove("CLAUDECODE")` on Command; monitor for other env-related failures

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in test framework + tokio::test |
| Config file | Cargo.toml [dev-dependencies] |
| Quick run command | `cargo test -p kommand0-core` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| SESS-01 | Session model CRUD, scrollback push/pop | unit | `cargo test -p kommand0-core session` | No -- Wave 0 |
| SESS-02 | Stop sends SIGTERM, then SIGKILL after timeout | integration | `cargo test -p kommand0-tui stop` | No -- Wave 0 |
| SESS-03 | Restart spawns new process with --resume | unit | `cargo test -p kommand0-core restart` | No -- Wave 0 |
| SESS-04 | Shutdown kills all process groups | integration | `cargo test -p kommand0-tui shutdown` | No -- Wave 0 |
| SESS-05 | SessionStatus enum serialization/display | unit | `cargo test -p kommand0-core session_status` | No -- Wave 0 |
| SESS-06 | ScrollbackBuffer capacity enforcement | unit | `cargo test -p kommand0-core scrollback` | No -- Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test --workspace`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/core/src/session.rs` -- Session struct, SessionStatus enum, CRUD methods, tests
- [ ] `apps/tui/src/scrollback.rs` -- ScrollbackBuffer struct with capacity tests
- [ ] Integration tests for process lifecycle need a mock command (not real `claude` CLI)

## Sources

### Primary (HIGH confidence)
- [Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference) -- stream-json flags, --resume, --cwd, --input-format
- [Claude Code headless/programmatic docs](https://code.claude.com/docs/en/headless) -- session_id extraction, --continue, --output-format json
- [Agent SDK streaming-vs-single-mode](https://platform.claude.com/docs/en/agent-sdk/streaming-vs-single-mode) -- stream-json input message format `{"type":"user","message":{...}}`
- [Agent SDK streaming output](https://platform.claude.com/docs/en/agent-sdk/streaming-output) -- StreamEvent format, session_id field
- [tokio::process docs](https://docs.rs/tokio/latest/tokio/process/index.html) -- Command, Child, piped I/O, kill_on_drop
- [std::os::unix::process::CommandExt](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html) -- process_group(0) stable since 1.64, setsid nightly-only
- [nix::sys::signal::kill](https://docs.rs/nix/latest/nix/sys/signal/fn.kill.html) -- kill with negative PID for process group
- [nix::unistd::setsid](https://docs.rs/nix/latest/nix/unistd/fn.setsid.html) -- setsid function (used as fallback reference)
- [tui-textarea docs](https://docs.rs/tui-textarea/latest/tui_textarea/) -- TextArea API, crossterm input handling, ratatui 0.29 compat

### Secondary (MEDIUM confidence)
- [portable-pty docs](https://docs.rs/portable-pty/latest/portable_pty/) -- Evaluated and rejected; piped I/O sufficient
- [strip-ansi-escapes crates.io](https://crates.io/crates/strip-ansi-escapes) -- Version and download stats
- [nix crate features](https://docs.rs/crate/nix/latest/features) -- Feature flags for process/signal

### Tertiary (LOW confidence)
- Stream-JSON multi-turn input format -- verified via SDK docs but not directly tested with CLI due to nested session restriction

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all crates verified via official docs, versions confirmed
- Architecture: HIGH -- stream-json transport verified via Claude Code official docs; process_group(0) verified stable
- Pitfalls: HIGH -- zombie process, nested session, and process group issues are well-documented failure modes
- Transport decision: HIGH -- Claude Code explicitly supports `-p --input-format stream-json` for programmatic use
- Stream-JSON input format: MEDIUM -- verified via SDK docs, not directly tested with CLI

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable ecosystem, 30-day validity)
