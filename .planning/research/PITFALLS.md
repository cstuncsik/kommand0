# Domain Pitfalls

**Domain:** Rust terminal process orchestrator / TUI process manager
**Project:** Kommand0
**Researched:** 2026-03-07

## Critical Pitfalls

Mistakes that cause rewrites, data loss, or unusable terminal state.

### Pitfall 1: Terminal State Corruption on Panic or Crash

**What goes wrong:** If the app panics after `enable_raw_mode()` and `EnterAlternateScreen`, the terminal is left in raw mode. The user's shell becomes unusable -- no echo, no line editing, no Ctrl+C. They must `reset` the terminal manually.

**Why it happens:** The current code (`apps/tui/src/main.rs`) calls `disable_raw_mode()` and `LeaveAlternateScreen` at the end of `main()`. Any panic between `enable_raw_mode()` and that cleanup skips restoration. Rust's default panic behavior unwinds the stack but does not guarantee the cleanup lines at the end of `main()` run.

**Consequences:** Corrupted terminal on any panic anywhere in the app. As the app grows with async tasks, process spawning, and more complex state, panics become more likely. Users lose trust in the tool fast.

**Prevention:**
1. Install a panic hook that restores terminal state BEFORE the default panic handler:
```rust
let default_panic = std::panic::take_hook();
std::panic::set_hook(Box::new(move |info| {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
    default_panic(info);
}));
```
2. Additionally, wrap terminal setup/teardown in a RAII guard (a struct implementing `Drop`) so teardown runs on normal exit AND unwind.
3. Consider `color_eyre` or custom hook that formats panic backtraces nicely even after leaving raw mode.

**Detection:** Test by adding `panic!("test")` in the event loop. If your terminal breaks, the guard is missing.

**Phase:** Stabilization (M1). Must be done before any process management code is added.

---

### Pitfall 2: Zombie Processes and Orphaned Process Trees

**What goes wrong:** When the orchestrator spawns a child process (e.g., `cargo build`), that child may itself spawn grandchildren. Killing the direct child with `SIGKILL` does NOT kill the grandchildren. They become orphans reparented to PID 1 and keep running. The user quits Kommand0, but rogue `rustc` processes keep consuming CPU.

**Why it happens:** Unix process semantics: `kill(pid)` only signals that specific PID. Child processes that spawn their own subprocesses create a process tree, but `Child::kill()` in both `std::process` and `tokio::process` only sends SIGKILL to the direct child. Rust's standard library explicitly states it does not automatically wait on child processes, not even when `Child` is dropped.

**Consequences:** Resource leaks, port conflicts (zombie processes holding sockets), confused users ("why is my CPU at 100% after I quit?"), potential data corruption from orphaned writes.

**Prevention:**
1. Spawn each session's command in its own **process group** using `pre_exec` + `setsid()` or `.process_group(0)` (available on `Command` on nightly / via `CommandExt`):
```rust
use std::os::unix::process::CommandExt;
let mut cmd = tokio::process::Command::new("bash");
cmd.args(["-c", &user_command]);
cmd.process_group(0); // child becomes its own process group leader
```
2. When stopping a session, kill the **entire process group** with `killpg`:
```rust
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
killpg(Pid::from_raw(child_pid), Signal::SIGTERM)?;
// After timeout, escalate:
killpg(Pid::from_raw(child_pid), Signal::SIGKILL)?;
```
3. On app exit (including panic -- see Pitfall 1), iterate ALL tracked sessions and kill their process groups.
4. Always `wait()` / `try_wait()` on children after killing to reap them and prevent zombie entries in the process table.
5. Consider the `process-wrap` crate for `KillOnDrop` semantics and session management, or `kill_tree` for recursive process tree termination.

**Detection:** After stopping a session, run `ps aux | grep <command>` to verify no orphans remain. Automate this in integration tests.

**Phase:** Session execution (M3). This is THE core correctness requirement for a process orchestrator.

---

### Pitfall 3: Blocking the TUI Render Loop with Synchronous Work

**What goes wrong:** The current event loop calls `event::read()` which blocks the thread until a key is pressed. Once async process management is added, you need the loop to simultaneously: render UI, poll keyboard events, receive streaming process output, and handle tick timers. A blocking `event::read()` prevents all of this.

**Why it happens:** The current architecture is synchronous and single-threaded. `event::read()` is blocking. `run_git_status()` uses `std::process::Command::output()` which blocks until completion. This works for the current vertical slice but fundamentally cannot support streaming output.

**Consequences:** UI freezes while commands run. No streaming output -- user sees nothing until the command completes. Cannot update multiple panes simultaneously. The app feels broken.

