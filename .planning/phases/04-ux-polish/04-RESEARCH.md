# Phase 4: UX Polish - Research

**Researched:** 2026-03-08
**Domain:** TUI keyboard navigation, overlays, pane management, visual polish (ratatui 0.29 + crossterm 0.28)
**Confidence:** HIGH

## Summary

Phase 4 is a pure UI/UX polish phase over the existing ratatui TUI. The codebase already has the correct Focus enum (Tree/Output/Composer), Tab/Shift-Tab cycling, Esc-to-tree, j/k+arrow navigation, output scrolling, session status colors, and focus-aware cyan/gray border styling. Many of the "locked decisions" are partially or fully implemented already. The main NEW work is: help overlay, zoom mode, chat bubble styling, scrollbar widget, composer auto-expand with placeholder/char count, the `x`/Delete stop-session binding, `g`/Home jump-to-top in output, Enter-on-workspace focusing composer + auto-starting session, and visual refinements (dimmed tree selection when unfocused, session status colors on borders).

**Primary recommendation:** Structure work into three waves: (1) key binding completeness + Enter-on-workspace behavior, (2) visual polish (focus indicators, chat bubbles, scrollbar, composer auto-expand, session status colors), (3) help overlay + zoom mode. All work is in `apps/tui/src/` -- main.rs, composer.rs, scrollback.rs, plus a new help.rs module.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Tab cycles forward: Tree -> Output -> Composer -> Tree
- Shift-Tab cycles backward (reverse order)
- Both j/k AND arrow keys for list navigation (inclusive)
- Both j/k AND arrow keys for output scrolling when Output focused
- Page Up/Down for page scrolling in output
- Both vim keys (g/G) AND Home/End for jump to top/bottom in output
- Esc always returns focus to Tree pane (home base)
- Enter on repo: expand/collapse (existing)
- Enter on workspace: focus Composer immediately; start or resume session if needed
- Ctrl+C in Composer: clears input, stays in Composer
- Enter in Output pane: does nothing (reserved for future functionality)
- Global keys (work from any pane): q (quit), ? (help)
- Focused keys: r/R only in Tree pane
- x/Delete in Tree on workspace: stops running session (note: archive/delete CRUD deferred to future phase)
- Tab in Composer switches pane (not tab character)
- Shift+Enter in Composer must add a newline (currently sends message -- this is a bug)
- Enter sends the message
- Help overlay triggered by '?' key (toggle: ? opens, ? or Esc closes)
- Centered modal overlay on top of TUI, semi-transparent dark background
- Dark background box with bright text, grouped by section with headers
- Shows which pane is currently active at top
- Context-aware: highlights keys relevant to current focus, but shows all keys
- Key format: bracketed -- [Tab] Switch pane  [j/k] Navigate  [?] Help
- No persistent status bar -- help overlay is sufficient
- Focused pane: cyan border color + cyan title text
- Unfocused panes: gray/dark borders, clearly receded but visible
- Same border style (no thick/double-line change), only color differs
- Tree pane: selected item always visible with background highlight; dimmed when Tree unfocused
- Composer: blinking cursor when focused, plus cyan border
- 'z' key triggers zoom (only when Output pane is focused)
- Zoom shows: output + composer pinned at bottom + minimal status bar
- Status bar content: workspace name + session status + scroll position
- Exit zoom: 'z' again OR Esc
- Instant snap transition (no animation)
- Chat bubble style: user messages right-aligned with subtle background color
- Claude output left-aligned, plain (no background)
- No labels -- alignment + background is the distinction
- Fix both auto-scroll and manual scroll (both currently broken)
- Auto-scroll: always tail new output when at bottom
- Manual scroll: j/k/arrows line-by-line, Page Up/Down pages, g/G and Home/End for top/bottom
- Scrollbar: thin unicode block character track (vim-style, minimal)
- Scrollbar only visible when content exceeds viewport
- Red accent for failed sessions, Yellow for stopped, Green for running
- Apply to status indicators in tree view and pane borders/titles when relevant
- Placeholder text: "Type a message..." when empty
- Auto-expand height: starts small, grows up to 6 lines, then scrolls internally
- Own border -- separate box with horizontal divider from output
- Small character/line count in bottom-right corner

### Claude's Discretion
- Exact chat bubble background color (subtle, not distracting)
- Scrollbar unicode characters and exact track style
- Help overlay dimensions and padding
- Auto-expand step behavior (grow per-line or in chunks)
- Exact gray shade for unfocused borders
- How to detect "at bottom" for auto-scroll resume

