---
phase: 3
slug: session-execution
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 3 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test framework + tokio::test |
| **Config file** | Cargo.toml [dev-dependencies] |
| **Quick run command** | `cargo test -p kommand0-core` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~5 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test --workspace`
- **After every plan wave:** Run `cargo test --workspace`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 10 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 03-01-01 | 01 | 1 | SESS-01, SESS-05 | unit | `cargo test -p kommand0-core session` | No -- Wave 0 | pending |
| 03-01-02 | 01 | 1 | SESS-06 | unit | `cargo test -p kommand0-tui scrollback` | No -- Wave 0 | pending |
| 03-02-01 | 02 | 2 | SESS-01, SESS-02, SESS-04 | build | `cargo build -p kommand0-tui` | No -- Wave 0 | pending |
| 03-02-02 | 02 | 2 | SESS-01 | build | `cargo build -p kommand0-tui` | No -- Wave 0 | pending |
| 03-03-01 | 03 | 3 | SESS-01-05 | build+manual | `cargo build -p kommand0-tui` | No -- Wave 0 | pending |
| 03-03-02 | 03 | 3 | SESS-01-04 | build | `cargo build -p kommand0-cli` | No -- Wave 0 | pending |

*Status: pending / green / red / flaky*

---

## Wave 0 Requirements

- [ ] `crates/core/src/session.rs` -- Session struct, SessionStatus enum, CRUD methods with tests
- [ ] `apps/tui/src/scrollback.rs` -- ScrollbackBuffer struct with capacity tests
- [ ] Integration tests for process lifecycle need a mock command (not real `claude` CLI)

*Note: Wave 0 test stubs are created as part of Plan 01 tasks (TDD approach).*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Live output streaming in TUI | SESS-01 | Requires real `claude` CLI process and terminal rendering | Start session with 'r', send message, observe streaming output |
| Process cleanup on quit | SESS-04 | Requires checking OS process table after app exit | Quit with 'q', run `ps aux \| grep claude` to verify no orphans |
| Session restart with --resume | SESS-03 | Requires real Claude CLI with session continuity | Stop session, press 'R', verify resumed conversation context |
| Status indicators in tree view | SESS-05 | Visual verification of TUI rendering | Check tree view shows running/stopped/failed icons |
| Composer input handling | SESS-01 | Interactive keyboard input in terminal | Type in composer, test Enter vs Shift+Enter, test Ctrl+C clears |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 10s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
