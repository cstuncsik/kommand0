# Phase 2: Output Pane Selection - Research

**Researched:** 2026-03-24
**Domain:** TUI cursor rendering, keyboard/mouse selection, ratatui span-level highlight injection
**Confidence:** HIGH

## Summary

Phase 2 adds a visible cursor and text selection to the output pane. The codebase already has all the infrastructure needed from Phase 1: `WrapMap` for coordinate translation, `SelectionState` for cursor/range state, and `ScrollbackBuffer` for viewport management. The work is primarily integration -- wiring these components into the key dispatch (main.rs), mouse handler (mouse.rs), and render pipeline (render.rs).

The main technical challenges are: (1) injecting selection highlight spans into the existing markdown-styled `Line` objects without breaking the styling pipeline, (2) implementing cursor blink using the existing 50ms tick interval, and (3) correctly translating between screen coordinates and logical text positions when the user navigates wrapped lines. All of these are well-understood problems with clear integration points in the current code.

**Primary recommendation:** Build in three waves -- cursor state + rendering first, then keyboard navigation, then mouse interaction. Each wave is independently testable and builds on the previous.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Block cursor (full character highlight, white bg/black text, ~500ms blink)
- Dim/hollow block when output pane is unfocused
- Bottom-left initial position when output pane first gets focus
- Remember cursor position when switching back to output pane
- Up/Down moves by visual rows (respects wrapping)
- Left/Right wraps across line boundaries
- Ctrl+Left/Right jumps by word boundaries
- Home = document top, End = document bottom (NOT line-level)
- Page Up/Down moves cursor + scrolls viewport (VS Code style)
- Maintain desired column across Up/Down movements
- Auto-scroll to reveal cursor when it hits visible edge
- Shift+Arrow extends character-level selection
- Shift+Ctrl+Left/Right extends selection by word
- Shift+Home/Shift+End extends selection to document start/end
- Ctrl+A selects all text when output pane is focused
- Selection clears on manual scroll
- Click focuses output pane AND places cursor at clicked position
- Click clears any existing selection
- Click on empty space snaps cursor to end of line
- Mouse drag: anchor on MouseDown, drag extends selection real time
- Cyan background / black text for selection highlight
- Cursor stays at logical position when new output arrives
- Placing cursor mid-document stops auto-scroll
- Selection persists through new output (clears on manual scroll only)

### Claude's Discretion
- Blink implementation details (timer mechanism)
- Exact word-boundary detection algorithm for Ctrl+Left/Right
- Mouse drag edge-scrolling behavior (if user drags past viewport edge)
- Any standard editor behavior not explicitly specified above

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| CURS-01 | User can move a blinking cursor in output pane using arrow keys | Cursor state in App struct, blink via tick_counter, key dispatch in Focus::Output arm |
| CURS-02 | Cursor scrolls into view when moved beyond visible area | Auto-scroll logic after each cursor movement using WrapMap::logical_to_screen |
| OSEL-01 | User can select text by holding Shift+arrow keys | Shift modifier detection via KeyModifiers::SHIFT, transition Cursor->Range in SelectionState |
| OSEL-02 | User can select to start/end with Shift+Home/Shift+End | Home/End already handled in Focus::Output -- add Shift variant |
| OSEL-03 | User can drag mouse to select text region in output pane | MouseDown sets anchor via screen_to_logical, Drag extends via same translation |
| OSEL-04 | Selected text is highlighted with cyan background and black text | Post-process styled Lines to overlay selection spans before render |
| OSEL-05 | User can select all output text with Ctrl+A when output focused | Ctrl+A sets Range from (0,0) to last position in scrollback |
| OSEL-06 | Selection clears when user scrolls | Clear SelectionState on scroll_up/scroll_down calls |
</phase_requirements>

## Standard Stack

### Core (already in project)
| Library | Purpose | Why Standard |
|---------|---------|--------------|
| ratatui | TUI framework -- Paragraph, Line, Span, Style, Wrap | Already used for all rendering |
| crossterm | Terminal events -- KeyCode, KeyModifiers, MouseEventKind | Already used for input |
| unicode-segmentation | Grapheme cluster iteration for cursor positioning | Already used in WrapMap |
| unicode-width | Display width for column calculations | Already used in WrapMap |

