# Architecture Patterns

**Domain:** Rust terminal process orchestrator / TUI process manager
**Researched:** 2026-03-07

## Recommended Architecture

The system uses a **message-passing architecture** where an async tokio runtime manages process lifecycle and output streaming, communicating with a synchronous ratatui render loop via mpsc channels. This is the established pattern in the ratatui ecosystem for async TUI apps.

```
+-------------------+      mpsc channels       +--------------------+
|   Async Runtime   | -----------------------> |    TUI Event Loop  |
|   (tokio tasks)   | <----------------------- |    (main thread)   |
|                   |   AppEvent (output,      |                    |
|  - Process spawn  |    process state)        |  - Render frame    |
|  - Output stream  |                          |  - Handle keys     |
|  - Signal handler |   AppAction (start,      |  - Dispatch actions|
|                   |    stop, kill)           |                    |
+-------------------+                          +--------------------+
        |                                              |
        v                                              v
+-------------------+                          +--------------------+
|  Process Registry |                          |    App State       |
|  (Arc<Mutex<>>)   |                          |  (owned by loop)   |
|                   |                          |                    |
|  - Child handles  |                          |  - Workspaces      |
|  - Process groups |                          |  - Output buffers  |
|  - Exit statuses  |                          |  - Focus/selection |
+-------------------+                          +--------------------+
        |
        v
+-------------------+
|   State Store     |
|   (JSON file)     |
+-------------------+
```

### Component Boundaries

| Component | Responsibility | Communicates With | Location |
|-----------|---------------|-------------------|----------|
| **EventHandler** | Polls crossterm events + tick timer, sends to main loop | TUI Event Loop (via mpsc) | `apps/tui/` |
| **TUI Event Loop** | Receives events, dispatches actions, triggers renders | EventHandler, AppState, ProcessManager | `apps/tui/` |
| **AppState** | Owns all UI state: workspaces, selections, output buffers, focus | TUI Event Loop (direct ownership) | `apps/tui/` |
| **ProcessManager** | Spawns/kills child processes, streams output lines | TUI Event Loop (via mpsc channels) | `crates/core/` |
| **SessionRunner** | Wraps a single tokio::process::Child with stdout/stderr streaming | ProcessManager | `crates/core/` |
| **StateStore** | Persists workspaces/repos to JSON, loads on startup | AppState, CLI | `crates/core/` |
| **Workspace Model** | Domain types: Workspace, Session, ProcessStatus | All components | `crates/core/` |

### Data Flow

**Startup:**
```
JSON file --> StateStore::load() --> AppState { repos, workspaces }
                                        |
                                        v
                                   TUI Event Loop starts
                                        |
                                        v
                                   EventHandler spawned (tokio task)
```

**User starts a session:**
```
KeyEvent(Enter) --> TUI Event Loop
    |
    v
AppAction::StartSession { workspace_id, command }
    |
    v
ProcessManager::spawn(command, working_dir)
    |
    v
tokio::process::Command::new(cmd)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .process_group(0)        // own process group for clean kill
    .kill_on_drop(true)      // safety net
    .spawn()
    |
    v
tokio::spawn(async move {
    // Two tasks: one for stdout, one for stderr
    // BufReader::new(stdout).lines()
    // Send each line via mpsc as AppEvent::Output { session_id, line }
})
```

**Output streaming (continuous):**
```
Child stdout/stderr --> BufReader.lines() --> AppEvent::Output { id, line, stream }
    |
    v  (via mpsc sender)
TUI Event Loop receives AppEvent::Output
    |
    v
AppState.sessions[id].output_buffer.push(line)
    |
    v
Next render frame shows updated output
```

**User stops a session:**
```
KeyEvent('x') --> AppAction::StopSession { session_id }
    |
    v
ProcessManager::stop(session_id)
    |
    v
1. Send SIGTERM to process group (-pgid)
2. tokio::time::timeout(5s, child.wait())
3. If timeout: SIGKILL
4. Send AppEvent::ProcessExited { id, status }
```

