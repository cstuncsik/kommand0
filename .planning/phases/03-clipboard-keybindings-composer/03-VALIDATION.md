---
phase: 3
slug: clipboard-keybindings-composer
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-28
---

# Phase 3 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust built-in) |
| **Config file** | Cargo.toml workspace config |
| **Quick run command** | `cargo test --package kommand0-tui` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test --package kommand0-tui`
- **After every plan wave:** Run `cargo test --workspace`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 03-01-01 | 01 | 1 | CLIP-01 | unit + manual | `cargo test --package kommand0-tui -- clipboard` | Partial (bridge exists) | ⬜ pending |
| 03-01-02 | 01 | 1 | CLIP-02 | unit | `cargo test --package kommand0-tui -- clipboard` | Partial | ⬜ pending |
| 03-01-03 | 01 | 1 | KEYS-01 | manual-only | N/A (requires running session + terminal) | N/A | ⬜ pending |
| 03-01-04 | 01 | 1 | KEYS-02 | manual-only | N/A (requires full app interaction) | N/A | ⬜ pending |
| 03-02-01 | 02 | 1 | COMP-01 | unit | `cargo test --package kommand0-tui -- composer` | ❌ W0 | ⬜ pending |
| 03-02-02 | 02 | 1 | COMP-02 | unit | `cargo test --package kommand0-tui -- composer` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `apps/tui/src/composer.rs` — add tests for `has_selection()`, `selected_text()`, `select_all()` methods
- [ ] Verify `selection_range()` col semantics with multi-byte text in a test

*Existing infrastructure covers clipboard bridge basics.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Ctrl+Q stops session | KEYS-01 | Requires running TUI session with active terminal | 1. Start app 2. Press Ctrl+Q 3. Verify session stops cleanly |
| Old Ctrl+C behavior removed | KEYS-02 | Requires full app interaction to verify no stop behavior | 1. Start app 2. With no selection, press Ctrl+C 3. Verify nothing happens 4. With selection, press Ctrl+C 5. Verify text copied to clipboard |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