### No New Dependencies Needed
All required functionality is achievable with existing dependencies. Cursor blink uses the existing tokio::time::interval tick. Selection highlighting uses ratatui's Span styling. Mouse events use crossterm's existing MouseEventKind variants.

## Architecture Patterns

### New State Fields on App Struct
```rust
// In App struct, add per-workspace:
pub(crate) selections: HashMap<String, SelectionState>,  // per-workspace selection
pub(crate) cursor_desired_col: HashMap<String, usize>,   // sticky column for Up/Down
pub(crate) cursor_blink_on: bool,                        // toggle every ~500ms
pub(crate) auto_scroll_suppressed: HashSet<String>,      // per-workspace auto-scroll suppression
```

**Pattern:** Follow the existing `scrollbacks: HashMap<String, ScrollbackBuffer>` pattern -- all per-workspace state is keyed by workspace ID string.

### Cursor Blink via Existing Tick
The app already has a 50ms tick interval (line 516) and a `tick_counter` (line 1423). Cursor blink at ~500ms = every 10th tick:
```rust
// In tick handler (line 1421-1425), add:
if app.tick_counter % 10 == 0 {
    app.cursor_blink_on = !app.cursor_blink_on;
}
```

### Selection Highlight Injection Pattern

The render pipeline currently: `raw_lines -> build_output_lines() -> style_markdown_line() -> Vec<Line> -> Paragraph`. Selection highlights must be applied AFTER markdown styling but BEFORE Paragraph creation.

**Approach:** Post-process the `Vec<Line>` to split spans at selection boundaries and override their style:

```rust
fn apply_selection_highlight(
    lines: &mut Vec<Line<'static>>,
    selection: &SelectionState,
    scroll_from_top: usize,
    inner_height: usize,
    raw_lines: &[&str],
    wrap_map: &WrapMap,
) {
    // For each visible Line, determine which character range is selected,
    // then split spans to apply Style::default().bg(Color::Cyan).fg(Color::Black)
}
```

**Key insight:** The existing `Line` objects from `build_output_lines` use `Span`s with owned `String` content. To apply selection, iterate each `Line`'s spans, track character position, and split any span that partially overlaps the selection range into pre/selected/post segments with appropriate styles.

### Cursor Rendering Pattern

The cursor is a single character with white bg/black text styling. It is applied the same way as selection highlighting -- by modifying the style of the span at the cursor position. When `cursor_blink_on` is false (or pane unfocused), use a different style (dim/hollow = just an underline or dim fg).

```rust
fn apply_cursor_highlight(
    lines: &mut Vec<Line<'static>>,
    cursor_line: usize,
    cursor_char: usize,
    blink_on: bool,
    focused: bool,
    // ... coordinate translation params
) {
    // Find the span containing the cursor character
    // Split it and apply cursor style to the single character
    // If blink_on && focused: white bg, black fg
    // If !focused: dim style or hollow block (e.g., just underline)
    // If !blink_on && focused: no style override (character shows normally)
}
```

**Cursor past end of line:** When cursor is on empty space, append a styled space character span to the Line.

### Key Dispatch Restructure

The current Focus::Output key handling (main.rs lines 797-861) treats Up/Down/etc as scroll commands. Phase 2 changes these to cursor movement. The restructure:

1. **Arrow keys without modifiers** = cursor movement (no longer scroll)
2. **Shift+Arrow** = extend selection
3. **Ctrl+Arrow** = word jump
4. **Shift+Ctrl+Arrow** = word-extend selection
5. **Home/End** = document top/bottom (move cursor, scroll to reveal)
6. **Shift+Home/Shift+End** = select to document start/end
7. **PageUp/PageDown** = move cursor + scroll viewport (cursor maintains relative position)
8. **Ctrl+A** = select all
9. **j/k** = can remain as scroll-only shortcuts (no cursor involvement) OR be removed