**App quit:**
```
KeyEvent('q') --> AppAction::Quit
    |
    v
ProcessManager::shutdown_all()
    |
    v
For each running session:
    1. SIGTERM to process group
    2. Brief wait
    3. SIGKILL if needed
    |
    v
Terminal cleanup (disable raw mode, leave alternate screen)
```

## Key Architecture Decisions

### 1. Two-channel message passing (not shared mutable state)

Use two mpsc channels:
- **action_tx/action_rx**: TUI loop sends actions to process manager (`StartSession`, `StopSession`, `Quit`)
- **event_tx/event_rx**: Process manager and event handler send events to TUI loop (`Output`, `ProcessExited`, `Key`, `Tick`)

The TUI loop owns AppState exclusively (no Arc/Mutex needed for UI state). This avoids deadlocks and keeps rendering fast.

```rust
// In main loop
loop {
    // Non-blocking: drain all pending events
    while let Ok(event) = event_rx.try_recv() {
        match event {
            AppEvent::Key(key) => handle_key(&mut app, key, &action_tx),
            AppEvent::Output { session_id, line, stream } => {
                app.append_output(session_id, line, stream);
            }
            AppEvent::ProcessExited { session_id, status } => {
                app.mark_exited(session_id, status);
            }
            AppEvent::Tick => {} // just triggers redraw
        }
    }
    terminal.draw(|f| ui(f, &app))?;
}
```

### 2. Process groups for clean cleanup

Every spawned child uses `.process_group(0)` to create its own process group. This is critical because:
- The child might spawn grandchildren (e.g., `npm run dev` spawns node)
- Killing just the child PID leaves orphaned grandchildren
- Killing the process group (`kill(-pgid, SIGTERM)`) cleans up the entire tree

```rust
use std::os::unix::process::CommandExt;

let mut cmd = tokio::process::Command::new("sh");
cmd.args(["-c", &command_string]);
cmd.current_dir(&working_dir);
cmd.stdout(Stdio::piped());
cmd.stderr(Stdio::piped());
unsafe { cmd.pre_exec(|| { libc::setpgid(0, 0); Ok(()) }) };
// Or use .process_group(0) which does the same
```

### 3. Output buffer with bounded ring buffer

Each session maintains a ring buffer (e.g., last 10,000 lines) rather than unbounded Vec. This prevents memory issues from long-running processes with heavy output.

```rust
pub struct OutputBuffer {
    lines: VecDeque<OutputLine>,
    capacity: usize,
}

pub struct OutputLine {
    pub text: String,
    pub stream: OutputStream, // Stdout or Stderr
    pub timestamp: Instant,
}
```

### 4. Focus-based pane navigation

The TUI has multiple panes (repos, workspaces, output). A simple enum tracks which pane has focus, and key handlers dispatch based on focus:

```rust
enum Focus {
    RepoList,
    WorkspaceList,
    OutputPane,
}

// Key handling routes based on focus
fn handle_key(app: &mut App, key: KeyEvent, action_tx: &Sender<AppAction>) {
    match key.code {
        // Global keys (always active)
        KeyCode::Char('q') => action_tx.send(AppAction::Quit),
        KeyCode::Tab => app.cycle_focus(),

        // Focus-specific keys
        _ => match app.focus {
            Focus::RepoList => handle_repo_keys(app, key),
            Focus::WorkspaceList => handle_workspace_keys(app, key, action_tx),
            Focus::OutputPane => handle_output_keys(app, key),
        }
    }
}
```

## Core Type Definitions

```rust
// --- Domain types (crates/core) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub repo_id: String,
    pub working_dir: String,
    pub default_command: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SessionStatus {
    Idle,
    Running { pid: u32 },
    Exited { code: Option<i32> },
    Failed { error: String },
}

// --- Event/Action enums (apps/tui or crates/core) ---

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Tick,
    Output { session_id: String, line: String, stream: OutputStream },
    ProcessExited { session_id: String, status: ExitStatus },
    Error { session_id: String, error: String },
}

#[derive(Debug)]
pub enum AppAction {
    StartSession { workspace_id: String, command: String },
    StopSession { session_id: String },
    RestartSession { session_id: String },
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub enum OutputStream {
    Stdout,
    Stderr,
}
```

## Patterns to Follow

