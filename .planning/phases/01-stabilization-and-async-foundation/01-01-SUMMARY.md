---
phase: 01-stabilization-and-async-foundation
plan: 01
subsystem: testing
tags: [rust, tempfile, tdd, unit-tests, anyhow]

requires: []
provides:
  - Testable AppState with load_from/save_to accepting custom base paths
  - 8 unit tests covering persistence roundtrip and git status edge cases
  - Workspace deps for async migration (futures, crossterm event-stream)
  - Accurate README with build/run/test instructions
affects: [01-02-async-migration]

tech-stack:
  added: [tempfile, futures]
  patterns: [base-path injection for filesystem isolation, pre-validation before process spawning]

key-files:
  created: []
  modified:
    - Cargo.toml
    - crates/core/Cargo.toml
    - crates/core/src/lib.rs
    - README.md

key-decisions:
  - "Removed dead state_file() helper after refactoring load/save to delegate through load_from/save_to"
  - "Added futures and crossterm event-stream to workspace deps now to avoid churn in Plan 02"

patterns-established:
  - "Base-path injection: load_from/save_to/add_repo_with_base accept Path for test isolation via TempDir"
  - "Pre-validation pattern: check path exists and is_dir before spawning external processes"

requirements-completed: [STAB-01, STAB-02, STAB-03, STAB-04, STAB-05]

duration: 2min
completed: 2026-03-07
---

# Phase 1 Plan 01: Core Testability Summary

**Testable AppState with load_from/save_to base-path injection, 8 unit tests for persistence and git status edge cases, workspace deps prepared for async migration**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-07T10:11:47Z
- **Completed:** 2026-03-07T10:14:17Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments
- AppState now has load_from/save_to public methods accepting a base path for filesystem-isolated testing
- 8 unit tests covering load/save roundtrip, add_repo validation, and run_git_status edge cases
- run_git_status validates path existence and directory-ness before spawning git
- Workspace Cargo.toml updated with tempfile, futures, and crossterm event-stream feature for Plan 02
- README updated with accurate prerequisites, build, run, and test instructions

## Task Commits

Each task was committed atomically:

1. **Task 1: Make AppState testable and add unit tests** - `a648ac3` (test: RED), `3ee1c71` (feat: GREEN+REFACTOR)
2. **Task 2: Update README with accurate instructions** - `e05d202` (docs)

_Note: Task 1 used TDD with separate RED and GREEN+REFACTOR commits._

## Files Created/Modified
- `Cargo.toml` - Added tempfile, futures workspace deps; crossterm event-stream feature
- `crates/core/Cargo.toml` - Added tempfile dev-dependency
- `crates/core/src/lib.rs` - Added load_from/save_to/add_repo_with_base, pre-validation in run_git_status, 8 unit tests
- `README.md` - Accurate build/run/test instructions with prerequisites

## Decisions Made
- Removed dead `state_file()` helper after refactoring load/save to use load_from/save_to
- Added futures and crossterm event-stream to workspace deps proactively for Plan 02 async migration
- Naming review confirmed existing identifiers across core/cli/tui are consistent (no fixes needed)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Core crate is tested and ready for async migration in Plan 02
- Workspace deps (futures, crossterm event-stream) already in place for Plan 02
- Test baseline established to verify nothing breaks during async refactor

## Self-Check: PASSED

All files exist. All commit hashes verified (a648ac3, 3ee1c71, e05d202).

---
*Phase: 01-stabilization-and-async-foundation*
*Completed: 2026-03-07*