**Important:** The existing `g`/`G` keybindings for jump-to-top/bottom now conflict with Home/End cursor movement. Home/End are now cursor commands. `g`/`G` should be kept as scroll-only shortcuts OR removed in favor of Home/End.

### Mouse Handler Restructure

Extend `handle_mouse()` in mouse.rs:

```
MouseDown(Left) in output area:
  1. Focus output pane
  2. Clear existing selection
  3. Translate (col, row) -> logical position via WrapMap::screen_to_logical
  4. Set SelectionState::Cursor at that position
  5. Store anchor for potential drag

Drag(Left) while in output area:
  1. Translate (col, row) -> logical position
  2. Set SelectionState::Range { anchor: stored, cursor: current }
  3. Trigger re-render

MouseUp(Left):
  1. Finalize selection (already in Range state from drag)
  2. No special action needed
```

**WrapMap availability in mouse handler:** The mouse handler needs access to WrapMap, which requires the raw lines and pane width. The handler already has `&mut App` which has `scrollbacks` and `pane_areas`. Build WrapMap on-demand during mouse events in the output pane.

### Scroll Offset Translation

The scrollback uses "offset from bottom" (`scroll_offset`), but WrapMap uses "offset from top" (`scroll_from_top`). The conversion is:

```rust
let total_visual = wrap_map.total_visual_rows();
let max_scroll = total_visual.saturating_sub(inner_height);
let clamped_offset = scroll_offset.min(max_scroll);
let scroll_from_top = max_scroll.saturating_sub(clamped_offset);
```

This conversion already exists in `render_output_content()` (line 880-882). It needs to be extracted into a shared helper since it is now needed in both rendering and input handling.

### Auto-scroll Suppression

When the user places a cursor mid-document, auto-scroll should stop (new output should not force viewport to bottom). This requires:

1. When cursor is set to a non-bottom position: add workspace ID to `auto_scroll_suppressed`
2. When new output arrives and workspace is in `auto_scroll_suppressed`: do NOT call `reset_scroll()`
3. When user explicitly scrolls to bottom OR clears cursor: remove from `auto_scroll_suppressed`

Currently, `reset_scroll()` is called when the user sends a message (line 789). Streaming output handling likely also pins to bottom. These need to be conditional.

### Recommended Project Structure (no new files)

All changes go into existing files:
```
apps/tui/src/
  main.rs        -- App struct fields, key dispatch restructure, tick blink
  mouse.rs       -- MouseDown cursor placement, Drag selection, MouseUp
  render.rs      -- apply_selection_highlight(), apply_cursor_highlight(), WrapMap integration
  selection.rs   -- (minimal changes -- already has what we need)
  scrollback.rs  -- (minimal changes -- maybe add auto-scroll suppression flag)
  wrap_map.rs    -- (no changes needed -- Phase 1 built everything)
```

### Anti-Patterns to Avoid
- **Building a separate cursor widget:** The cursor is NOT a ratatui widget. It is a style override on existing spans. Do not create a separate render pass.
- **Re-wrapping text for selection:** Do NOT re-wrap text to compute selection. Use WrapMap for all coordinate translation.
- **Storing selection in screen coordinates:** Selection MUST be in logical coordinates (line, grapheme_offset). Screen coordinates change on resize/scroll.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Coordinate translation | Custom screen-to-text mapping | WrapMap::screen_to_logical / logical_to_screen | Already handles wrapping, CJK, emoji correctly |
| Word boundary detection | Custom word break algorithm | Simple Unicode-aware algorithm: split on whitespace/punctuation boundaries | Standard approach, good enough for Ctrl+arrow |
| Span splitting for highlights | Manual string slicing | Grapheme-aware span splitting utility function | Must handle multi-byte graphemes, CJK double-width correctly |

## Common Pitfalls

