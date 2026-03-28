# Phase 3: Clipboard, Keybindings & Composer - Research

**Researched:** 2026-03-28
**Domain:** Clipboard integration, keybinding rewire, composer text selection
**Confidence:** HIGH

## Summary

Phase 3 wires together three related concerns: (1) Ctrl+C copies selected text to the system clipboard, (2) Ctrl+Q replaces Ctrl+C's old stop/quit role, and (3) the composer gets text selection via tui-textarea's built-in selection API. The codebase already has all foundational pieces -- `ClipboardBridge`, `SelectionState`, `WrapMap::extract_text()`, and the key dispatch structure in main.rs. The main work is rewiring the key handlers and exposing the composer's selection state for copy.

A critical finding: **tui-textarea 0.7 has built-in selection support** including `start_selection()`, `select_all()`, `selection_range()`, `copy()`, `yank_text()`, `set_selection_style()`, and `is_selecting()`. Shift+arrow keys automatically trigger selection when passed through `textarea.input()`. This means the composer does NOT need a custom `SelectionState` or custom highlight rendering -- tui-textarea handles it natively. The only custom work needed is intercepting Ctrl+C before it reaches tui-textarea (since tui-textarea's default Ctrl+C copies to its internal yank buffer, not the system clipboard) and routing Ctrl+A to `textarea.select_all()` instead of its default "move to line start" behavior.

**Primary recommendation:** Leverage tui-textarea's built-in selection for the composer. Intercept only Ctrl+C and Ctrl+A at the composer key handler level; let all other keys (including Shift+arrows) pass through to `textarea.input()` unchanged.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Ctrl+C is ALWAYS copy -- clean separation, no dual behavior
- Ctrl+C with no selection = pure no-op (no side effects, no error, no clear)
- Ctrl+C never clears composer anymore -- user does Ctrl+A then Delete to clear
- No double-tap Ctrl+C escape hatch -- Ctrl+C is always copy, period
- No dedicated clear-composer shortcut -- select-all + delete replaces it
- Ctrl+Q stops session from ANY pane (including Composer) -- universally available
- Ctrl+Q with no running session quits app
- Ctrl+Q with running session: stop session first, then quit on next press
- Full selection like output pane -- SelectionState + highlight rendering + extract_text (NOTE: research shows tui-textarea handles this natively, simpler approach available)
- Consistent UX across panes (same interaction model)
- Cyan background / black text highlight (same as output pane)
- Ctrl+A selects all composer text when Composer is focused
- Shift+arrow extends selection character-by-character
- Typing replaces selection (standard editor behavior)
- Brief flash -- selection highlight flashes/pulses once on copy
- Selection persists after copy (editor convention, allows re-copy)
- Selection clears on click (like iTerm2), not automatically after copy
- Escape clears selection in Output pane
- Clipboard unavailable (headless/SSH) = silent no-op, no error shown
- Selection persists (dimmed) when pane loses focus
- Can still copy from unfocused pane with Ctrl+C (selection is global)

### Claude's Discretion
- Flash animation timing/implementation for copy feedback
- Composer WrapMap implementation details (simpler than output pane -- no scroll offset)
- How to intercept tui-textarea key events for Shift+arrow selection
- Exact integration of SelectionState with tui-textarea cursor position

### Deferred Ideas (OUT OF SCOPE)
- Tree pane text selection -- user wants to copy repo/workspace/branch names. Needs its own phase with selection model for tree items.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| CLIP-01 | User can copy selected text to system clipboard with Ctrl+C or Cmd+C | ClipboardBridge.set_text() already implemented; need to wire Ctrl+C handler to extract text from active selection (output or composer) and call it |
| CLIP-02 | Ctrl+C with no active selection does nothing (no side effects) | Key handler checks both output SelectionState and composer is_selecting(); if neither has range, pure no-op |
| KEYS-01 | Ctrl+Q stops the current session (replaces Ctrl+C's old stop role) | Rewire existing Ctrl+C stop logic to Ctrl+Q; make it work from ALL panes including Composer |
| KEYS-02 | Existing Ctrl+C behavior (clear composer, stop session) is fully replaced | Remove old Ctrl+C match arm entirely; replace with copy-if-selection logic |
| COMP-01 | User can select text in composer using Shift+arrow keys | tui-textarea 0.7 handles this natively via input() -- just stop intercepting Shift+arrows before they reach textarea |
| COMP-02 | User can select all composer text with Ctrl+A when composer is focused | Call textarea.select_all() in composer key handler; override tui-textarea's default Ctrl+A (move to line start) |
</phase_requirements>

## Standard Stack

### Core (already in project)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| arboard | 3.6 | System clipboard access | Cross-platform clipboard crate, already integrated as ClipboardBridge |
| tui-textarea | 0.7 | Composer text input with built-in selection | Already used; has native selection, copy, select_all APIs |
| ratatui | 0.29 | TUI framework | Already the rendering backbone |

### Supporting (already in project)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| unicode-segmentation | (workspace) | Grapheme-aware text extraction | Used by WrapMap::extract_text for output pane copy |
| crossterm | (via ratatui) | Terminal event handling | Key event dispatch |

### No New Dependencies Needed
This phase requires zero new crate additions. All functionality is covered by existing dependencies.

## Architecture Patterns

### Key Dispatch Flow (Current -> Target)

**Current flow:**
```
Global match on key.code:
  'q' (non-Composer) -> quit
  'c' + Ctrl -> Composer: clear | Other: stop/quit
  _ -> Focus-specific dispatch
```

**Target flow:**
```
Global match on key.code:
  'q' + Ctrl -> stop session (any pane) / quit app
  'c' + Ctrl -> copy selection if any exists, else no-op
  _ -> Focus-specific dispatch (Composer gets all other keys via textarea.input())
```

### Copy Flow Pattern
```
Ctrl+C pressed (any focus):
  1. Check output pane: selections.get(ws_id).has_range()?
     YES -> extract_text via WrapMap, copy to ClipboardBridge, flash feedback
  2. Check composer: composer.is_selecting()?
     YES -> get text via selection_range() + lines(), copy to ClipboardBridge, flash feedback
  3. Neither has selection -> no-op (silent, no side effects)
```

### Composer Selection Architecture

**Key insight:** tui-textarea 0.7 handles selection natively. We do NOT need:
- Custom SelectionState for composer (tui-textarea has `is_selecting()`, `selection_range()`)
- Custom highlight rendering for composer (tui-textarea renders selection with `set_selection_style()`)
- Custom WrapMap for composer (tui-textarea wraps internally)

**What we DO need:**
1. Set `textarea.set_selection_style()` to cyan background / black text to match output pane
2. Intercept Ctrl+A in composer to call `textarea.select_all()` (overrides default "move to line start")
3. Intercept Ctrl+C globally before it reaches any focus-specific handler
4. Extract composer selected text: use `selection_range()` to get positions, then extract from `textarea.lines()`
5. Let Shift+arrows pass through to `textarea.input()` -- they trigger selection automatically

### Composer Key Interception Order
```rust
// In composer.handle_key():
match key {
    // Ctrl+A -> select_all (override tui-textarea default)
    Ctrl+A => { self.textarea.select_all(); }
    // Shift+Enter -> newline (existing)
    Shift+Enter => { self.textarea.insert_newline(); }
    // Enter -> send (existing)
    Enter => { /* extract and return */ }
    // Everything else (including Shift+arrows) -> textarea.input()
    _ => { self.textarea.input(key); }
}
```

Note: Ctrl+C is handled GLOBALLY in main.rs before reaching composer, so composer never sees it.

### Copy Feedback Flash Pattern
```
1. Set flash state: flash_until = Instant::now() + Duration::from_millis(150)
2. During render: if Instant::now() < flash_until, use white/bright background instead of cyan
3. After flash expires, revert to normal cyan selection highlight
```

Recommendation: Use a simple `Option<Instant>` field on App for the flash timer. 150ms is perceptible but not distracting. The flash only affects the selection style color, not the selection state itself.

### Selection Persistence Across Focus
- Output pane: `selections` HashMap already persists across focus changes
- Composer: tui-textarea maintains selection state internally, survives focus changes
- Dimmed rendering when unfocused: output pane already has this pattern in render.rs (dim style for unfocused cursor); extend to selection highlight
- Composer dimmed selection: call `set_selection_style()` with dimmed color when composer loses focus

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Composer text selection | Custom SelectionState + highlight for composer | tui-textarea's built-in `start_selection()` + `select_all()` + `set_selection_style()` | tui-textarea handles wrapping, cursor, highlight rendering internally |
| Composer selected text extraction | Custom WrapMap for composer | `textarea.lines()` + `selection_range()` | Composer text is simple lines, no scrollback complexity |
| Shift+arrow selection in composer | Manual anchor/cursor tracking | Pass keys to `textarea.input()` | tui-textarea tracks shift modifier and extends selection |
| Typing-replaces-selection | Manual delete-then-insert | `textarea.input()` default behavior | tui-textarea already deletes selection on new input |

**Key insight:** The CONTEXT.md mentioned "Full selection like output pane -- SelectionState + highlight rendering + extract_text" but research shows tui-textarea already provides all of this. The composer selection is dramatically simpler than the output pane selection.

## Common Pitfalls

### Pitfall 1: tui-textarea Ctrl+A Default
**What goes wrong:** tui-textarea's default Ctrl+A moves cursor to line start (Emacs binding), not select-all
**Why it happens:** tui-textarea uses Emacs-style keybindings by default
**How to avoid:** Intercept Ctrl+A in `composer.handle_key()` and call `textarea.select_all()` explicitly, before the key reaches `textarea.input()`
**Warning signs:** Ctrl+A in composer moves cursor to beginning of line instead of selecting all

### Pitfall 2: tui-textarea Ctrl+C Default
**What goes wrong:** tui-textarea's default Ctrl+C copies to its INTERNAL yank buffer, not the system clipboard
**Why it happens:** tui-textarea has its own copy/paste system (`copy()`, `yank_text()`, `paste()`)
**How to avoid:** Ctrl+C is handled globally in main.rs before reaching composer. Never let Ctrl+C reach `textarea.input()`
**Warning signs:** Text appears in yank buffer but not system clipboard

### Pitfall 3: Bare 'q' Still Quits from Non-Composer
**What goes wrong:** The current `KeyCode::Char('q')` handler (line 1267) quits without Ctrl modifier when not in Composer
**Why it happens:** Legacy binding: bare 'q' quits the app
**How to avoid:** This behavior must be preserved alongside the new Ctrl+Q. Bare 'q' quits (non-Composer), Ctrl+Q stops session then quits (any pane). These are separate bindings.
**Warning signs:** User can't type 'q' in output search (if ever added)

### Pitfall 4: ClipboardBridge Not in App Struct
**What goes wrong:** ClipboardBridge exists as a module but is not yet a field on the App struct
**Why it happens:** Phase 1 created the bridge but didn't wire it into App (deferred to Phase 3)
**How to avoid:** Add `clipboard: ClipboardBridge` field to App struct in the first task
**Warning signs:** Compilation errors when trying to access clipboard from key handler

### Pitfall 5: Composer Text Extraction from selection_range()
**What goes wrong:** `selection_range()` returns `((row, col), (row, col))` but col is byte offset or char offset depending on version
**Why it happens:** API ambiguity between byte/char/grapheme positions
**How to avoid:** Use `textarea.lines()` to get line content, then use the col values from `selection_range()` as character indices into those lines. Test with multi-byte characters.
**Warning signs:** Wrong text copied when composer contains unicode

### Pitfall 6: Esc Behavior Conflict
**What goes wrong:** Esc currently exits zoom or returns to Tree focus. Adding "Esc clears selection in Output" conflicts.
**Why it happens:** Esc is overloaded
**How to avoid:** Priority order: (1) if zoomed, exit zoom (2) if Output has selection, clear selection (3) return to Tree. Or: Esc in Output clears selection first, second Esc returns to Tree.
**Warning signs:** User presses Esc expecting to clear selection but gets moved to Tree pane

## Code Examples

### Adding ClipboardBridge to App
```rust
// In App struct definition:
pub(crate) clipboard: clipboard::ClipboardBridge,

// In App::new():
clipboard: clipboard::ClipboardBridge::new(),
```

### Global Ctrl+C Copy Handler
```rust
// Replace existing Ctrl+C handler (main.rs ~line 1278):
KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    // Check output pane selection first
    let copied = if let Some(ws_id) = app.selected_workspace().map(|ws| ws.id.clone()) {
        if let Some(sel) = app.selections.get(&ws_id) {
            if let Some((start, end)) = sel.ordered_range() {
                // Extract text from output using WrapMap
                let lines = collect_output_lines(&app, &ws_id);
                let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
                let wm = WrapMap::build(&refs, app.last_pane_width as usize);
                let text = wm.extract_text(&refs, start, end);
                let _ = app.clipboard.set_text(&text); // silent fail on headless
                // Trigger flash feedback
                app.copy_flash_until = Some(std::time::Instant::now() + Duration::from_millis(150));
                true
            } else { false }
        } else { false }
    } else { false };

    // If no output selection, check composer
    if !copied && app.composer.has_selection() {
        if let Some(text) = app.composer.selected_text() {
            let _ = app.clipboard.set_text(&text);
            app.copy_flash_until = Some(std::time::Instant::now() + Duration::from_millis(150));
        }
    }
    // If neither has selection: no-op (requirement CLIP-02)
}
```

### Ctrl+Q Stop/Quit Handler
```rust
// Replace bare 'q' quit with Ctrl+Q (works in ALL panes including Composer):
KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    let has_running = app.selected_workspace()
        .and_then(|ws| app.state.find_session_by_workspace(&ws.id))
        .filter(|s| s.status == SessionStatus::Running)
        .map(|s| s.id.clone());
    if let Some(session_id) = has_running {
        // First press: stop session
        let _ = app.session_manager.stop_session(&session_id).await;
        let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
        if let Some(ws_id) = app.selected_workspace().map(|ws| ws.id.clone()) {
            if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                buf.push_line("--- Session stopped ---".to_string());
            }
        }
        app.focus = Focus::Output;
        app.composer.set_active(false);
    } else {
        // No running session: quit
        app.session_manager.shutdown_all().await?;
        break;
    }
}
```

### Composer Selection Methods
```rust
// Add to Composer impl:
pub fn has_selection(&self) -> bool {
    self.textarea.is_selecting()
}

pub fn selected_text(&self) -> Option<String> {
    let ((r1, c1), (r2, c2)) = self.textarea.selection_range()?;
    let lines = self.textarea.lines();
    if r1 == r2 {
        let line = lines.get(r1)?;
        let chars: Vec<char> = line.chars().collect();
        let end = (c2 + 1).min(chars.len());
        Some(chars[c1..end].iter().collect())
    } else {
        let mut result = Vec::new();
        // First line
        if let Some(line) = lines.get(r1) {
            let chars: Vec<char> = line.chars().collect();
            result.push(chars[c1..].iter().collect::<String>());
        }
        // Middle lines
        for i in (r1 + 1)..r2 {
            if let Some(line) = lines.get(i) {
                result.push(line.to_string());
            }
        }
        // Last line
        if let Some(line) = lines.get(r2) {
            let chars: Vec<char> = line.chars().collect();
            let end = (c2 + 1).min(chars.len());
            result.push(chars[..end].iter().collect::<String>());
        }
        Some(result.join("\n"))
    }
}

pub fn select_all(&mut self) {
    self.textarea.select_all();
}
```

### Composer Selection Style
```rust
// In Composer::make_textarea():
textarea.set_selection_style(Style::default().bg(Color::Cyan).fg(Color::Black));
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Custom selection for all panes | tui-textarea built-in selection for composer | tui-textarea 0.4+ | No custom SelectionState needed for composer |
| tui-textarea had no selection | tui-textarea 0.4+ added selection API | 2024 | select_all, selection_range, is_selecting available |

## Open Questions

1. **selection_range() col offset semantics**
   - What we know: Returns `((usize, usize), (usize, usize))` as (row, col) pairs
   - What's unclear: Whether col is byte offset, char offset, or grapheme index for non-ASCII text
   - Recommendation: Test empirically with multi-byte content during implementation. If col is char-based, the extraction code above works. If byte-based, adjust with `.char_indices()`.

2. **Copy feedback flash for composer selection**
   - What we know: Output pane selection highlight is rendered in render.rs; flash can modify the style
   - What's unclear: tui-textarea renders its own selection -- we control style via `set_selection_style()` but can't flash per-frame without calling it each render
   - Recommendation: Call `set_selection_style()` with flash color during the flash window, then revert. This is called per render anyway.

3. **Dimmed selection in unfocused composer**
   - What we know: We can call `set_selection_style()` when focus changes
   - What's unclear: Whether calling it while selection is active properly updates the rendered style
   - Recommendation: Set selection style in `set_active()` method -- active gets cyan/black, inactive gets dimmed gray

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (Rust built-in) |
| Config file | Cargo.toml workspace config |
| Quick run command | `cargo test --package kommand0-tui` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| CLIP-01 | Copy selected text to clipboard | unit + manual | `cargo test --package kommand0-tui -- composer::tests` | Wave 0 (composer extraction) |
| CLIP-02 | No-op when no selection | unit | `cargo test --package kommand0-tui -- clipboard::tests` | Partial (bridge exists) |
| KEYS-01 | Ctrl+Q stops session | manual-only | N/A (requires running session + terminal) | N/A |
| KEYS-02 | Old Ctrl+C behavior removed | manual-only | N/A (requires full app interaction) | N/A |
| COMP-01 | Shift+arrow selection in composer | unit | `cargo test --package kommand0-tui -- composer::tests` | Wave 0 |
| COMP-02 | Ctrl+A select all in composer | unit | `cargo test --package kommand0-tui -- composer::tests` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test --package kommand0-tui`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `apps/tui/src/composer.rs` -- add tests for `has_selection()`, `selected_text()`, `select_all()` methods
- [ ] Verify `selection_range()` col semantics with multi-byte text in a test

## Sources

### Primary (HIGH confidence)
- [tui-textarea docs.rs](https://docs.rs/tui-textarea/0.7.0/tui_textarea/struct.TextArea.html) - Verified selection API: start_selection, select_all, selection_range, copy, yank_text, set_selection_style, is_selecting
- [tui-textarea module docs](https://docs.rs/tui-textarea/0.7.0/tui_textarea/index.html) - Default key bindings: Ctrl+A = line start (not select all), Ctrl+C = copy to yank buffer
- Codebase inspection - clipboard.rs, selection.rs, main.rs key handlers, composer.rs, render.rs, mouse.rs

### Secondary (MEDIUM confidence)
- [tui-textarea GitHub](https://github.com/rhysd/tui-textarea) - Shift+arrow triggers selection when passed through input()
- [tui-textarea README](https://github.com/rhysd/tui-textarea/blob/main/README.md) - Feature list confirms text selection support

### Tertiary (LOW confidence)
- selection_range() col offset semantics -- docs don't specify byte vs char; needs empirical testing

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - all crates already in project, APIs verified via docs.rs
- Architecture: HIGH - key dispatch flow clearly understood from codebase reading, tui-textarea API verified
- Pitfalls: HIGH - identified from direct codebase analysis and API documentation comparison

**Research date:** 2026-03-28
**Valid until:** 2026-04-28 (stable crates, no fast-moving dependencies)
