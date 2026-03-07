# Feature Landscape

**Domain:** Terminal process orchestrator / workspace manager for parallel coding sessions
**Researched:** 2026-03-07
**Comparable tools:** mprocs, process-compose, procmux, zellij, tmux, just, devbox

## Table Stakes

Features users expect from a keyboard-driven terminal process orchestrator. Missing any of these and the tool feels broken or unusable.

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Start a command in a workspace | Core value proposition -- running processes is the whole point | Medium | Requires async (tokio) for non-blocking execution |
| Stream live stdout/stderr output | Every comparable tool (mprocs, procmux, process-compose) shows live output per process | Medium | Needs line-buffered streaming into a scrollable buffer |
| Stop a running process | Users must be able to halt runaway or finished processes | Low | Send SIGTERM, escalate to SIGKILL after timeout |
| Restart a stopped/failed process | mprocs, process-compose all support this; manual restart is minimum | Low | Re-run same command in same workspace context |
| Process status indicators | Visual distinction between running/stopped/failed/exited states | Low | Color-coded status in process list (green/red/yellow) |
| Clean shutdown on quit | All child processes must die when the orchestrator exits | Medium | Process group management, SIGTERM cascade, SIGKILL fallback. Critical -- orphaned processes are a deal-breaker |
| Keyboard-first navigation | Target user is keyboard-oriented; defined in PROJECT.md | Low | j/k or arrow navigation, Enter to select, consistent bindings |
| Pane layout (list + output) | Every TUI process manager uses split-pane: sidebar list + output area | Low | Already partially implemented in current TUI |
| Scrollback in output pane | Users need to scroll through command output history | Medium | Configurable scrollback buffer (1000+ lines default, like mprocs) |
| Workspace persistence | Workspaces must survive app restart; saved to state file | Low | Serialize workspace config to JSON state |
| Help overlay / key hints | Zellij popularized discoverability via status bar; mprocs shows keymap | Low | Show available keys for current context |

## Differentiators

Features that set Kommand0 apart from generic process managers. Not expected by default, but create competitive advantage for the target use case (parallel coding sessions).

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| Repo-aware workspaces | Workspaces are tied to git repos, not just arbitrary directories. Shows git status, branch info contextually | Low | Already have repo registry; workspace extends it |
| Git worktree integration | Create isolated working directories per workspace for parallel branch work. Killer feature for AI coding agent workflows in 2025-2026 | High | `git worktree add/remove/list` orchestration. Deferred to later milestone per PROJECT.md |
| Session templates / presets | Define reusable command sets per workspace (e.g., "start dev server + watch tests + tail logs") | Medium | Similar to mprocs.yaml but workspace-scoped. Could be part of workspace config |
| Workspace-scoped environment | Each workspace gets its own env vars, working directory, PATH | Medium | process-compose and mprocs both support per-process env. Essential for multi-repo setups |
| Multi-session workspace view | Run multiple commands in one workspace and switch between their outputs | Medium | mprocs core UX pattern. Workspace becomes a container for multiple sessions |
| Auto-restart policies | Configurable restart behavior: never, on-failure, always | Low | process-compose supports this with backoff. Useful for dev servers that crash |
| Session command history | Remember what commands were run in each workspace for quick re-execution | Low | Persist last N commands per workspace in state |
| Focused/zoomed output | Expand a single session's output to full screen, hiding the sidebar | Low | mprocs supports zoom. Useful when debugging verbose output |
| Copy mode | Enter a mode to select and copy text from output buffer | Medium | mprocs and tmux both have this. Essential for copying error messages |

## Anti-Features

Features to explicitly NOT build. These are tempting but wrong for Kommand0's scope.

