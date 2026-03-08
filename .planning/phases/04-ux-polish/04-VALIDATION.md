---
phase: 4
slug: ux-polish
status: draft
nyquist_compliant: false
wave_0_complete: false
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

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 04-01-01 | 01 | 0 | UX-01 | unit | `cargo test -p kommand0-tui -- key_dispatch` | ❌ W0 | ⬜ pending |
| 04-01-02 | 01 | 0 | UX-01 | unit | `cargo test -p kommand0-tui -- scroll_to` | ❌ W0 | ⬜ pending |
| 04-01-03 | 01 | 0 | UX-02 | unit | `cargo test -p kommand0-tui -- help_content` | ❌ W0 | ⬜ pending |
| 04-01-04 | 01 | 0 | UX-03 | unit | `cargo test -p kommand0-tui -- focus_cycle` | ❌ W0 | ⬜ pending |
| 04-01-05 | 01 | 0 | UX-04 | unit | `cargo test -p kommand0-tui -- zoom_toggle` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `apps/tui/src/scrollback.rs` — extend existing tests for `scroll_to_top()`, `total_lines()`, `clamped_offset()`
- [ ] Extract key dispatch logic from async `run()` into testable `App` methods for key_dispatch and focus_cycle tests
- [ ] Help overlay module (`help.rs`) must expose key definitions as data, not just rendering, for help_content tests

*Wave 0 creates test stubs and extracts testable interfaces before feature work begins.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Enter on workspace starts/resumes session + focuses composer | UX-01 | Requires live terminal + session manager | 1. Select workspace 2. Press Enter 3. Verify session starts and composer focused |
| Zoom renders full-screen output+composer+status | UX-04 | Visual terminal rendering verification | 1. Focus output 2. Press zoom key 3. Verify full-screen layout 4. Press again to restore |
| Help overlay renders centered with correct keys | UX-02 | Visual verification of overlay positioning | 1. Press ? 2. Verify centered overlay 3. Verify keys match current focus context |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