### Pitfall 1: Span Splitting at Grapheme Boundaries
**What goes wrong:** Splitting a span at a byte offset mid-grapheme (e.g., mid-emoji) produces invalid text or rendering artifacts.
**Why it happens:** Selection boundaries are in grapheme offsets but spans store String content.
**How to avoid:** Always iterate by graphemes when splitting spans. Track grapheme index, not byte index, when determining split points.
**Warning signs:** Garbled text at selection boundaries, panics on emoji content.

### Pitfall 2: Off-by-One in Selection Range
**What goes wrong:** Selection includes one too many or one too few characters. Cursor appears one position off.
**Why it happens:** Confusion between inclusive/exclusive ranges, especially at line boundaries.
**How to avoid:** SelectionState::ordered_range() returns inclusive range. WrapMap::screen_to_logical clamps to last character. Be consistent: cursor position = the character the cursor is ON (inclusive).
**Warning signs:** Selection highlight doesn't match copied text (Phase 3).

### Pitfall 3: WrapMap Stale After Resize
**What goes wrong:** Cursor position is correct for old pane width but wrong after terminal resize.
**Why it happens:** WrapMap is built with a specific pane_width. If the pane resizes, old logical positions may map to different screen positions.
**How to avoid:** Rebuild WrapMap fresh each render frame (it's cheap -- just iteration). Do NOT cache WrapMap across frames. The cursor's logical position remains valid; only the screen rendering changes.
**Warning signs:** Cursor jumps after resize, selection highlight misaligned.

### Pitfall 4: Scroll Offset Direction Confusion
**What goes wrong:** Scrolling in the wrong direction, cursor auto-scroll scrolls away from cursor.
**Why it happens:** ScrollbackBuffer uses "offset from bottom" (0 = at bottom). WrapMap uses "scroll_from_top" (0 = at top). These are inverses.
**How to avoid:** Always convert through the formula: `scroll_from_top = max_scroll - clamped_offset`. Extract this into a helper function used by both render and input code.
**Warning signs:** Auto-scroll goes wrong direction, cursor "runs away" from viewport.

### Pitfall 5: Modifier Key Detection on macOS
**What goes wrong:** Shift+Arrow or Ctrl+A not detected on macOS.
**Why it happens:** crossterm on macOS with certain terminal emulators may report modifiers differently. Ctrl+Left/Right may arrive as different key codes depending on terminal.
**How to avoid:** Test with the actual target terminal. For Ctrl+Left/Right, some terminals send `KeyCode::Char('b')` with Ctrl (readline-style) instead of `KeyCode::Left` with CONTROL modifier. Check crossterm documentation for platform differences.
**Warning signs:** Key combinations work in one terminal but not another.

### Pitfall 6: Cursor on Styled/Transformed Lines
**What goes wrong:** Cursor position is wrong on lines that `style_markdown_line` transforms (e.g., `"> "` prefix removed, `"```"` replaced with `"---"`).
**Why it happens:** The raw logical line and the styled Line have different content. User messages get `"> "` prefix stripped and right-aligned. Code fences become decorators.
**How to avoid:** Selection highlighting operates on styled Lines (post-processing), so character positions must correspond to styled content. Use a mapping from logical position to styled-line character position. OR accept that cursor/selection on transformed lines (separators, code fence markers) is approximate.
**Warning signs:** Cursor appears in wrong column on user message lines, selection includes invisible prefix characters.

## Code Examples

### Span Splitting for Selection Overlay
```rust
/// Split a Line's spans to apply a highlight style over a character range.
/// `start_col` and `end_col` are display-column offsets within this Line.
fn overlay_style_on_line(
    line: &mut Line<'static>,
    start_col: usize,
    end_col: usize,
    style: Style,
) {
    let mut new_spans: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;

    for span in line.spans.drain(..) {
        let span_text: &str = span.content.as_ref();
        let span_width: usize = UnicodeWidthStr::width(span_text);
        let span_end = col + span_width;

        if span_end <= start_col || col >= end_col {
            // Entirely outside selection
            new_spans.push(span);
        } else if col >= start_col && span_end <= end_col {
            // Entirely inside selection
            new_spans.push(Span::styled(span.content.into_owned(), style));
        } else {
            // Partially overlapping -- split by graphemes
            let mut pre = String::new();
            let mut mid = String::new();
            let mut post = String::new();
            let mut c = col;
            for g in span_text.graphemes(true) {
                let gw = UnicodeWidthStr::width(g);
                if c < start_col {
                    pre.push_str(g);
                } else if c < end_col {
                    mid.push_str(g);
                } else {
                    post.push_str(g);
                }
                c += gw;
            }
            if !pre.is_empty() {
                new_spans.push(Span::styled(pre, span.style));
            }
            if !mid.is_empty() {
                new_spans.push(Span::styled(mid, style));
            }
            if !post.is_empty() {
                new_spans.push(Span::styled(post, span.style));
            }
        }
        col = span_end;
    }
    line.spans = new_spans;
}
```

### Word Boundary Detection for Ctrl+Arrow
```rust
/// Find the next word boundary position moving right from `char_offset` in `line`.
fn next_word_boundary(line: &str, char_offset: usize) -> usize {
    let graphemes: Vec<&str> = line.graphemes(true).collect();
    if char_offset >= graphemes.len() {
        return graphemes.len();
    }
    let mut i = char_offset;
    // Skip current word (non-whitespace)
    while i < graphemes.len() && !graphemes[i].chars().all(|c| c.is_whitespace()) {
        i += 1;
    }
    // Skip whitespace
    while i < graphemes.len() && graphemes[i].chars().all(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// Find the previous word boundary position moving left from `char_offset` in `line`.
fn prev_word_boundary(line: &str, char_offset: usize) -> usize {
    let graphemes: Vec<&str> = line.graphemes(true).collect();
    if char_offset == 0 {
        return 0;
    }
    let mut i = char_offset;
    // Skip whitespace
    while i > 0 && graphemes[i - 1].chars().all(|c| c.is_whitespace()) {
        i -= 1;
    }
    // Skip word
    while i > 0 && !graphemes[i - 1].chars().all(|c| c.is_whitespace()) {
        i -= 1;
    }
    i
}
```

### Cursor Auto-Scroll
```rust
/// After moving the cursor, ensure it is visible by adjusting scroll offset.
fn ensure_cursor_visible(
    buf: &mut ScrollbackBuffer,
    wrap_map: &WrapMap,
    cursor_line: usize,
    cursor_char: usize,
    inner_height: usize,
    lines: &[&str],
) {
    let total_visual = wrap_map.total_visual_rows();
    let max_scroll = total_visual.saturating_sub(inner_height);

    // Find cursor's visual row
    if let Some((_x, y)) = wrap_map.logical_to_screen(cursor_line, cursor_char, 0, lines) {
        let cursor_visual_row = y as usize;
        let clamped_offset = buf.scroll_offset().min(max_scroll);
        let scroll_from_top = max_scroll.saturating_sub(clamped_offset);

        if cursor_visual_row < scroll_from_top {
            // Cursor above viewport -- scroll up
            let new_scroll_from_top = cursor_visual_row;
            let new_offset = max_scroll.saturating_sub(new_scroll_from_top);
            // Set scroll_offset directly (need setter on ScrollbackBuffer)
        } else if cursor_visual_row >= scroll_from_top + inner_height {
            // Cursor below viewport -- scroll down
            let new_scroll_from_top = cursor_visual_row - inner_height + 1;
            let new_offset = max_scroll.saturating_sub(new_scroll_from_top);
            // Set scroll_offset directly
        }
    }
}
```

## State of the Art

| Old Approach (Phase 1) | New Approach (Phase 2) | Impact |
|------------------------|------------------------|--------|
| Arrow keys = scroll output | Arrow keys = move cursor | Key dispatch completely changes for Focus::Output |
| No cursor visible | Block cursor with blink | New render pass for cursor highlight |
| Mouse click = focus only | Mouse click = focus + place cursor | handle_click expanded significantly |
| Mouse drag = track position only | Mouse drag = real-time selection | Drag handler becomes stateful |
| No selection state used | SelectionState drives rendering | selections HashMap on App |

## Open Questions

1. **ScrollbackBuffer scroll_offset setter**
   - What we know: ScrollbackBuffer has scroll_up/scroll_down/reset_scroll but no direct `set_scroll_offset(n)` method
   - What's unclear: Whether we should add one or compute equivalent scroll_up/scroll_down calls
   - Recommendation: Add `pub fn set_scroll_offset(&mut self, offset: usize)` -- simple and clean

2. **j/k keys in output pane post-cursor**
   - What we know: Currently j/k scroll the output (vim-style)
   - What's unclear: Should j/k move the cursor (like vim normal mode) or remain as scroll shortcuts?
   - Recommendation: Keep j/k as scroll-only (no cursor involvement) since the user decided on "editor-style navigation, not vim motions." Arrow keys move cursor. j/k remain scroll shortcuts for users who want to scroll without cursor.

3. **Styled line character offset mapping**
   - What we know: `style_markdown_line` transforms some lines (adds padding to user messages, replaces code fences with decorators)
   - What's unclear: How to map logical position to display position for these transformed lines
   - Recommendation: Build WrapMap from raw lines (which matches logical positions in SelectionState). Apply selection overlay using display-column offsets computed from the styled Line content. Accept that selection on separator/fence lines may highlight the decorator characters.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (Rust built-in) |
| Config file | Cargo.toml workspace |
| Quick run command | `cargo test -p kommand0-tui` |
| Full suite command | `cargo test` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| CURS-01 | Cursor movement with arrow keys | manual | N/A -- requires TUI interaction | N/A |
| CURS-02 | Cursor auto-scroll | unit | `cargo test -p kommand0-tui -- cursor_auto_scroll` | Wave 0 |
| OSEL-01 | Shift+arrow selection | manual | N/A -- requires TUI interaction | N/A |
| OSEL-02 | Shift+Home/End selection | manual | N/A -- requires TUI interaction | N/A |
| OSEL-03 | Mouse drag selection | manual | N/A -- requires mouse interaction | N/A |
| OSEL-04 | Cyan highlight rendering | unit | `cargo test -p kommand0-tui -- overlay_style` | Wave 0 |
| OSEL-05 | Ctrl+A select all | manual | N/A -- requires TUI interaction | N/A |
| OSEL-06 | Selection clears on scroll | unit | `cargo test -p kommand0-tui -- selection_clear_on_scroll` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p kommand0-tui`
- **Per wave merge:** `cargo test`
- **Phase gate:** Full suite green + manual UAT before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] Unit tests for `overlay_style_on_line` span splitting (selection highlight logic)
- [ ] Unit tests for word boundary detection (Ctrl+arrow)
- [ ] Unit tests for cursor auto-scroll calculation
- [ ] Unit tests for selection-clear-on-scroll behavior

Note: Most phase 2 requirements (CURS-01, OSEL-01-03, OSEL-05) are interaction-based and require manual UAT testing. Unit tests cover the algorithmic core (span splitting, word boundaries, auto-scroll math).

## Sources

### Primary (HIGH confidence)
- Codebase inspection: main.rs, render.rs, mouse.rs, selection.rs, wrap_map.rs, scrollback.rs -- direct reading of current implementation
- crossterm documentation -- MouseEventKind::Down, Drag, Up variants; KeyModifiers::SHIFT, CONTROL
- ratatui Line/Span/Style API -- span composition and style override patterns

### Secondary (MEDIUM confidence)
- unicode-segmentation grapheme iteration patterns -- verified via existing WrapMap usage
- unicode-width display column calculations -- verified via existing codebase usage

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- no new dependencies, all libraries already in use and verified
- Architecture: HIGH -- all integration points identified by reading current code, patterns follow existing conventions
- Pitfalls: HIGH -- identified from direct code analysis (scroll offset inversion, grapheme boundary splitting, styled line transforms)

**Research date:** 2026-03-24
**Valid until:** 2026-04-24 (stable -- no external dependency changes expected)
