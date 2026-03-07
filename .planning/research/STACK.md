# Technology Stack

**Project:** Kommand0 -- Async Process Management & Streaming Output Milestone
**Researched:** 2026-03-07

## Current Stack (Already in Place)

These are declared in the workspace Cargo.toml and should NOT be re-evaluated. Listed here for context only.

| Technology | Version | Purpose |
|------------|---------|---------|
| Rust | 2024 edition | Language |
| ratatui | 0.29 | TUI rendering |
| crossterm | 0.28 | Terminal backend |
| clap | 4 | CLI argument parsing |
| tokio | 1 (full features) | Async runtime (declared, not yet used) |
| serde / serde_json | 1 | State serialization |
| thiserror | 2 | Error types (declared, not yet used) |
| anyhow | 1 | Error propagation |
| tracing | 0.1 | Structured logging (declared, not yet used) |

**Note on ratatui/crossterm versions:** The latest ratatui is 0.30.0 and crossterm is 0.29.0. Upgrading is NOT recommended during this milestone -- ratatui 0.30 restructured into a workspace of sub-crates and has breaking changes (e.g., `Alignment` renamed to `HorizontalAlignment`, new backend trait requirements). The current 0.29/0.28 pairing works. Upgrade later as a standalone task.

## New Dependencies for This Milestone

### Process Management & Async I/O

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| `tokio` (already declared) | 1, features = ["full"] | Async runtime, process spawning, channels | Already in workspace deps. `tokio::process::Command` provides async process spawning with piped stdout/stderr. `tokio::io::BufReader` + `AsyncBufReadExt::lines()` gives streaming line-by-line output. `tokio::sync::mpsc` channels bridge between spawned processes and the TUI event loop. | HIGH |
| `crossterm` features = ["event-stream"] | 0.28 | Async keyboard event stream | Required to convert the current blocking `event::read()` loop to async. Provides `EventStream` that yields crossterm events as a `Stream`, which integrates with `tokio::select!`. Without this, the TUI blocks on input and cannot receive process output. | HIGH |
| `tokio-stream` | 0.1.18 | Stream utilities (StreamExt, wrappers) | Provides `StreamExt` trait for `.next()` on streams, plus `LinesStream` to convert `tokio::io::Lines` into a proper `Stream`. Needed to unify crossterm events and process output into `tokio::select!`. Small, maintained by the tokio team. | HIGH |

### Process Lifecycle & Cleanup

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| `nix` | 0.31, features = ["signal", "process"] | UNIX process group management | For reliable child process cleanup on quit. `killpg()` sends SIGTERM/SIGKILL to entire process groups, which handles the case where a spawned command itself spawns children (e.g., `cargo build` spawning `rustc`). `setsid()` / `setpgid()` at spawn time ensures children are in their own group. macOS-first so UNIX-only is fine. | HIGH |
| `shared_child` | 1.1.1 | Thread-safe child process handle | Wraps `std::process::Child` to allow `kill()` and `wait()` from multiple threads/tasks. Needed because the TUI event loop and process monitoring task both need access to the child handle. Alternative: raw `Arc<Mutex<Child>>`, but `shared_child` handles edge cases (race between wait and kill). | MEDIUM |

### Identity & Time

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| `uuid` | 1.22, features = ["v4"] | Unique IDs for workspaces/sessions | Current code uses timestamp-based hex IDs (`generate_id()`). For workspaces and sessions that persist and reference each other, proper UUIDs avoid collisions and are standard practice. Drop-in replacement for the current ID generation. | HIGH |
| `chrono` | 0.4.44 | Timestamps for session events | Session start/stop times, workspace creation dates. `SystemTime` works but chrono provides human-readable formatting and serde integration out of the box. | MEDIUM |

### Logging & Diagnostics

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| `tracing-subscriber` | 0.3.22, features = ["env-filter", "fmt"] | Log output to file | `tracing` is already declared but has no subscriber. For a TUI app, logs MUST go to a file (not stdout). `tracing-subscriber` with `fmt::layer().with_writer(file)` handles this. `env-filter` allows `RUST_LOG=debug` style filtering. Essential for debugging async process issues. | HIGH |
| `tracing-appender` | 0.2 | Non-blocking file logging | Companion to tracing-subscriber. Provides `non_blocking` writer so logging never blocks the TUI render loop. Writes to rolling log files in `.kommand0-dev/logs/`. | MEDIUM |

## What NOT to Add

| Technology | Why Not |
|------------|---------|
| `portable-pty` | Provides PTY emulation for programs that behave differently without a TTY. Kommand0 captures command output (git, cargo, scripts) -- these work fine with piped stdout/stderr. PTY adds complexity (raw byte streams, escape sequence handling) with no benefit for this use case. Only needed if building a full terminal emulator, which is explicitly out of scope. |
| `signal-hook` | Provides signal handling via iterators. For this project, `nix::sys::signal` is sufficient since we only need to send signals (SIGTERM/SIGKILL to children), not install custom signal handlers. Tokio itself handles SIGINT/SIGTERM for graceful shutdown via `tokio::signal`. |
| `async-process` (from async-std) | Duplicates `tokio::process`. Since we're already on tokio, adding async-std's process crate creates two runtime ecosystems. Use tokio's built-in process module. |
| `vt100` / `vte` / terminal parsers | Only needed for full terminal emulation (parsing ANSI escape sequences, maintaining a virtual screen buffer). Overkill -- line-by-line output capture is sufficient for the MVP. |
| `duct` | Nice API for shell pipelines but synchronous. `tokio::process::Command` does everything we need in async context. |
| `ctrlc` | Superseded by `tokio::signal::ctrl_c()` which integrates natively with the tokio event loop. |
| `tui-textarea` / `tui-input` | No text input needed in MVP. Commands are defined in workspace config, not typed interactively. |

