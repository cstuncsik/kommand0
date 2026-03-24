---
phase: 2
slug: output-pane-selection
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-24
---

# Phase 2 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust built-in) |
| **Config file** | Cargo.toml workspace |
| **Quick run command** | `cargo test -p kommand0-tui` |
| **Full suite command** | `cargo test` |
| **Estimated runtime** | ~10 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p kommand0-tui`
- **After every plan wave:** Run `cargo test`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 10 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 02-01-01 | 01 | 1 | OSEL-04 | unit | `cargo test -p kommand0-tui -- overlay_style` | ❌ W0 | ⬜ pending |
| 02-01-02 | 01 | 1 | CURS-01 | unit | `cargo test -p kommand0-tui -- word_boundary` | ❌ W0 | ⬜ pending |
| 02-01-03 | 01 | 1 | CURS-02 | unit | `cargo test -p kommand0-tui -- cursor_auto_scroll` | ❌ W0 | ⬜ pending |
| 02-01-04 | 01 | 1 | OSEL-06 | unit | `cargo test -p kommand0-tui -- selection_clear` | ❌ W0 | ⬜ pending |
| 02-02-01 | 02 | 2 | CURS-01 | manual | N/A | N/A | ⬜ pending |
| 02-02-02 | 02 | 2 | CURS-02 | manual | N/A | N/A | ⬜ pending |
| 02-02-03 | 02 | 2 | OSEL-01 | manual | N/A | N/A | ⬜ pending |
| 02-02-04 | 02 | 2 | OSEL-02 | manual | N/A | N/A | ⬜ pending |
| 02-02-05 | 02 | 2 | OSEL-05 | manual | N/A | N/A | ⬜ pending |
| 02-03-01 | 03 | 2 | OSEL-03 | manual | N/A | N/A | ⬜ pending |
| 02-03-02 | 03 | 2 | OSEL-04 | manual | N/A | N/A | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `apps/tui/src/render.rs` — unit tests for `overlay_style_on_line` span splitting
- [ ] `apps/tui/src/render.rs` — unit tests for word boundary detection (Ctrl+arrow)
- [ ] `apps/tui/src/render.rs` — unit tests for cursor auto-scroll calculation
- [ ] `apps/tui/src/selection.rs` — unit tests for selection-clear-on-scroll behavior

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Cursor movement with arrow keys | CURS-01 | Requires TUI interaction | Focus output pane, press arrow keys, verify cursor moves by visual rows |
| Cursor auto-scroll | CURS-02 | Requires viewport interaction | Move cursor beyond visible area, verify viewport scrolls to reveal cursor |
| Shift+arrow selection | OSEL-01 | Requires modifier key interaction | Hold Shift+arrows, verify cyan highlight extends character-by-character |
| Shift+Home/End selection | OSEL-02 | Requires modifier key interaction | Press Shift+Home, verify selection to document start; Shift+End to end |
| Mouse drag selection | OSEL-03 | Requires mouse interaction | Click and drag across text, verify cyan highlight follows in real time |
| Ctrl+A select all | OSEL-05 | Requires modifier key interaction | Focus output, press Ctrl+A, verify all text highlighted cyan |
| Selection clears on scroll | OSEL-06 | Requires interaction sequence | Select text, then scroll manually, verify selection clears |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 10s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
