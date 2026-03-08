---
phase: 4
slug: ux-polish
status: draft
nyquist_compliant: true
wave_0_complete: true
created: 2026-03-08
---

# Phase 4 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (built-in Rust test framework) |
| **Config file** | Cargo.toml (workspace) |
| **Quick run command** | `cargo test -p kommand0-tui` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p kommand0-tui`
- **After every plan wave:** Run `cargo test --workspace`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Requirement | Test Type | Automated Command | Status |
|---------|------|-------------|-----------|-------------------|--------|
| 04-01-01 | 01 | UX-01 | unit | `cargo test -p kommand0-tui -- scrollback` | ⬜ pending |
| 04-01-02 | 01 | UX-01, UX-03 | build | `cargo build -p kommand0-tui` | ⬜ pending |
| 04-02-01 | 02 | UX-01, UX-03 | build | `cargo build -p kommand0-tui` | ⬜ pending |
| 04-02-02 | 02 | UX-03 | build | `cargo build -p kommand0-tui` | ⬜ pending |
| 04-03-01 | 03 | UX-02 | build + unit | `cargo build -p kommand0-tui` | ⬜ pending |
| 04-03-02 | 03 | UX-04 | build | `cargo build -p kommand0-tui` | ⬜ pending |
| 04-03-03 | 03 | UX-01..04 | manual | Plan 03 checkpoint: human-verify (17 steps) | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Rationale — No Separate Wave 0 Plan Needed

This phase is primarily **visual/TUI rendering work** where the meaningful feedback signal is `cargo build` (type-checks compile) plus the human-verify checkpoint in Plan 03 Task 3. A separate Wave 0 test-stub plan would add overhead without meaningful coverage because:

1. **ScrollbackBuffer tests (04-01-01):** Already folded into Plan 01 Task 1 which adds unit tests for `scroll_to_top`, `total_lines`, and `clamped_offset` directly alongside the implementation. This satisfies the Nyquist requirement — automated test feedback exists before subsequent tasks consume these methods.

2. **Key dispatch / focus cycle / zoom toggle:** These are async event-loop behaviors tightly coupled to terminal I/O (`crossterm::event::read`, `ratatui::Frame`). Extracting them into pure testable functions would require a major refactor (mocking the terminal, session manager, scrollback state) that exceeds the phase scope. The `cargo build` compilation check catches type errors, missing fields, and signature mismatches. The Plan 03 human-verify checkpoint (17 steps) provides the behavioral verification.

3. **Help overlay content (04-03-01):** Plan 03 Task 1 creates help.rs with structured `KeyBinding`/`KeySection` data types. The executor can add a simple content test (e.g., assert all sections have non-empty bindings) as part of Task 1 implementation. No Wave 0 stub needed.

**Sampling continuity:** No 3 consecutive tasks lack automated feedback — every task has at minimum `cargo build` which catches compile errors within 15 seconds, and Plan 01 Task 1 and Plan 03 Task 1 include unit tests.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Enter on workspace starts/resumes session + focuses composer | UX-01 | Requires live terminal + session manager | 1. Select workspace 2. Press Enter 3. Verify session starts and composer focused |
| Shift+Enter in Composer inserts newline (not sends) | UX-03 | Terminal modifier detection varies by emulator | 1. Focus composer 2. Type text 3. Shift+Enter 4. Verify newline inserted, not sent |
| Zoom renders full-screen output+composer+status | UX-04 | Visual terminal rendering verification | 1. Focus output 2. Press z 3. Verify full-screen layout 4. Press z again to restore |
| Help overlay renders centered with correct keys | UX-02 | Visual verification of overlay positioning | 1. Press ? 2. Verify centered overlay 3. Verify keys match current focus context |
| Chat bubbles right-align user messages | UX-03 | Visual alignment verification | 1. Send message 2. Verify right-aligned with background 3. Verify Claude output left-aligned |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify commands (cargo test or cargo build)
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 rationale documented (no separate plan needed)
- [x] No watch-mode flags
- [x] Feedback latency < 15s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