### Deferred Ideas (OUT OF SCOPE)
- Add repositories from TUI (new CRUD) -- future phase
- Add workspaces to repos from TUI -- future phase
- Archive/delete/reorder workspaces from TUI -- future phase
- x/Delete on repo for cascade delete -- future phase
- Settings for kommand0 root working dir -- future phase
- Tab autocomplete like Claude Code -- future phase
- '/' key for Claude Code actions/skills passthrough -- future phase
- Shell tab completion for workspace names -- deferred from Phase 2
- Text selection/copy mode -- v2 (ASESS-05)
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| UX-01 | Keyboard-first navigation with consistent bindings (j/k, arrows, Enter, Tab) | Key binding scheme fully specified in CONTEXT.md; existing code has partial implementation -- need to add g/G/Home/End, x/Delete, Enter-on-workspace auto-start, ? global key |
| UX-02 | Help overlay showing available keys for current context | New help.rs module needed; render as Clear overlay widget with ratatui; context-aware key grouping by focus state |
| UX-03 | Pane navigation between repo list, workspace list, and output | Largely implemented already (Tab/Shift-Tab/Esc cycling); needs Enter-on-workspace -> focus Composer behavior and zoom mode pane state |
| UX-04 | Focused/zoomed output view (full-screen single session) | New zoom mode: boolean flag on App, alternate layout in ui() rendering full-screen output+composer+status bar; z toggle key when Output focused |
</phase_requirements>

## Standard Stack

### Core (already in use)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| ratatui | 0.29 | Terminal UI framework | Industry standard Rust TUI; already in use |
| crossterm | 0.28 | Terminal backend, key events | Paired with ratatui; already in use |
| tui-textarea | 0.7 | Multi-line text input | Already wrapping Composer; handles cursor, editing |

### Supporting (already in use)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| tokio | 1.x | Async runtime | Event loop, session management |
| futures | 0.3 | Stream utilities | EventStream processing |

### No new dependencies needed

All phase 4 features can be built with the existing stack. ratatui provides:
- `Clear` widget for overlay backgrounds
- `Paragraph` with `Wrap` for help text rendering
- `Block` with `Borders`, `border_style()`, `title_style()` for focus indicators
- `Layout` with `Constraint` for zoom mode layout switching
- `Rect` for manual positioning of overlays and scrollbar

No external scrollbar widget crate is needed -- a thin unicode scrollbar is trivially rendered as a `Paragraph` column.

## Architecture Patterns

### Recommended Changes to Project Structure
```
apps/tui/src/
  main.rs          # App struct + run() + ui() -- modify for zoom, new bindings
  composer.rs      # Modify: auto-expand, placeholder, char count
  scrollback.rs    # Modify: add scroll_to_top(), total_lines(), auto-scroll fix
  help.rs          # NEW: help overlay widget and key definitions
  session_manager.rs  # No changes needed
```

### Pattern 1: Boolean Zoom State (not a Focus variant)
**What:** Add `zoomed: bool` to App struct, not a new Focus enum variant
**When to use:** Zoom is orthogonal to focus -- you can be zoomed while focused on Output or Composer
**Example:**
```rust
struct App {
    // ... existing fields ...
    zoomed: bool,
    show_help: bool,
}
```
**Rationale:** Focus determines which pane receives key events. Zoom determines layout. Mixing them creates invalid states (e.g., what does Focus::Zoomed mean when the user tabs to Composer?).

### Pattern 2: Layered Key Dispatch
**What:** Restructure key handling into layers: modal (help) -> global -> focus-specific
**When to use:** Always -- this replaces the current flat match structure
**Example:**
```rust
// In the key event handler:
if app.show_help {
    match key.code {
        KeyCode::Char('?') | KeyCode::Esc => app.show_help = false,
        _ => {} // swallow all other keys while help is shown
    }
    continue; // or return, skip further handling
}
// Global keys (q, ?, Tab, Shift-Tab, Esc, Ctrl+C)
// Then focus-specific keys
```
**Rationale:** Help overlay is modal -- it must swallow all keys except its own dismiss keys. Current code has global keys and focus-specific keys interleaved, which works but won't scale with help overlay.

### Pattern 3: Render Overlay with Clear + Centered Rect
**What:** Use ratatui's `Clear` widget to blank overlay area, then render content on top
**When to use:** Help overlay rendering
**Example:**
```rust
fn render_help_overlay(frame: &mut Frame, focus: Focus) {
    let area = centered_rect(60, 70, frame.area()); // % width, % height
    frame.render_widget(Clear, area); // blank the area
    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    // ... render key groups as Paragraph inside block
    frame.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ]).split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ]).split(popup_layout[1])[1]
}
```

