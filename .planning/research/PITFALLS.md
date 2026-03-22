# Pitfalls Research: TUI Text Selection & Clipboard

**Research Date:** 2026-03-22
**Domain:** TUI text selection implementation

## Critical Pitfalls

### 1. Coordinate Translation with Wrapped Lines

**Severity:** HIGH
**Phase:** Output mouse selection

ratatui's `Paragraph` widget wraps lines internally but does NOT expose the wrap state. When a user clicks at screen position (x, y), you must reverse-map to (logical_line, char_offset) — but you don't know where ratatui broke the line.

**Warning signs:**
- Selection highlight doesn't match what user dragged over
- Click at end of wrapped line selects wrong character
- Off-by-one errors on every wrapped line

**Prevention:**
- Build your own line-wrap map during rendering using `unicode-width` to calculate character widths
- Map each visual row to (logical_line_index, char_start_offset)
- Only compute for visible lines (not entire scrollback)
- Cache the map per frame — invalidate on resize, scroll, or new content
- Test with: CJK characters (width 2), emoji (width 2), tabs, zero-width joiners

### 2. Unicode Width vs Byte Offset Confusion

**Severity:** HIGH
**Phase:** Coordinate translation, text extraction

Rust strings are UTF-8 byte sequences. Screen columns use display width. A CJK character is 1 char, 3-4 bytes, but 2 columns wide. Mixing these coordinate systems causes selection to drift.

**Warning signs:**
- Selection works for ASCII but breaks with non-ASCII text
- Extracted text is truncated or includes extra characters
- Cursor position wrong after CJK/emoji characters

**Prevention:**
- Always use `unicode-width::UnicodeWidthStr` for display width calculations
- Maintain separate coordinate types: `ByteOffset`, `CharIndex`, `DisplayColumn`
- Never convert between them implicitly — use explicit conversion functions
- Test with mixed ASCII + CJK + emoji strings

### 3. Span Splitting at Selection Boundaries

**Severity:** MEDIUM
**Phase:** Selection highlight rendering

ratatui `Line` contains `Vec<Span>` with styles. Selection boundaries may fall mid-span. You must split spans at selection start/end and apply highlight style to the selected portion.

**Warning signs:**
- Selection highlight "snaps" to span boundaries instead of character positions
- Style corruption — original styling lost inside selection
- Panic on empty spans after splitting

**Prevention:**
- Write a `split_span_at(span, char_offset) -> (Span, Span)` utility
- Preserve original style on non-selected portions
- Merge selection style with original style (background = cyan, foreground = black, keep bold/italic)
- Handle edge cases: split at 0, split at end, empty spans
- Unit test the splitter extensively before integrating

### 4. Scroll Offset Mismatch

**Severity:** MEDIUM
**Phase:** Output cursor, keyboard selection

Scrollback uses `scroll_offset` (lines from bottom, 0 = latest). Screen coordinates use (0, 0) = top-left. Converting between these while accounting for wrapping is error-prone.

**Warning signs:**
- Selection appears offset by N lines after scrolling
- Auto-scroll breaks selection
- Cursor jumps when new output arrives

**Prevention:**
- Selection clears on scroll (simplifies v1 enormously)
- Lock auto-scroll when selection is active
- Use visual-line coordinates (post-wrap) for all screen interactions
- Convert to logical-line coordinates only for text extraction

### 5. Ctrl+C Signal Handling Conflict

**Severity:** MEDIUM
**Phase:** Keybinding changes

crossterm's raw mode captures Ctrl+C as a key event. But if the terminal is not properly in raw mode (e.g., during panic, crash recovery), Ctrl+C sends SIGINT. Changing Ctrl+C semantics means users lose the emergency kill mechanism.

**Warning signs:**
- User can't kill hung TUI process
- Ctrl+C does nothing (selection state bug, copy fails silently)

**Prevention:**
- Ctrl+Q for stop-session must work reliably before changing Ctrl+C
- If no selection AND no active session, Ctrl+C should still allow quit (match current behavior)
- Test: Ctrl+C with selection → copies. Ctrl+C without selection → ignores. Ctrl+Q → stops session.
- Ensure Ctrl+\ (SIGQUIT) still works as emergency kill

### 6. tui-textarea Selection API Assumptions

**Severity:** LOW-MEDIUM
**Phase:** Composer selection

tui-textarea v0.7 has selection methods but the API may behave differently than expected. The internal yank buffer is separate from system clipboard.

**Warning signs:**
- `copy()` puts text in internal buffer but not system clipboard
- Selection style doesn't apply
- Shift+arrow doesn't start selection automatically

**Prevention:**
- Prototype composer selection early — validate tui-textarea API actually works as documented
- Bridge: after `copy()`, read the yank buffer content and push to arboard
- If tui-textarea selection is broken, fall back to wrapping with custom selection layer
- Check tui-textarea GitHub issues for known selection bugs

### 7. arboard Clipboard Lifetime on Linux

**Severity:** LOW (macOS primary target)
**Phase:** Clipboard integration

On X11/Wayland, the clipboard is "owned" by the process. If the `Clipboard` instance is dropped, clipboard contents may be lost. macOS uses a persistent pasteboard, so this is not an issue there.

**Warning signs:**
- Copy works but paste is empty (Linux only)
- Copy works only while app is running

**Prevention:**
- Store `Clipboard` as long-lived `Option<Clipboard>` in App struct
- Initialize once on startup, reuse for all copy operations
- Don't create/drop Clipboard per copy operation

## Risk Summary

| Pitfall | Severity | Likelihood | Phase |
|---------|----------|------------|-------|
| Coordinate translation with wrapping | HIGH | HIGH | Output mouse selection |
| Unicode width confusion | HIGH | MEDIUM | Coordinate translation |
| Span splitting at boundaries | MEDIUM | MEDIUM | Highlight rendering |
| Scroll offset mismatch | MEDIUM | MEDIUM | Output cursor |
| Ctrl+C signal conflict | MEDIUM | LOW | Keybinding changes |
| tui-textarea API assumptions | LOW-MEDIUM | MEDIUM | Composer selection |
| arboard lifetime on Linux | LOW | LOW | Clipboard integration |

---
*Pitfalls research: 2026-03-22*