### Pattern 1: Async Event Handler (crossterm + tick)

Spawn a dedicated tokio task for terminal event polling. This decouples input handling from rendering.

```rust
pub struct EventHandler {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    task: JoinHandle<()>,
}

impl EventHandler {
    pub fn new(event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        let tx = event_tx.clone();
        let task = tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();
            let mut tick = tokio::time::interval(Duration::from_millis(250));

            loop {
                let crossterm_event = reader.next().fuse();
                let tick_delay = tick.tick();

                tokio::select! {
                    maybe_event = crossterm_event => {
                        if let Some(Ok(Event::Key(key))) = maybe_event {
                            if key.kind == KeyEventKind::Press {
                                let _ = tx.send(AppEvent::Key(key));
                            }
                        }
                    }
                    _ = tick_delay => {
                        let _ = tx.send(AppEvent::Tick);
                    }
                }
            }
        });

        Self { event_tx, task }
    }
}
```

### Pattern 2: Session Runner (output streaming task)

Each running session gets its own tokio task that streams output lines back via the shared event channel.

```rust
pub async fn run_session(
    session_id: String,
    command: String,
    working_dir: PathBuf,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<tokio::process::Child> {
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", &command])
        .current_dir(&working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let sid = session_id.clone();

    // Stdout reader task
    let tx1 = event_tx.clone();
    let sid1 = sid.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx1.send(AppEvent::Output {
                session_id: sid1.clone(),
                line,
                stream: OutputStream::Stdout,
            });
        }
    });

    // Stderr reader task
    let tx2 = event_tx.clone();
    let sid2 = sid.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx2.send(AppEvent::Output {
                session_id: sid2.clone(),
                line,
                stream: OutputStream::Stderr,
            });
        }
    });

    // Wait for exit in background
    let tx3 = event_tx.clone();
    let sid3 = session_id;
    // Return child handle for kill/stop capability
    // Caller stores the child handle in ProcessManager

    Ok(child)
}
```

### Pattern 3: Graceful shutdown with process group kill

```rust
pub async fn stop_session(child: &mut tokio::process::Child) -> Result<ExitStatus> {
    let pid = child.id().expect("child has pid");

    // Send SIGTERM to process group
    unsafe { libc::kill(-(pid as i32), libc::SIGTERM); }

    // Wait with timeout
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        _ => {
            // Force kill if timeout
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
            child.wait().await.map_err(Into::into)
        }
    }
}
```

## Anti-Patterns to Avoid

### Anti-Pattern 1: Blocking the render loop
**What:** Calling `std::process::Command` synchronously in the TUI event loop (as the current codebase does with `run_git_status`).
**Why bad:** Freezes the entire UI until the process completes. No output streaming possible. User cannot interact during execution.
**Instead:** Spawn all processes via `tokio::process::Command` in background tasks. Stream output via mpsc channels.

### Anti-Pattern 2: Arc<Mutex<AppState>> shared between tasks
**What:** Wrapping all app state in Arc<Mutex<>> so multiple tasks can read/write it.
**Why bad:** Risk of deadlocks. Mutex contention on every render frame. Harder to reason about state transitions.
**Instead:** The main loop owns AppState exclusively. Background tasks communicate via channels only. No shared mutable state.

### Anti-Pattern 3: Killing only the child PID
**What:** Using `child.kill()` which only sends SIGKILL to the direct child.
**Why bad:** Commands like `npm run dev` or `cargo watch` spawn subprocesses. Killing only the parent leaves orphaned zombie processes consuming resources and holding ports.
**Instead:** Always use process groups (`.process_group(0)`) and kill the entire group with `kill(-pgid, signal)`.

### Anti-Pattern 4: Unbounded output buffers
**What:** Pushing every output line into a `Vec<String>` without limits.
**Why bad:** A process outputting thousands of lines per second (e.g., verbose build logs) will consume unbounded memory.
**Instead:** Use a `VecDeque` with a capacity limit. Drop oldest lines when full.