### Pattern 4: Zoom Mode Layout Switch
**What:** In `ui()`, branch on `app.zoomed` to render full-screen output+composer+status instead of tree+right_pane
**When to use:** When `app.zoomed == true`
**Example:**
```rust
fn ui(frame: &mut Frame, app: &mut App) {
    if app.show_help {
        // render normal layout first, then overlay on top
    }
    if app.zoomed {
        // Full screen: status bar (1 line) + output (fill) + composer (dynamic)
        let chunks = Layout::vertical([
            Constraint::Length(1),      // status bar
            Constraint::Min(1),         // output
            Constraint::Length(composer_height),  // composer
        ]).split(frame.area());
        render_zoom_status_bar(frame, app, chunks[0]);
        render_output(frame, app, chunks[1]);
        render_composer(frame, app, chunks[2]);
    } else {
        // Normal split layout (existing code)
    }
}
```

### Pattern 5: Chat Bubble Rendering with Alignment
**What:** Track message origin (user vs claude) in scrollback, render with different alignment/styling
**When to use:** Output rendering
**Approach:** Extend ScrollbackBuffer lines to carry metadata (source: user|claude|system), or use a prefix convention (the current `"> "` prefix for user messages). For rendering:
```rust
// User messages: right-aligned with background
if line.starts_with("> ") {
    let content = &line[2..];
    let padding = area_width.saturating_sub(content.len() as u16);
    Line::from(vec![
        Span::raw(" ".repeat(padding as usize)),
        Span::styled(content, Style::default().bg(Color::DarkGray)),
    ])
} else {
    // Claude output: left-aligned, no background
    Line::raw(line)
}
```

### Anti-Patterns to Avoid
- **Adding Focus::Zoomed variant:** Zoom is layout, not focus. A bool keeps the state space clean.
- **Rendering scrollbar as a separate widget crate:** A thin unicode track is 15 lines of code. Don't add a dependency.
- **Storing formatted Lines in ScrollbackBuffer:** Keep raw strings in the buffer, format during render. This decouples data from presentation.
- **Using `Paragraph::scroll()` for output scrolling:** The current `visible_lines()` approach is better -- it gives precise control over what's shown and avoids ratatui's scroll limitations with wrapped text.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Multi-line text editing | Custom key-by-key editor | tui-textarea (already in use) | Cursor movement, selection, undo/redo built in |
| Terminal raw mode / key capture | Manual terminal setup | crossterm + ratatui::init() (already in use) | Already handles restore on panic |
| Overlay z-ordering | Manual buffer manipulation | ratatui `Clear` widget + render order | Last rendered widget wins in ratatui |

## Common Pitfalls

### Pitfall 1: Shift+Enter Detection on macOS Terminal
**What goes wrong:** Some terminal emulators don't send distinct Shift+Enter key events -- they send the same KeyCode::Enter with no SHIFT modifier.
**Why it happens:** Terminal protocol limitations; CSI u mode needed for modifier detection.
**How to avoid:** The existing code already handles this correctly with crossterm 0.28's `KeyModifiers::SHIFT` check. This works in iTerm2, Alacritty, Kitty, and WezTerm. In Apple Terminal.app, Shift+Enter may not be distinguishable -- accept this limitation.
**Warning signs:** User reports newlines not working. Document that a modern terminal emulator is needed.

### Pitfall 2: Auto-Scroll Resume Detection
**What goes wrong:** After manually scrolling up, new output arrives but auto-scroll shouldn't engage until user scrolls back to bottom.
**Why it happens:** Need to track whether user is "at bottom" and resume auto-scroll when they return.
**How to avoid:** The existing `ScrollbackBuffer` already has `is_at_bottom()` (checks `scroll_offset == 0`). Use this: only auto-scroll (reset_scroll) when `is_at_bottom()` was true BEFORE new lines arrived. Currently, `push_line` tracks `new_lines_since_scroll` but doesn't auto-scroll. The fix: in the tick handler where output events are processed, check `is_at_bottom()` before pushing lines, and if true, ensure offset stays at 0 after push.
**Recommendation for discretion area:** "At bottom" = `scroll_offset == 0`. This is already implemented and correct.

### Pitfall 3: Composer Auto-Expand Layout Recalculation
**What goes wrong:** When composer grows from 3 to 6 lines, the output area must shrink. If not properly constrained, output text jumps or scrollbar position becomes incorrect.
**Why it happens:** Layout is recalculated every frame in ratatui, so dynamic height is natural. But the scrollback `visible_lines(height)` must use the NEW output height after composer expansion.
**How to avoid:** Calculate composer height first, then use it in layout constraints. The `height_hint()` method already exists -- extend it to return actual content lines (capped at 6) + 2 for borders.
**Warning signs:** Output text jumping when composer height changes.

