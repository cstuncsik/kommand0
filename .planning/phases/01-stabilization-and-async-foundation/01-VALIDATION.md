---
phase: 1
slug: stabilization-and-async-foundation
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 1 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in `#[test]` with `cargo test` |
| **Config file** | none — uses default Cargo test runner |
| **Quick run command** | `cargo test -p kommand0-core` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~5 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p kommand0-core`
- **After every plan wave:** Run `cargo test --workspace`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 10 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 01-01-01 | 01 | 1 | STAB-01 | manual | `cargo check --workspace` | N/A | ⬜ pending |
| 01-01-02 | 01 | 1 | STAB-02 | manual | `cargo check --workspace` | N/A | ⬜ pending |
| 01-02-01 | 02 | 1 | STAB-03 | unit | `cargo test -p kommand0-core` | ❌ W0 | ⬜ pending |
| 01-02-02 | 02 | 1 | STAB-05 | unit | `cargo test -p kommand0-core` | ❌ W0 | ⬜ pending |
| 01-03-01 | 03 | 2 | STAB-07 | manual | `cargo run -p kommand0-tui` | N/A | ⬜ pending |
| 01-03-02 | 03 | 2 | STAB-06 | manual | `cargo run -p kommand0-tui` (then force panic) | N/A | ⬜ pending |
| 01-01-03 | 01 | 1 | STAB-04 | manual | Follow README instructions | N/A | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/core/src/lib.rs` — add `#[cfg(test)] mod tests` with test stubs for STAB-03, STAB-05
- [ ] Add `tempfile` to workspace dev-dependencies for test isolation

*Existing `cargo test` infrastructure covers the test runner.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Naming consistency | STAB-01, STAB-02 | Code review, not testable | Review all public types and function names across crates |
| Async event loop | STAB-07 | TUI requires terminal | Run TUI, press keys, verify responsive rendering |
| Panic recovery | STAB-06 | Requires terminal state | Run TUI, trigger panic, verify terminal restores |
| README accuracy | STAB-04 | Human verification | Follow README build/run/test instructions from scratch |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 10s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