**Prevention:**
1. Migrate the event loop to `tokio::select!` with multiple branches:
```rust
loop {
    tokio::select! {
        Some(event) = event_rx.recv() => { /* handle key/mouse */ }
        Some(output) = output_rx.recv() => { /* append to buffer */ }
        _ = tick_interval.tick() => { /* trigger render */ }
        _ = cancellation_token.cancelled() => { break; }
    }
}
```
2. Use `crossterm::event::EventStream` (requires `event-stream` feature on crossterm) to get async key events instead of blocking `event::read()`.
3. Spawn process output readers as separate `tokio::spawn` tasks that send lines through `mpsc` channels.
4. Keep rendering in the main task (ratatui's `Terminal` is not `Send`), but feed it data through channels.

**Detection:** If adding `tokio::time::sleep(Duration::from_secs(5)).await` in the loop body causes the entire UI to freeze for 5 seconds, the architecture is wrong.

**Phase:** Session execution (M3), but the event loop restructuring should be planned in stabilization (M1) to avoid a rewrite later. At minimum, the M1 refactor should move to the async event pattern even before process management exists.

---

### Pitfall 4: Stdout/Stderr Interleaving and Ordering

**What goes wrong:** When reading both stdout and stderr from a child process using separate async readers, the output arrives in non-deterministic order. A compile error on stderr may appear BEFORE the stdout line that triggered it, or vice versa. The user sees jumbled output that does not match what they would see in a normal terminal.

**Why it happens:** Stdout and stderr are separate file descriptors with separate kernel buffers. Tokio reads them with separate futures. There is no way to reconstruct the original ordering without a PTY. The tokio issue tracker explicitly documents this as a known limitation: "stdout/stderr not read in the order they are produced by underlying process."

**Consequences:** Confusing output display. Users cannot trust the output ordering in Kommand0 and fall back to running commands in a regular terminal, defeating the purpose.

**Prevention:**
1. **Merge stdout and stderr at the source** by redirecting stderr to stdout on the `Command`:
```rust
cmd.stderr(std::process::Stdio::piped()); // OR:
cmd.stderr(std::process::Stdio::from(stdout_pipe)); // merge into stdout
```
The simplest approach: `cmd.args(["2>&1"])` if wrapping in `bash -c`.
2. If you need to distinguish stdout from stderr (e.g., color errors red), accept that ordering will be approximate. Read both in separate tasks but timestamp each line and merge-sort in the display buffer.
3. Do NOT attempt to build a full PTY layer (marked out of scope in PROJECT.md). The redirect approach is sufficient for a process orchestrator.

**Detection:** Run a command that produces interleaved stdout/stderr (e.g., `cargo build` with warnings) and compare output ordering with a normal terminal.

**Phase:** Session execution (M3).

---

### Pitfall 5: Unbounded Output Buffer Memory Growth

**What goes wrong:** A long-running process (e.g., `cargo watch`, `npm run dev`) produces output continuously. If every line is appended to a `String` or `Vec<String>` without limit, memory grows without bound. A process that runs for hours can consume gigabytes of RAM.

**Why it happens:** It is natural to model output as `Vec<String>` and push every line. The current code stores output in a single `String` field. Without explicit limits, this grows forever.

**Consequences:** OOM kills, system slowdown, app crash after extended sessions.

**Prevention:**
1. Use a **ring buffer** (circular buffer) with a configurable maximum line count (e.g., 10,000 lines). The `VecDeque` from std works:
```rust
const MAX_LINES: usize = 10_000;
struct OutputBuffer {
    lines: VecDeque<String>,
}
impl OutputBuffer {
    fn push(&mut self, line: String) {
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}
```
2. Consider storing only the visible viewport + a bounded scrollback, similar to how terminal emulators work.
3. For very long output, consider writing to a temp file and memory-mapping the tail for display.

**Detection:** Run `yes | head -1000000` as a session command and monitor the app's RSS with `ps`.

**Phase:** Session execution (M3). Design the output buffer data structure before implementing streaming output.

## Moderate Pitfalls

### Pitfall 6: Graceless Shutdown -- SIGKILL Without SIGTERM

**What goes wrong:** Using `child.kill()` sends SIGKILL immediately, which cannot be caught by the child process. The child has no chance to flush buffers, save state, or clean up temporary files.

**Prevention:**
1. Always send SIGTERM first, wait up to N seconds (e.g., 3-5s), then escalate to SIGKILL:
```rust
killpg(pid, Signal::SIGTERM)?;
tokio::time::sleep(Duration::from_secs(3)).await;
match child.try_wait() {
    Ok(Some(_)) => { /* exited cleanly */ }
    _ => { killpg(pid, Signal::SIGKILL)?; }
}
```
2. Make the timeout configurable per-workspace or globally.
3. Show a visual indicator in the TUI ("Stopping..." -> "Force killing...") so the user knows what is happening.

**Phase:** Session execution (M3).

---

### Pitfall 7: Channel Backpressure -- Output Producer Overwhelms Consumer

**What goes wrong:** A fast-output process (e.g., `find /`) sends thousands of lines per second through an `mpsc` channel. If using `unbounded_channel`, memory grows. If using bounded channel, the producer task blocks and can cause the child process to stall (stdout buffer fills, process blocks on write).

**Prevention:**
1. Use a **bounded** channel with a reasonable capacity (e.g., 1000 lines).
2. When the channel is full, **drop old lines** rather than blocking the producer. Implement a "lossy" send that discards and increments a "dropped lines" counter displayed in the UI.
3. Alternatively, batch lines: accumulate for 16ms (one frame at 60fps) then send a batch, reducing channel pressure.
4. Rate-limit rendering: do not re-render on every single line. Render on a tick interval (e.g., every 50-100ms) and just accumulate lines in the buffer between renders.

**Phase:** Session execution (M3).

---

### Pitfall 8: State File Corruption with Concurrent Access

**What goes wrong:** If the CLI (`kmd repo add`) and TUI (`kommand0-tui`) run simultaneously and both read/modify/write `state.json`, one overwrites the other's changes. The current `AppState::save()` does a full overwrite with no locking.

**Prevention:**
1. Use file locking (`flock` / `fcntl` via the `fs2` crate or `fd-lock`):
```rust
use fs2::FileExt;
let file = File::open(state_path)?;
file.lock_exclusive()?; // blocks until lock acquired
// read, modify, write
file.unlock()?;
```
2. Alternatively, adopt a write-ahead pattern: read, modify in memory, write to a `.tmp` file, then atomically rename.
3. For the TUI specifically, hold state in memory and only persist on explicit save operations, not on every mutation.

**Phase:** Stabilization (M1) or workspace management (M2). Address before workspaces add more frequent state writes.

---

### Pitfall 9: Async Runtime Misconfiguration

**What goes wrong:** Using `#[tokio::main]` with default settings creates a multi-threaded runtime. Ratatui's `Terminal` is not `Send`, so it cannot be shared across tokio tasks. Attempting to render from a spawned task causes compile errors or requires `unsafe`. Alternatively, using `block_on` inside an async context causes panics.

**Prevention:**
1. Keep `Terminal` ownership in the main task. Never move it into a `tokio::spawn`.
2. Use `#[tokio::main]` (multi-threaded is fine) but ensure the render loop stays on the main task.
3. All spawned tasks communicate back to the main task via channels -- they never touch the terminal directly.
4. Never call `block_on()` from within an async context. If you need sync code, use `tokio::task::spawn_blocking`.

**Phase:** Session execution (M3), but plan the architecture in M1/M2.

---

### Pitfall 10: Missing Cancellation Token for Clean Task Shutdown

**What goes wrong:** When a user stops a session or quits the app, spawned tokio tasks (output readers, process watchers) keep running because nothing signals them to stop. The app hangs on exit waiting for tasks that will never complete, or tasks panic because shared state was dropped.

**Prevention:**
1. Use `tokio_util::sync::CancellationToken` for every spawned task:
```rust
let token = CancellationToken::new();
let child_token = token.child_token();
tokio::spawn(async move {
    loop {
        tokio::select! {
            line = reader.next_line() => { /* process */ }
            _ = child_token.cancelled() => { break; }
        }
    }
});
// To stop:
token.cancel();
```
2. Create a hierarchy: app-level token -> session-level tokens (children of app token). Cancelling the app token cascades to all sessions.
3. Use `JoinHandle` to await task completion after cancellation, with a timeout to prevent hanging.

**Phase:** Session execution (M3).

## Minor Pitfalls

### Pitfall 11: Event Key Handling Conflicts Across Panes

**What goes wrong:** As the TUI adds more panes (repo list, workspace list, output viewer), key bindings collide. `j`/`k` should navigate the focused pane, but without a focus model, they affect the wrong pane or trigger actions in multiple panes simultaneously.

**Prevention:**
1. Implement explicit focus state tracking: `enum Focus { RepoList, WorkspaceList, OutputPane }`.
2. Route key events through a dispatcher that checks focus before forwarding to pane-specific handlers.
3. Use Tab/Shift+Tab or a consistent key (e.g., Ctrl+W then arrow) for pane switching, documented in a help overlay.

**Phase:** UX polish (M4), but the focus enum should be introduced in M2 when the workspace pane is added.

---

### Pitfall 12: Scrolling in Output Pane Without Viewport Management

**What goes wrong:** Ratatui's `Paragraph` widget renders from the top. When output exceeds the visible area, the user sees only the first N lines with no way to scroll. Adding `.scroll()` offset without managing it creates confusing behavior (auto-scroll vs. manual scroll conflict).

**Prevention:**
1. Default to "follow mode" (auto-scroll to bottom, like `tail -f`).
2. When user scrolls up, switch to "frozen mode" and stop auto-scrolling. Show an indicator ("[+42 new lines]") at the bottom.
3. When user presses a key (e.g., `G` or `End`), jump back to follow mode.
4. Track viewport offset as `scroll_offset: usize` in the output pane state. Calculate it relative to `buffer.len() - viewport_height`.

**Phase:** Session execution (M3) for basic scrolling, UX polish (M4) for follow/frozen mode.

---

### Pitfall 13: Hardcoded Terminal Size Assumptions

**What goes wrong:** Layout calculations assume a minimum terminal size. If the user's terminal is very small (e.g., 40x10), widgets overlap, text truncates badly, or the app panics on underflow when computing layout math.

**Prevention:**
1. Check `terminal.size()` and show a "terminal too small" message if below a minimum (e.g., 80x24).
2. Use ratatui's `Constraint::Min` and `Constraint::Max` to create responsive layouts.
3. Handle terminal resize events (`Event::Resize`) to trigger re-layout.

**Phase:** UX polish (M4).

## Phase-Specific Warnings

| Phase Topic | Likely Pitfall | Mitigation |
|-------------|---------------|------------|
| M1: Stabilization | Terminal state corruption (Pitfall 1) | Install panic hook and RAII guard immediately |
| M1: Stabilization | State file corruption (Pitfall 8) | Add file locking before more writers exist |
| M2: Workspace management | Key handling conflicts (Pitfall 11) | Introduce focus enum with the second pane |
| M3: Session execution | Zombie processes (Pitfall 2) | Process groups + killpg from day one |
| M3: Session execution | Blocking render loop (Pitfall 3) | Async event loop with tokio::select! |
| M3: Session execution | Output ordering (Pitfall 4) | Merge stderr into stdout |
| M3: Session execution | Memory growth (Pitfall 5) | Ring buffer design before first streaming impl |
| M3: Session execution | Ungraceful shutdown (Pitfall 6) | SIGTERM-then-SIGKILL pattern |
| M3: Session execution | Channel backpressure (Pitfall 7) | Bounded channels with lossy send |
| M3: Session execution | Runtime misconfiguration (Pitfall 9) | Keep Terminal on main task |
| M3: Session execution | Task leaks (Pitfall 10) | CancellationToken hierarchy |
| M4: UX polish | Scroll management (Pitfall 12) | Follow/frozen mode toggle |
| M4: UX polish | Small terminals (Pitfall 13) | Min size check + responsive constraints |

## Sources

- [Ratatui Async Event Stream Tutorial](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/)
- [Ratatui Component Architecture](https://ratatui.rs/concepts/application-patterns/component-architecture/)
- [Ratatui FAQ](https://ratatui.rs/faq/)
- [Crossterm Raw Mode Issue #368](https://github.com/crossterm-rs/crossterm/issues/368)
- [Ratatui Panic Handler Issue #1005](https://github.com/ratatui/ratatui/issues/1005)
- [Tokio Child Graceful Cleanup Issue #2504](https://github.com/tokio-rs/tokio/issues/2504)
- [Rust std::process::Child docs](https://doc.rust-lang.org/std/process/struct.Child.html)
- [Rust Child::kill does not kill children Issue #115241](https://github.com/rust-lang/rust/issues/115241)
- [Tokio stdout/stderr ordering Issue #1386](https://github.com/tokio-rs/tokio/issues/1386)
- [process-wrap crate](https://crates.io/crates/process-wrap/6.0.0)
- [kill_tree crate](https://crates.io/crates/kill_tree)
- [nix crate killpg](https://docs.rs/nix/latest/nix/sys/signal/fn.killpg.html)
- [Pretty Rust backtraces in raw terminal mode](https://werat.dev/blog/pretty-rust-backtraces-in-raw-terminal-mode/)
- [Tokio process module docs](https://docs.rs/tokio/latest/tokio/process/index.html)
- [Ratatui forum: tokio::spawn vs spawn_blocking](https://forum.ratatui.rs/t/understanding-tokio-spawn-and-tokio-spawn-blocking/74)