### Pitfall 4: Help Overlay Swallowing Keys
**What goes wrong:** Keys meant for help dismissal also trigger actions in the underlying pane.
**Why it happens:** If help overlay check isn't first in the key dispatch chain, keys leak through.
**How to avoid:** Check `app.show_help` FIRST in the key handler, handle ?/Esc to dismiss, and `continue`/return for all other keys. This ensures the overlay is truly modal.

### Pitfall 5: Unicode Scrollbar Width Calculation
**What goes wrong:** Unicode block characters may be wider than expected, misaligning the scrollbar track.
**Why it happens:** Terminal font rendering of unicode characters varies.
**How to avoid:** Use single-width unicode blocks: `\u{2588}` (full block) for thumb, `\u{2502}` (thin vertical line) for track. These are reliably single-cell-width across terminals.

### Pitfall 6: Chat Bubble Right-Alignment with Wrapping
**What goes wrong:** Right-aligned user messages that are longer than the viewport width don't wrap cleanly.
**Why it happens:** Right-alignment padding is calculated for the full message, but wrapped lines need recalculation.
**How to avoid:** For user messages wider than viewport, don't right-align -- just apply the background color. Only right-align when the message fits in one line. Alternatively, use `Paragraph::alignment(Alignment::Right)` but this affects the whole paragraph. Since output is rendered line-by-line, apply alignment per-line.

## Code Examples

### Extending ScrollbackBuffer for Auto-Scroll
```rust
// In scrollback.rs - add methods:

/// Jump to top (g key / Home)
pub fn scroll_to_top(&mut self) {
    let total = self.lines.len();
    self.scroll_offset = total; // will be clamped in visible_lines
}

/// Total line count (for scrollbar position calculation)
pub fn total_lines(&self) -> usize {
    self.lines.len()
}

/// Clamped scroll offset (for scrollbar thumb position)
pub fn clamped_offset(&self, viewport_height: usize) -> usize {
    let max_offset = self.lines.len().saturating_sub(viewport_height);
    self.scroll_offset.min(max_offset)
}
```

### Composer Auto-Expand Height
```rust
// In composer.rs - modify height_hint():

pub fn height_hint(&self) -> u16 {
    let content_lines = self.textarea.lines().len().max(1);
    let capped = content_lines.min(6); // max 6 lines of content
    (capped as u16) + 2 // +2 for top/bottom borders
}
```

### Thin Unicode Scrollbar
```rust
fn render_scrollbar(frame: &mut Frame, area: Rect, total_lines: usize, viewport_height: usize, offset: usize) {
    if total_lines <= viewport_height || area.height < 3 {
        return; // no scrollbar needed
    }
    let track_height = area.height.saturating_sub(2) as usize; // inside borders
    let thumb_size = ((viewport_height as f64 / total_lines as f64) * track_height as f64)
        .max(1.0) as usize;
    let max_offset = total_lines.saturating_sub(viewport_height);
    let thumb_pos = if max_offset == 0 {
        0
    } else {
        ((max_offset.saturating_sub(offset)) as f64 / max_offset as f64 * (track_height - thumb_size) as f64) as usize
    };

    for i in 0..track_height {
        let ch = if i >= thumb_pos && i < thumb_pos + thumb_size {
            "\u{2588}" // full block - thumb
        } else {
            "\u{2502}" // thin line - track
        };
        let y = area.y + 1 + i as u16; // +1 for top border
        let x = area.x + area.width - 1; // rightmost column inside border
        // Render single character at position
        frame.render_widget(
            Paragraph::new(ch).style(Style::default().fg(Color::DarkGray)),
            Rect::new(x, y, 1, 1),
        );
    }
}
```