### Anti-Pattern 5: Polling for events with sleep
**What:** Using `crossterm::event::poll(Duration)` in a synchronous loop with sleeps.
**Why bad:** Either wastes CPU (short sleep) or feels laggy (long sleep). Cannot integrate with async process output.
**Instead:** Use `crossterm::event::EventStream` with `tokio::select!` for zero-waste event multiplexing.

## Module Layout

```
crates/core/src/
    lib.rs              -- re-exports
    state.rs            -- StateStore (JSON persistence)
    repo.rs             -- RepoEntry, repo operations
    workspace.rs        -- Workspace model, CRUD
    session.rs          -- SessionRunner, output streaming
    process.rs          -- ProcessManager, spawn/stop/kill
    types.rs            -- Shared types (OutputLine, OutputStream, etc.)

apps/tui/src/
    main.rs             -- tokio::main, setup, teardown
    app.rs              -- AppState, focus management, state transitions
    event.rs            -- EventHandler (crossterm + tick)
    action.rs           -- AppEvent, AppAction enums
    ui/
        mod.rs          -- top-level layout
        repo_pane.rs    -- repo list rendering
        workspace_pane.rs -- workspace list rendering
        output_pane.rs  -- session output rendering
        help.rs         -- help overlay
    handler.rs          -- key event dispatch (global + focus-specific)

apps/cli/src/
    main.rs             -- CLI commands (unchanged, uses core)
```

## Suggested Build Order

Dependencies between components dictate this order:

1. **Domain types first** (`workspace.rs`, `types.rs`) -- Everything depends on these. No external dependencies.
2. **State persistence** (`state.rs` expanded) -- Workspaces need to persist. Extend existing JSON store.
3. **Async event loop migration** (`event.rs`, `action.rs`, `main.rs`) -- Convert TUI from sync `event::read()` to async `EventStream` + `tokio::select!`. This is the foundational infrastructure change.
4. **Focus/pane management** (`app.rs`, `handler.rs`) -- Multi-pane navigation. Depends on async event loop being in place.
5. **Process spawning** (`session.rs`, `process.rs`) -- Spawn commands, stream output. Depends on async runtime and event channels.
6. **Output rendering** (`output_pane.rs`) -- Display streaming output in the TUI. Depends on process spawning sending output events.
7. **Process lifecycle** (stop, restart, cleanup) -- Kill/restart sessions, cleanup on quit. Depends on process spawning working.
8. **Help overlay and polish** (`help.rs`) -- Last, depends on final keybindings being settled.

**Critical path:** Steps 1-3 must be sequential. Steps 4-5 can partially overlap. Steps 6-7 depend on 5.

## Scalability Considerations

| Concern | 1-3 sessions | 10-20 sessions | 50+ sessions |
|---------|-------------|----------------|-------------|
| Output buffering | Vec is fine | Ring buffer needed | Ring buffer + lazy rendering |
| Render performance | Redraw all | Redraw all (still fast) | Only redraw visible pane |
| Process cleanup | Manual iterate | Map lookup by ID | Same (map scales fine) |
| Channel throughput | Unbounded fine | Unbounded fine | Consider bounded + backpressure |
| State persistence | Save on change | Debounce saves | Debounce + background save |

For Kommand0's target use case (experienced developer, handful of parallel sessions), the 1-3 and 10-20 columns are the realistic operating range. Design for 10-20, do not over-engineer for 50+.

## Sources

- [Ratatui Async Event Stream Tutorial](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/) -- HIGH confidence, official docs
- [Ratatui Async Template Structure](https://ratatui.github.io/async-template/02-structure.html) -- HIGH confidence, official template
- [Ratatui Component Architecture](https://ratatui.rs/concepts/application-patterns/component-architecture/) -- HIGH confidence, official docs
- [tokio::process module docs](https://docs.rs/tokio/latest/tokio/process/index.html) -- HIGH confidence, official docs
- [tokio::process::Command docs](https://docs.rs/tokio/latest/tokio/process/struct.Command.html) -- HIGH confidence, official docs
- [Tokio process group / kill_on_drop behavior](https://github.com/tokio-rs/tokio/issues/2504) -- MEDIUM confidence, issue discussion
- [Ratatui async-template GitHub](https://github.com/ratatui/async-template) -- HIGH confidence, official repo
