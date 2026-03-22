# Features Research: TUI Text Selection & Clipboard

**Research Date:** 2026-03-22
**Domain:** Terminal UI text selection

## Table Stakes (Must Have)

| Feature | Complexity | Dependencies |
|---------|------------|--------------|
| Mouse drag selection in output pane | HIGH | Coordinate translation |
| Visual selection highlight (cyan bg/black text) | MEDIUM | Span splitting |
| Ctrl+C / Cmd+C copies selection to system clipboard | LOW | arboard crate |
| Selection feedback (visual confirmation of copy) | LOW | None |
| Keyboard selection (Shift+arrows) in output pane | MEDIUM | Output cursor |
| Select All (Ctrl+A) in focused pane | LOW | Selection state |
| Ctrl+C safety (ignore when no selection) | LOW | Selection state check |
| Ctrl+Q replaces stop-session | LOW | Key handler update |
| Composer selection (via tui-textarea) | LOW | tui-textarea API |

## Differentiators (Nice to Have)

| Feature | Complexity | Dependencies | Priority |
|---------|------------|--------------|----------|
| Word selection (double-click) | MEDIUM | Coordinate translation | v1.x |
| Line selection (triple-click) | LOW | Word selection | v1.x |
| Selection survives during streaming output | HIGH | Anchor recalculation | v2+ |
| Block/column selection (Alt+drag) | HIGH | Custom rendering | v2+ |
| Markdown-aware selection (skip formatting) | HIGH | AST integration | v2+ |
| Formatted copy (preserve markdown) | MEDIUM | Selection extraction | v2+ |

## Anti-Features (Do NOT Build)

| Feature | Reason |
|---------|--------|
| Cross-pane selection (output + composer) | Confusing UX, unclear what gets copied |
| Right-click context menu | Not standard in TUI, adds complexity |
| Paste (Ctrl+V) | Separate feature, defer to future |
| Multi-cursor selection | Overkill for output pane viewing |
| Selection persists across scroll | Stale coordinates, confusing behavior |
| Auto-copy on select (tmux-style) | Non-standard, conflicts with extend-selection |

## Dependency Tree

```
Coordinate Translation (foundation)
├── Mouse Selection (needs screen→text mapping)
│   └── Mouse Drag Selection
├── Keyboard Selection (needs cursor position)
│   ├── Arrow Key Cursor
│   └── Shift+Arrow Extend
├── Selection Highlight Rendering (needs span splitting)
│   └── Span Splitter (split styled spans at selection boundaries)
└── Text Extraction (needs selection→text mapping)
    └── Clipboard Copy
```

**Critical path:** Coordinate translation → selection state → highlight rendering → clipboard copy

## Competitor Analysis

| App | Selection Model | Copy Mechanism | Notes |
|-----|----------------|----------------|-------|
| **vim** | Visual mode (v/V/Ctrl+V) | Yank to register, system clipboard optional | Mode-based, not applicable |
| **less** | None (relies on terminal emulator) | Terminal handles it | No custom selection |
| **tmux** | Copy mode with vi/emacs keys | Copy to tmux buffer, optional system clipboard | Mode-based approach |
| **Helix** | Selection-first (every movement selects) | System clipboard integration | Most modern approach |
| **Terminal emulators** | Mouse drag, Shift+click extend | Auto-copy or Cmd+C | What users expect |

**Takeaway:** Users expect terminal-emulator-like behavior (mouse drag, Ctrl+C), not mode-based selection. Our approach aligns with user expectations.

## MVP Definition

**v1 (this milestone):**
- All 9 table stakes features
- Output cursor + keyboard selection
- Mouse drag selection
- Clipboard copy
- Ctrl+Q for stop-session

**v1.x (future):**
- Word/line selection (double/triple click)
- Selection mode indicator in status bar

**v2+ (far future):**
- Block selection
- Markdown-aware selection
- Streaming-safe selection

---
*Features research: 2026-03-22*