## Architecture: How These Fit Together

### The Async Event Loop Pattern

The current synchronous loop (`event::read()` blocks) must become an async loop using `tokio::select!`:

```rust
// Pseudocode for the new main loop structure
loop {
    tokio::select! {
        // Branch 1: Keyboard/mouse events from crossterm
        Some(Ok(event)) = event_stream.next() => {
            handle_input(event, &mut app, &action_tx);
        }
        // Branch 2: Process output lines from running sessions
        Some(output) = process_rx.recv() => {
            app.append_output(output);
        }
        // Branch 3: Render tick (e.g., every 16ms for 60fps)
        _ = tick_interval.tick() => {
            terminal.draw(|f| ui::render(f, &app))?;
        }
    }
}
```

### Process Spawning Flow

```
User presses Enter on workspace
  -> action_tx.send(Action::StartSession { workspace_id })
  -> spawn tokio task:
       1. tokio::process::Command::new(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)  // own process group for cleanup
            .spawn()
       2. BufReader::new(stdout).lines() in a loop
       3. Send each line to process_rx channel
       4. On exit, send SessionExited status
```

### Cleanup on Quit

```
User presses 'q' or Ctrl+C
  -> For each running session:
       1. nix::sys::signal::killpg(pgid, Signal::SIGTERM)
       2. tokio::time::timeout(3 seconds, child.wait())
       3. If timeout: killpg(pgid, Signal::SIGKILL)
  -> Restore terminal
  -> Exit
```

## Updated Cargo.toml Changes

### workspace Cargo.toml additions

```toml
[workspace.dependencies]
# Add to existing:
crossterm = { version = "0.28", features = ["event-stream"] }  # update existing line
tokio-stream = "0.1"
nix = { version = "0.31", features = ["signal", "process"] }
shared_child = "1.1"
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
tracing-appender = "0.2"
```

### apps/tui/Cargo.toml additions

```toml
[dependencies]
# Add to existing:
tokio.workspace = true          # needs async runtime in TUI now
tokio-stream.workspace = true   # for EventStream + process output streams
tracing.workspace = true        # structured logging
tracing-subscriber.workspace = true
tracing-appender.workspace = true
nix.workspace = true            # process group cleanup
```

### crates/core/Cargo.toml additions

```toml
[dependencies]
# Add to existing:
uuid.workspace = true           # workspace/session IDs
chrono.workspace = true         # timestamps
shared_child.workspace = true   # thread-safe child handle
nix.workspace = true            # process group operations
tokio-stream.workspace = true   # stream utilities
```

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| Async process | `tokio::process` | `async-process` (smol/async-std) | Already committed to tokio. Two runtimes = pain. |
| Process cleanup | `nix::sys::signal::killpg` | `libc` directly | `nix` provides safe Rust wrappers. `libc` requires unsafe everywhere. |
| Child handle sharing | `shared_child` | `Arc<Mutex<Child>>` | `shared_child` handles wait/kill races. Worth the tiny dependency. |
| Async events | crossterm `event-stream` | polling in separate thread | `event-stream` is the canonical approach recommended by ratatui docs. Thread approach works but adds unnecessary complexity. |
| ID generation | `uuid` v4 | nanoid, ulid, timestamp hex | uuid is standard, well-known, serde support. Current hex timestamp has collision risk. |
| Timestamps | `chrono` | `time` crate | `chrono` has better serde integration and is more widely used in the Rust ecosystem. Either works. |
| File logging | `tracing-subscriber` + `tracing-appender` | `env_logger`, `fern` | Already using `tracing`. These are the official companion crates. |

## Sources

- [Ratatui async counter tutorial](https://ratatui.rs/tutorials/counter-async-app/) - Canonical async TUI pattern
- [Ratatui async event stream](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/) - crossterm event-stream integration
- [Ratatui best practices discussion](https://github.com/ratatui/ratatui/discussions/220) - Community patterns
- [Ratatui 0.30.0 release notes](https://ratatui.rs/highlights/v030/) - Breaking changes (reason to stay on 0.29)
- [Tokio process docs](https://docs.rs/tokio/latest/tokio/process/index.html) - Async process spawning
- [Tokio BufReader](https://docs.rs/tokio/latest/tokio/io/struct.BufReader.html) - Streaming line reads
- [Tokio channels](https://tokio.rs/tokio/tutorial/channels) - mpsc for action/event passing
- [nix killpg](https://docs.rs/nix/latest/nix/sys/signal/fn.kill.html) - Process group signals
- [shared_child crate](https://docs.rs/shared_child) - Thread-safe child process handle
- [PTY-based output capture (2025)](https://developerlife.com/2025/08/10/pty-rust-osc-seq/) - Reference for why PTY is overkill here
- [d-holguin/async-ratatui example](https://github.com/d-holguin/async-ratatui) - Community async ratatui pattern