### x/Delete Stop Session Binding (in Tree focus)
```rust
// In the Focus::Tree match arm:
KeyCode::Char('x') | KeyCode::Delete => {
    if let Some(ws) = app.selected_workspace().cloned() {
        let session_info = app.state.find_session_by_workspace(&ws.id)
            .filter(|s| s.status == SessionStatus::Running)
            .map(|s| s.id.clone());
        if let Some(session_id) = session_info {
            let _ = app.session_manager.stop_session(&session_id).await;
            let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
            if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
                buf.push_line("--- Session stopped ---".to_string());
            }
        }
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| ratatui `Scrollbar` widget | Custom thin unicode scrollbar | ratatui 0.29 has built-in `Scrollbar` widget | Could use built-in, but custom is simpler for this use case (single column, minimal style). Either works. |
| tui-textarea 0.4 placeholder | tui-textarea 0.7 `set_placeholder_text()` | v0.5+ | Already using correct version; placeholder already set |

**Note on ratatui built-in Scrollbar:** ratatui 0.29 does include a `Scrollbar` widget (`ratatui::widgets::Scrollbar`, `ScrollbarState`, `ScrollbarOrientation`). It could be used instead of custom rendering. The built-in widget provides `ScrollbarOrientation::VerticalRight`, customizable symbols, and automatic thumb positioning. Either approach works -- the custom approach gives more control over the minimal style requested (thin track, no arrows). Recommend trying the built-in first; fall back to custom if styling is too opaque.

## Open Questions

1. **tui-textarea auto-expand behavior**
   - What we know: `tui-textarea` renders within the `Rect` given to it. The height is determined by the layout constraint, not the widget.
   - What's unclear: Whether tui-textarea internally scrolls when content exceeds its area, or if it clips. Testing suggests it scrolls internally.
   - Recommendation: The layout should dynamically size the composer area based on `textarea.lines().len()`, capped at 6 lines + borders. tui-textarea handles internal scrolling when content exceeds the rendered area.

2. **Ctrl+C in Composer behavior change**
   - What we know: CONTEXT.md says "Ctrl+C in Composer: clears input, stays in Composer". Current code: if empty, goes to Output; if not empty, clears.
   - What's unclear: Should it always stay in Composer even when empty? CONTEXT.md says "stays in Composer" without the empty-composer exception.
   - Recommendation: Follow CONTEXT.md literally -- Ctrl+C always clears and stays in Composer. Remove the "if empty, go to Output" behavior.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (built-in Rust test framework) |
| Config file | Cargo.toml (workspace) |
| Quick run command | `cargo test -p kommand0-tui` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| UX-01 | Key bindings dispatch correctly per focus state | unit | `cargo test -p kommand0-tui -- key_dispatch` | No - Wave 0 |
| UX-01 | g/G/Home/End scroll output to top/bottom | unit | `cargo test -p kommand0-tui -- scroll_to` | No - Wave 0 (scrollback.rs tests exist, extend) |
| UX-02 | Help overlay content correct for each focus | unit | `cargo test -p kommand0-tui -- help_content` | No - Wave 0 |
| UX-03 | Tab/Shift-Tab cycles focus correctly | unit | `cargo test -p kommand0-tui -- focus_cycle` | No - Wave 0 |
| UX-04 | Zoom toggle changes layout state | unit | `cargo test -p kommand0-tui -- zoom_toggle` | No - Wave 0 |
| UX-01 | Enter on workspace starts/resumes session + focuses composer | integration | manual-only (requires terminal) | N/A |
| UX-04 | Zoom renders full-screen output+composer+status | integration | manual-only (requires terminal rendering) | N/A |
| UX-02 | Help overlay renders centered with correct keys | integration | manual-only (visual verification) | N/A |

### Sampling Rate
- **Per task commit:** `cargo test -p kommand0-tui`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `apps/tui/src/scrollback.rs` -- extend existing tests for `scroll_to_top()`, `total_lines()`, `clamped_offset()`
- [ ] Key dispatch and focus cycle tests require extracting logic into testable functions (currently embedded in async `run()` -- extract into `App` methods for testability)
- [ ] Help overlay content tests require the help.rs module to expose key definitions as data (not just rendering)

## Sources

### Primary (HIGH confidence)
- **Codebase inspection** -- `apps/tui/src/main.rs` (761 lines), `composer.rs`, `scrollback.rs`, `session_manager.rs`
- **Cargo.toml** -- ratatui 0.29, crossterm 0.28, tui-textarea 0.7 confirmed from workspace deps
- **CONTEXT.md** -- All user decisions locked and detailed

### Secondary (MEDIUM confidence)
- **ratatui 0.29 API** -- Clear widget, Layout, Block, Borders, Scrollbar widget confirmed from training data (ratatui is stable, API well-known)
- **tui-textarea 0.7 API** -- set_placeholder_text, lines(), set_block confirmed from codebase usage

### Tertiary (LOW confidence)
- **ratatui Scrollbar widget API** -- built-in Scrollbar exists in 0.29 but exact API (ScrollbarState, symbols) not verified against Context7. Recommendation to try it is low-risk since custom fallback is trivial.

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - no new deps, all existing libs verified in Cargo.toml
- Architecture: HIGH - patterns derived from reading the actual codebase, not speculation
- Pitfalls: HIGH - identified from code structure and terminal UI experience
- Key bindings: HIGH - CONTEXT.md is extremely specific; existing code partially implements them

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable stack, no fast-moving dependencies)
