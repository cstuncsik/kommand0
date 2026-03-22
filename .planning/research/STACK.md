# Stack Research: TUI Text Selection & Clipboard

**Research Date:** 2026-03-22
**Confidence:** HIGH

## Recommendations

### New Dependencies

| Crate | Version | Purpose | Confidence |
|-------|---------|---------|------------|
| `arboard` | 3.6 | Cross-platform system clipboard access | HIGH |

**arboard rationale:** Maintained by 1Password, simple API (`Clipboard::new()?.set_text(s)?`), cross-platform (macOS, Linux X11/Wayland, Windows). Latest release Aug 2025.

**Rejected alternatives:**
- `copypasta` — decent but worse Wayland support and maintenance cadence
- `cli-clipboard` — unmaintained since 2022

### Existing Stack Capabilities

| Component | Selection Support | Notes |
|-----------|-------------------|-------|
| `tui-textarea` 0.7 | Full — since v0.4.0 | `start_selection()`, `select_all()`, `copy()`, `set_selection_style()`, built-in Shift+arrow selection |
| `crossterm` 0.28 | All mouse events | Down, Drag, Up, Moved all available. App currently missing MouseUp handler |
| `ratatui` 0.29 | No native selection | Must implement custom span splitting + highlight rendering |

### Key Findings

1. **Only one new dependency needed** — `arboard` for clipboard. Everything else is in existing stack.
2. **tui-textarea 0.7 already has full selection support** — 12 selection methods available. Internal yank buffer must be bridged to `arboard` for system clipboard.
3. **crossterm 0.28 has all needed mouse events** — MouseUp must be added to handler (currently not handled).
4. **No crate exists for output pane selection** — custom engineering required. Needs line-wrap map from `Line::styled_graphemes()` + `unicode-width`, and span splitter for selection highlighting.

## Integration Notes

- **arboard:** Add as `default-features = false` for text-only (no image deps). Store as `Option<Clipboard>` in app state for Linux lifetime requirements.
- **Composer selection nearly free** — tui-textarea does heavy lifting, just bridge yank buffer to arboard.
- **Output pane selection is the hard part** — coordinate translation and span splitting are bulk of work.

## Phase Ordering Implication

Composer selection first (validates clipboard with minimal custom code) → Output pane selection (complex, builds on clipboard foundation).

---
*Stack research: 2026-03-22*
