# Requirements: TUI Text Selection & Clipboard

**Defined:** 2026-03-22
**Core Value:** Users can select any text in the TUI and copy it to the system clipboard

## v1 Requirements

### Output Cursor

- [ ] **CURS-01**: User can move a blinking cursor in output pane using arrow keys
- [ ] **CURS-02**: Cursor scrolls into view when moved beyond visible area

### Output Selection

- [ ] **OSEL-01**: User can select text by holding Shift+arrow keys in output pane
- [ ] **OSEL-02**: User can select to start/end of line with Shift+Home/Shift+End
- [ ] **OSEL-03**: User can drag mouse to select text region in output pane
- [ ] **OSEL-04**: Selected text is highlighted with cyan background and black text
- [ ] **OSEL-05**: User can select all output text with Ctrl+A when output pane is focused
- [ ] **OSEL-06**: Selection clears when user scrolls

### Composer Selection

- [ ] **COMP-01**: User can select text in composer using Shift+arrow keys (tui-textarea)
- [ ] **COMP-02**: User can select all composer text with Ctrl+A when composer is focused

### Clipboard

- [ ] **CLIP-01**: User can copy selected text to system clipboard with Ctrl+C or Cmd+C
- [ ] **CLIP-02**: Ctrl+C with no active selection does nothing (no side effects)
- [x] **CLIP-03**: Clipboard integration works cross-platform via arboard crate

### Keybindings

- [ ] **KEYS-01**: Ctrl+Q stops the current session (replaces Ctrl+C's old stop role)
- [ ] **KEYS-02**: Existing Ctrl+C behavior (clear composer, stop session) is fully replaced

### Coordinate Translation

- [x] **CORD-01**: Screen position (x,y) correctly maps to text position (line, char) accounting for line wrapping
- [x] **CORD-02**: Coordinate translation handles unicode characters (CJK, emoji) correctly
- [x] **CORD-03**: Coordinate translation accounts for border padding and scroll offset

## v2 Requirements

### Enhanced Selection

- **ESEL-01**: User can double-click to select a word
- **ESEL-02**: User can triple-click to select a line
- **ESEL-03**: Selection survives during streaming output (anchor recalculation)

### Enhanced Copy

- **ECPY-01**: User can copy with markdown formatting preserved
- **ECPY-02**: Selection mode indicator shown in status bar

## Out of Scope

| Feature | Reason |
|---------|--------|
| Paste from clipboard (Ctrl+V) | Separate feature, defer to future milestone |
| Cross-pane selection (output + composer) | Confusing UX, unclear what gets copied |
| Right-click context menu | Not standard in TUI applications |
| Block/column selection (Alt+drag) | High complexity, overkill for v1 |
| Multi-cursor selection | Not applicable to output viewing |
| Selection persistence across scroll | Stale coordinates cause confusing behavior |
| Auto-copy on select (tmux-style) | Non-standard, conflicts with extend-selection |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| CURS-01 | Phase 2 | Pending |
| CURS-02 | Phase 2 | Pending |
| OSEL-01 | Phase 2 | Pending |
| OSEL-02 | Phase 2 | Pending |
| OSEL-03 | Phase 2 | Pending |
| OSEL-04 | Phase 2 | Pending |
| OSEL-05 | Phase 2 | Pending |
| OSEL-06 | Phase 2 | Pending |
| COMP-01 | Phase 3 | Pending |
| COMP-02 | Phase 3 | Pending |
| CLIP-01 | Phase 3 | Pending |
| CLIP-02 | Phase 3 | Pending |
| CLIP-03 | Phase 1 | Complete |
| KEYS-01 | Phase 3 | Pending |
| KEYS-02 | Phase 3 | Pending |
| CORD-01 | Phase 1 | Complete |
| CORD-02 | Phase 1 | Complete |
| CORD-03 | Phase 1 | Complete |

**Coverage:**
- v1 requirements: 18 total
- Mapped to phases: 18
- Unmapped: 0

---
*Requirements defined: 2026-03-22*
*Last updated: 2026-03-22 after roadmap creation*
