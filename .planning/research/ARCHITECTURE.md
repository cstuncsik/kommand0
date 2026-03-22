# Architecture Research: TUI Text Selection & Clipboard

**Research Date:** 2026-03-22
**Confidence:** MEDIUM

## Scope

Purely a TUI-layer concern — zero changes needed to `crates/core`. All new code lives in `apps/tui/src/`.

## New Components

### 1. SelectionState

**Purpose:** Track cursor position, selection anchor, and selection range in logical coordinates.
**Location:** New file `apps/tui/src/selection.rs`
**Coordinates:** Logical (line_index, char_offset) — survives resize and scroll.

```
struct OutputCursor {
    line: usize,      // index into scrollback lines
    col: usize,       // character offset (unicode-aware)
    visible: bool,    // blink state
}

struct Selection {
    anchor: (usize, usize),  // (line, col) where selection started
    cursor: (usize, usize),  // (line, col) current end
}
```

**Used by:** Key handler, mouse handler, render pipeline, clipboard copy.

### 2. WrapMap (Critical Path)

**Purpose:** Map visual screen rows to logical text positions, enabling screen→text coordinate translation.
**Location:** New file or module in `apps/tui/src/wrap_map.rs`

```
struct WrapMap {
    /// For each visual row in viewport: (logical_line_index, char_start_offset)
    rows: Vec<(usize, usize)>,
    /// Viewport dimensions used to compute this map
    width: u16,
}

impl WrapMap {
    fn from_lines(lines: &[Line], width: u16) -> Self;
    fn screen_to_logical(&self, row: u16, col: u16) -> (usize, usize);
    fn logical_to_screen(&self, line: usize, col: usize) -> Option<(u16, u16)>;
}
```

**Critical risk:** Must precisely replicate ratatui's `Paragraph` wrapping algorithm using `unicode-width`. Mismatch = selection highlights wrong characters.

**Performance:** Rebuilt per-frame for viewport-only lines (~50 lines). Sub-millisecond, no caching needed.

### 3. ClipboardBridge

**Purpose:** Wrapper around `arboard::Clipboard` for system clipboard access.
**Location:** New file `apps/tui/src/clipboard.rs`

```
struct ClipboardBridge {
    clipboard: Option<arboard::Clipboard>,
}

impl ClipboardBridge {
    fn new() -> Self;  // graceful fallback if clipboard unavailable
    fn copy(&mut self, text: &str) -> Result<()>;
}
```

**Notes:** Store as long-lived instance in App struct. Initialize once on startup.

### 4. Highlight Renderer

**Purpose:** Modify `Line<'static>` spans to apply selection highlight before Paragraph render.
**Location:** Integrated into existing `render.rs` (`build_output_lines()`)

**Approach:** Span re-styling (not overlay). For each line overlapping selection:
1. Find selection start/end character offsets for this line
2. Split spans at selection boundaries
3. Apply cyan bg / black fg to selected spans
4. Preserve original styling on non-selected portions

## Data Flow

```
User Input (key/mouse event)
    │
    ▼
Event Handler (main.rs / mouse.rs)
    │ updates
    ▼
SelectionState (selection.rs)
    │ read by
    ▼
Render Pipeline (render.rs)
    │ uses
    ├── WrapMap (coordinate translation)
    └── Highlight Renderer (span re-styling)
    │ produces
    ▼
Styled Paragraph with selection highlights

Copy Action (Ctrl+C / Cmd+C)
    │ reads
    ├── SelectionState (range)
    ├── ScrollbackBuffer (text content)
    └── ClipboardBridge (system clipboard)
```

## Files to Modify

| File | Changes |
|------|---------|
| `apps/tui/src/selection.rs` | **NEW** — SelectionState, OutputCursor, Selection structs |
| `apps/tui/src/wrap_map.rs` | **NEW** — WrapMap for coordinate translation |
| `apps/tui/src/clipboard.rs` | **NEW** — ClipboardBridge wrapper |
| `apps/tui/src/render.rs` | Add selection highlight rendering to `build_output_lines()` |
| `apps/tui/src/main.rs` | Add SelectionState/ClipboardBridge to App, key handlers for cursor/selection/copy, Ctrl+Q for stop |
| `apps/tui/src/mouse.rs` | Add MouseUp handler, drag-to-select logic |
| `apps/tui/src/composer.rs` | Enable tui-textarea selection, bridge to ClipboardBridge |
| `apps/tui/src/scrollback.rs` | Add text extraction method for selection range |
| `apps/tui/Cargo.toml` | Add `arboard` dependency |

## Build Order (by dependencies)

1. **SelectionState** — pure data, no deps, unit testable
2. **WrapMap** — hardest component, build early to de-risk
3. **Highlight rendering** — depends on SelectionState + WrapMap
4. **Mouse selection** — depends on WrapMap + SelectionState
5. **Keyboard selection** — depends on SelectionState
6. **ClipboardBridge** — depends on SelectionState (for text extraction)
7. **Ctrl+C/Ctrl+Q remapping** — depends on ClipboardBridge + SelectionState
8. **Composer selection** — investigate tui-textarea first, may be nearly free

## Open Questions

- Does ratatui 0.29's `Paragraph` with `Wrap { trim: false }` wrap purely on unicode width, or has word-wrap heuristics? WrapMap must match exactly.
- Does tui-textarea v0.7 have working built-in selection? If yes, composer selection is free.
- How does `arboard` behave on headless Linux (CI, SSH)? Need graceful fallback.

---
*Architecture research: 2026-03-22*
