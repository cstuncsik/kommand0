---
phase: 1
slug: coordinate-translation-infrastructure
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-23
---

# Phase 1 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust built-in) |
| **Config file** | Cargo.toml (workspace) |
| **Quick run command** | `cargo test -p kommand0-tui` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~10 seconds |

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
| 1-01-01 | 01 | 1 | CORD-01 | unit | `cargo test -p kommand0-tui wrap_map` | ❌ W0 | ⬜ pending |
| 1-01-02 | 01 | 1 | CORD-02 | unit | `cargo test -p kommand0-tui wrap_map::tests::cjk` | ❌ W0 | ⬜ pending |
| 1-01-03 | 01 | 1 | CORD-03 | unit | `cargo test -p kommand0-tui wrap_map::tests::scroll` | ❌ W0 | ⬜ pending |
| 1-02-01 | 02 | 2 | CLIP-03 | unit | `cargo test -p kommand0-tui clipboard` | ❌ W0 | ⬜ pending |
| 1-02-02 | 02 | 2 | CORD-01 | integration | `cargo test -p kommand0-tui scrollback::tests::extract` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `apps/tui/src/wrap_map.rs` — module with test stubs for CORD-01, CORD-02, CORD-03
- [ ] `apps/tui/src/selection.rs` — module with test stubs for SelectionState
- [ ] `apps/tui/src/clipboard.rs` — module with test stubs for CLIP-03

*Existing test infrastructure (cargo test) covers all phase requirements.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| arboard clipboard writes on target platform | CLIP-03 | Requires system clipboard access | Run TUI, call ClipboardBridge::copy(), paste in another app |
| WrapMap matches ratatui visual rendering | CORD-01 | Visual correctness | Run TUI with CJK/emoji text, verify no misalignment |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
