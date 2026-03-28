# Roadmap: TUI Text Selection & Clipboard

## Overview

This milestone adds text selection and clipboard copy to the kommand0 TUI. The work progresses from building the coordinate translation foundation (the hardest, highest-risk piece), through output pane selection with visual feedback, to clipboard integration and keybinding changes that complete the feature. Composer selection rides last because tui-textarea handles the hard parts.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [ ] **Phase 1: Coordinate Translation & Infrastructure** - Build WrapMap, SelectionState, and ClipboardBridge -- the foundation everything depends on
- [ ] **Phase 2: Output Pane Selection** - Cursor navigation, mouse drag, keyboard selection, and highlight rendering in the output pane
- [ ] **Phase 3: Clipboard, Keybindings & Composer** - Wire copy to system clipboard, rewire Ctrl+C/Q, and enable composer selection

## Phase Details

### Phase 1: Coordinate Translation & Infrastructure
**Goal**: The coordinate translation and data structures needed for selection exist, are tested, and handle edge cases (wrapping, unicode, CJK, emoji)
**Depends on**: Nothing (first phase)
**Requirements**: CORD-01, CORD-02, CORD-03, CLIP-03
**Success Criteria** (what must be TRUE):
  1. Screen position (x,y) correctly maps to logical text position (line, char) for plain ASCII, CJK, and emoji text in the output pane
  2. Coordinate translation accounts for border padding, scroll offset, and line wrapping
  3. arboard clipboard can be initialized and write text on the target platform without error
  4. SelectionState can represent no-selection, cursor-only, and anchor+cursor range states
**Plans:** 2 plans

Plans:
- [ ] 01-01-PLAN.md — WrapMap coordinate translation + SelectionState (TDD)
- [ ] 01-02-PLAN.md — ClipboardBridge + display-width bug fix

### Phase 2: Output Pane Selection
**Goal**: Users can see a cursor, navigate it, and select text in the output pane with visual feedback
**Depends on**: Phase 1
**Requirements**: CURS-01, CURS-02, OSEL-01, OSEL-02, OSEL-03, OSEL-04, OSEL-05, OSEL-06
**Success Criteria** (what must be TRUE):
  1. User sees a blinking cursor in the output pane and can move it with arrow keys
  2. Cursor scrolls the view when moved beyond the visible area
  3. User can drag the mouse across output text and see cyan-background/black-text highlight appear in real time
  4. User can hold Shift+arrow keys (and Shift+Home/End) to extend a selection from the cursor position
  5. Ctrl+A selects all text in the output pane when it is focused, and selection clears on scroll
**Plans:** 3 plans

Plans:
- [ ] 02-01-PLAN.md — App state fields + highlight overlay rendering engine
- [ ] 02-02-PLAN.md — Cursor navigation + keyboard selection
- [ ] 02-03-PLAN.md — Mouse click/drag interaction + visual checkpoint

### Phase 3: Clipboard, Keybindings & Composer
**Goal**: Selected text anywhere in the TUI can be copied to the system clipboard, and all keybindings reflect the new Ctrl+C/Q semantics
**Depends on**: Phase 2
**Requirements**: CLIP-01, CLIP-02, KEYS-01, KEYS-02, COMP-01, COMP-02
**Success Criteria** (what must be TRUE):
  1. Ctrl+C (or Cmd+C) copies the currently selected text to the system clipboard when a selection exists in either pane
  2. Ctrl+C with no active selection does nothing -- no side effects, no error, no session stop
  3. Ctrl+Q stops the current session (replacing Ctrl+C's old stop role), and the old Ctrl+C clear/stop behavior is fully removed
  4. User can select text in the composer using Shift+arrow keys and Ctrl+A, and copy it with Ctrl+C
**Plans:** 1/2 plans executed

Plans:
- [ ] 03-01-PLAN.md — Ctrl+C copy-to-clipboard + Ctrl+Q stop/quit + ClipboardBridge wiring + copy flash
- [ ] 03-02-PLAN.md — Composer selection (Ctrl+A, Shift+arrows) + end-to-end verification checkpoint

## Progress

**Execution Order:**
Phases execute in numeric order: 1 -> 2 -> 3

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Coordinate Translation & Infrastructure | 2/2 | Complete | 2026-03-23 |
| 2. Output Pane Selection | 1/3 | In progress | - |
| 3. Clipboard, Keybindings & Composer | 1/2 | In Progress|  |