| Anti-Feature | Why Avoid | What to Do Instead |
|--------------|-----------|-------------------|
| Full terminal emulator / PTY | Massive complexity (VT100 parsing, cursor positioning, interactive shells). mprocs and zellij invest enormous effort here. Not needed for command output streaming | Capture stdout/stderr as text streams. Support simple ANSI color codes for readability but don't emulate a full terminal |
| Plugin / extension system | Premature abstraction. process-compose and zellij have plugins but they're mature projects. Adds API surface area before the core is stable | Keep the codebase simple. Add features directly. Revisit only if there's clear demand |
| Remote execution | Breaks the "local-only, fast, simple" philosophy. SSH + remote process management is a different product (ansible, salt) | Local processes only. Users can run SSH commands as local processes if needed |
| Process dependency graphs | process-compose's dependency DAG is powerful but adds significant complexity (cycle detection, ordered startup, health checks) | Start processes independently. If users need ordering, they can use shell scripts or `just` for sequencing |
| Health checks / readiness probes | Kubernetes-style liveness/readiness is enterprise complexity. Overkill for a dev tool | Process is "healthy" if its PID is alive. Exit code tells the story |
| REST API / remote control | mprocs has TCP remote control, process-compose has a full REST API. Adds attack surface and complexity for a local tool | CLI commands for automation (e.g., `kmd session start workspace-name "command"`) |
| Scheduled / cron processes | process-compose supports this. Not relevant for interactive coding sessions | Out of scope. Users have cron/launchd for scheduled tasks |
| Config file format (YAML/TOML) | mprocs uses mprocs.yaml, process-compose uses YAML. Kommand0 uses JSON state. Adding a separate config format creates two sources of truth | Persist everything in JSON state file. CLI commands to configure workspaces. Possibly add import from mprocs.yaml later |
| Windows support | macOS-first per PROJECT.md constraints. Cross-platform signal handling and process management is a significant burden | macOS (and Linux as a bonus since POSIX signals work). Windows is a different project |
| AI/LLM integration | TUICommander and Superset target AI agent orchestration. Tempting but premature and couples to fast-moving APIs | Keep Kommand0 agent-agnostic. It orchestrates processes -- those processes can be AI agents, but Kommand0 doesn't need to know |
| Multiplayer / collaboration | Zellij supports multiplayer terminal sharing. Different product category | Single-user, local tool |

## Feature Dependencies

```
Repo Registry (exists) --> Workspace Model --> Workspace Persistence
                                           --> Workspace CRUD in TUI
                                           --> Workspace CRUD in CLI

Workspace Model --> Session Execution --> Live Output Streaming
                                      --> Process Status Tracking
                                      --> Stop/Restart Session
                                      --> Clean Shutdown

Live Output Streaming --> Scrollback Buffer --> Copy Mode
                                            --> Zoomed View

Session Execution --> Auto-restart Policies
                  --> Session Templates

Workspace Model --> Git Worktree Integration (deferred)
                --> Workspace-scoped Environment
```

Key dependency chain: **Workspace Model is the gateway.** Nothing meaningful happens without it. Session execution is second -- it unlocks the entire right side of the graph.

## MVP Recommendation

### Phase 1: Workspace Foundation
Prioritize:
1. **Workspace model and persistence** -- gate for everything else
2. **Workspace CRUD in TUI and CLI** -- users need to create/list/select workspaces
3. **Basic session execution** -- run a command, see output, know when it exits

### Phase 2: Process Lifecycle
Prioritize:
1. **Live output streaming** -- async stdout/stderr capture with scrollback
2. **Process status indicators** -- running/stopped/failed visual states
3. **Stop and restart** -- SIGTERM/SIGKILL and re-run
4. **Clean shutdown** -- all children die on quit (critical reliability feature)

### Phase 3: UX Polish
Prioritize:
1. **Help overlay** -- discoverability
2. **Zoomed output view** -- full-screen single session
3. **Multi-session per workspace** -- run multiple commands, switch between them
4. **Copy mode** -- select and copy from output

### Defer
- **Git worktree integration**: High complexity, requires workspace UX to be solid first
- **Session templates**: Nice-to-have once the manual workflow is proven
- **Auto-restart policies**: Add when users ask for it
- **Workspace-scoped environment**: Add when multi-repo usage patterns emerge

## Sources

- [mprocs - GitHub](https://github.com/pvolok/mprocs) -- Rust TUI process manager, closest comparable tool
- [process-compose - GitHub](https://github.com/F1bonacc1/process-compose) -- Go-based process orchestrator with advanced lifecycle management
- [process-compose docs](https://f1bonacc1.github.io/process-compose/) -- Dependency management, health checks, restart policies
- [procmux - GitHub](https://github.com/napisani/procmux) -- Python TUI for parallel commands with YAML config
- [zellij - GitHub](https://github.com/zellij-org/zellij) -- Rust terminal workspace with layouts, plugins, sessions
- [just - GitHub](https://github.com/casey/just) -- Command runner with task dependencies and parallel execution
- [Zellij about page](https://zellij.dev/about/) -- Workspace management philosophy
- [mprocs blog post](https://www.bitecode.dev/p/mprocs-start-all-your-projects-commands) -- Feature walkthrough
- [Git Worktrees for AI Agents - Nx Blog](https://nx.dev/blog/git-worktrees-ai-agents) -- Worktree workflow for parallel coding agents
- [Using Git Worktrees with AI Agents](https://www.nrmitchi.com/2025/10/using-git-worktrees-for-multi-feature-development-with-ai-agents/) -- Real-world worktree patterns
