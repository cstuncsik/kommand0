---
phase: 2
slug: workspace-model
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 2 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test framework (`#[cfg(test)]`) |
| **Config file** | None (cargo test uses Cargo.toml) |
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
| 02-01-01 | 01 | 1 | WORK-01 | unit | `cargo test -p kommand0-core -- workspace::tests::create` | No -- W0 | pending |
| 02-01-02 | 01 | 1 | WORK-01 | unit | `cargo test -p kommand0-core -- workspace::tests::create_missing_repo` | No -- W0 | pending |
| 02-01-03 | 01 | 1 | WORK-01 | unit | `cargo test -p kommand0-core -- workspace::tests::create_duplicate_name` | No -- W0 | pending |
| 02-01-04 | 01 | 1 | WORK-01 | unit | `cargo test -p kommand0-core -- workspace::tests::create_auto_name` | No -- W0 | pending |
| 02-01-05 | 01 | 1 | WORK-01 | unit | `cargo test -p kommand0-core -- workspace::tests::resolve_repo` | No -- W0 | pending |
| 02-02-01 | 01 | 1 | WORK-02 | unit | `cargo test -p kommand0-core -- workspace::tests::list_active` | No -- W0 | pending |
| 02-02-02 | 01 | 1 | WORK-02 | unit | `cargo test -p kommand0-core -- workspace::tests::list_all` | No -- W0 | pending |
| 02-02-03 | 01 | 1 | WORK-02 | unit | `cargo test -p kommand0-core -- workspace::tests::list_by_repo` | No -- W0 | pending |
| 02-03-01 | 02 | 2 | WORK-03 | manual | Visual: TUI tree renders repos+workspaces | N/A | pending |
| 02-04-01 | 01 | 1 | WORK-04 | unit | `cargo test -p kommand0-core -- workspace::tests::roundtrip` | No -- W0 | pending |
| 02-04-02 | 01 | 1 | WORK-04 | unit | `cargo test -p kommand0-core -- workspace::tests::backward_compat` | No -- W0 | pending |
| 02-05-01 | 02 | 2 | WORK-05 | manual | Visual: TUI shows repo->workspace hierarchy | N/A | pending |

*Status: pending / green / red / flaky*

---

## Wave 0 Requirements

- [ ] Workspace CRUD unit tests stubs in `crates/core/src/workspace.rs` (or tests module)
- [ ] Smart repo resolver test stubs
- [ ] Backward compatibility test (deserialize JSON without `workspaces` key)
- [ ] Archive/activate/delete test stubs
- [ ] `chrono` added to workspace dependencies for timestamp formatting

*Existing test infrastructure (cargo test, tempfile) covers framework needs.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| TUI tree view renders repos and workspaces | WORK-03 | Visual rendering requires human eye | 1. Add repos via CLI, 2. Create workspaces, 3. Launch TUI, 4. Verify tree with expand/collapse |
| TUI shows repo-workspace hierarchy | WORK-05 | Visual layout and styling check | 1. Expand repo in tree, 2. Verify indent + connector lines, 3. Check status dots, 4. Verify dimmed archived items |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 10s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
