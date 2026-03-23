# Phase 1: Coordinate Translation & Infrastructure - Research

**Researched:** 2026-03-23
**Domain:** Text wrapping coordinate translation, clipboard integration, selection state modeling
**Confidence:** HIGH

## Summary

Phase 1 builds the foundation that all subsequent selection/clipboard work depends on. The central challenge is replicating ratatui's WordWrapper algorithm outside of the render pass so that screen coordinates (x, y) can be mapped back to logical text positions (line index, character offset). The existing codebase has a known bug where `wrapped_line_height()` and `styled_total_visual()` use byte length (`s.content.len()`) instead of display width (`UnicodeWidthStr::width()`), which must be fixed as part of this work.

ratatui's WordWrapper (in `reflow.rs`) operates on grapheme clusters via `unicode-segmentation`, uses display width via `unicode-width`, and handles word boundaries, leading whitespace trimming/preservation, double-width CJK characters, and zero-width joiners. It is `pub(crate)` -- not exposed in ratatui's public API. We must reimplement its wrapping logic to produce a WrapMap that records, for each visual row, which logical line it came from and the character offset range.

The clipboard piece (arboard) is straightforward -- `Clipboard::new()` + `set_text()` covers the requirement. SelectionState is a pure data structure with no external dependencies.

**Primary recommendation:** Build WrapMap first as the highest-risk component, validate it against ratatui's WordWrapper with identical test inputs, then layer SelectionState and ClipboardBridge on top.

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| CORD-01 | Screen position (x,y) correctly maps to text position (line, char) accounting for line wrapping | WrapMap architecture -- reimplements ratatui WordWrapper to produce a bidirectional mapping between visual rows and logical (line, char_offset) positions |
| CORD-02 | Coordinate translation handles unicode characters (CJK, emoji) correctly | Must use `unicode-segmentation` for grapheme clusters + `unicode-width` for display widths, matching ratatui's approach. Existing bug in `styled_total_visual()` uses byte length not display width -- must fix. |
| CORD-03 | Coordinate translation accounts for border padding and scroll offset | Border padding = 1 cell each side (ratatui `Borders::ALL`). Scroll offset converted from bottom-up (`scroll_offset`) to top-down (`scroll_from_top`) already in `render_output_content()`. WrapMap must accept these offsets. |
| CLIP-03 | Clipboard integration works cross-platform via arboard crate | arboard 3.6.x: `Clipboard::new()?.set_text(text)?` -- cross-platform (macOS, Linux, Windows). Thin wrapper needed. |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| unicode-width | 0.2 | Character display width calculation | Already in workspace deps; same version ratatui 0.29 uses internally |
| unicode-segmentation | 1.x | Grapheme cluster iteration | ratatui's WordWrapper uses this internally; we must match its behavior |
| arboard | 3.6 | Cross-platform clipboard access | Maintained by 1Password, covers macOS/Linux/Windows |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| ratatui | 0.29 | TUI framework (existing) | Already in use; provides Rect, Line, Span types |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| arboard | cli-clipboard, copypasta | arboard is more actively maintained, 1Password-backed |
| unicode-segmentation | manual char iteration | Would diverge from ratatui's wrapping logic, causing mismatches |

**Installation:**
```bash
cargo add arboard --manifest-path apps/tui/Cargo.toml
cargo add unicode-segmentation --manifest-path apps/tui/Cargo.toml
```

Note: `unicode-segmentation` may need to go in the workspace `[dependencies]` since the WrapMap logic could live in a shared module.

## Architecture Patterns

### Recommended Project Structure
```
apps/tui/src/
  wrap_map.rs        # WrapMap: wrapping logic + coordinate translation
  selection.rs       # SelectionState: anchor/cursor/range modeling
  clipboard.rs       # ClipboardBridge: thin arboard wrapper
  render.rs          # Existing -- fix styled_total_visual bug, integrate WrapMap
  main.rs            # Existing -- add new modules, wire into App
```

### Pattern 1: WrapMap (Core Component)

**What:** A data structure that records the mapping between visual rows and logical text positions, built by running the same wrapping algorithm ratatui uses.

**When to use:** Every time the output pane content or width changes (resize, new content, scroll).

**Design:**

```rust
/// One visual row in the output pane after wrapping.
#[derive(Debug, Clone)]
pub struct VisualRow {
    /// Index into the logical lines array (pre-wrap)
    pub logical_line: usize,
    /// Byte offset within the logical line where this visual row starts
    pub start_byte: usize,
    /// Byte offset within the logical line where this visual row ends (exclusive)
    pub end_byte: usize,
    /// Display width of this visual row
    pub width: u16,
}

/// Maps between screen coordinates and logical text positions.
pub struct WrapMap {
    rows: Vec<VisualRow>,
    pane_width: usize,
}

impl WrapMap {
    /// Build from raw text lines at a given pane width.
    /// Must replicate ratatui's WordWrapper with trim=false.
    pub fn build(lines: &[&str], pane_width: usize) -> Self { ... }

    /// Screen (x, y) relative to pane inner area -> (logical_line, char_offset)
    /// y is visual row index (0-based from top of visible area)
    /// x is column within that row
    pub fn screen_to_logical(&self, x: u16, y: u16, scroll_from_top: usize) -> Option<(usize, usize)> { ... }

    /// (logical_line, char_offset) -> (visual_row, x_column)
    pub fn logical_to_screen(&self, line: usize, char_offset: usize, scroll_from_top: usize) -> Option<(u16, u16)> { ... }

    /// Total visual row count (for scroll calculations)
    pub fn total_visual_rows(&self) -> usize { ... }

    /// Extract text between two logical positions (for copy)
    pub fn extract_text(&self, lines: &[&str], start: (usize, usize), end: (usize, usize)) -> String { ... }
}
```

**Critical detail:** The wrapping algorithm MUST operate on grapheme clusters with display widths, not bytes or chars. ratatui's WordWrapper:
1. Iterates grapheme clusters (via `unicode-segmentation`)
2. Measures display width (via `UnicodeWidthStr::width()`)
3. Breaks on word boundaries (whitespace transitions)
4. When a word exceeds max width, breaks at width limit
5. With `trim: false`, preserves leading whitespace on wrapped lines
6. Skips graphemes wider than max_line_width entirely

### Pattern 2: SelectionState

**What:** Pure data structure representing selection state in the output pane.

```rust
#[derive(Debug, Clone, Default)]
pub enum SelectionState {
    #[default]
    None,
    /// Cursor visible but no range selected
    Cursor {
        line: usize,
        char_offset: usize,
    },
    /// Active selection range
    Range {
        anchor_line: usize,
        anchor_char: usize,
        cursor_line: usize,
        cursor_char: usize,
    },
}

impl SelectionState {
    pub fn is_none(&self) -> bool { ... }
    pub fn has_range(&self) -> bool { ... }
    /// Returns (start, end) in document order regardless of anchor/cursor direction
    pub fn ordered_range(&self) -> Option<((usize, usize), (usize, usize))> { ... }
    pub fn clear(&mut self) { *self = Self::None; }
}
```

### Pattern 3: ClipboardBridge

**What:** Thin wrapper around arboard that handles initialization failures gracefully.

```rust
pub struct ClipboardBridge {
    clipboard: Option<arboard::Clipboard>,
}

impl ClipboardBridge {
    pub fn new() -> Self {
        Self {
            clipboard: arboard::Clipboard::new().ok(),
        }
    }

    pub fn set_text(&mut self, text: &str) -> Result<(), String> {
        match &mut self.clipboard {
            Some(cb) => cb.set_text(text.to_string())
                .map_err(|e| e.to_string()),
            None => Err("Clipboard not available".to_string()),
        }
    }
}
```

### Anti-Patterns to Avoid
- **Using byte length for width calculations:** The existing `styled_total_visual()` uses `s.content.len()` (bytes) instead of display width. CJK characters are 2 display columns but 3 bytes in UTF-8. Emoji with joiners can be 4+ bytes but 2 display columns.
- **Using `char` count for width:** Rust `char` is a Unicode scalar value, not a grapheme cluster. Emoji like family emoji are multiple chars but 2 display columns.
- **Building WrapMap over entire scrollback:** Only build for visible lines + small buffer. 50k lines would be wasteful.
- **Assuming 1 char = 1 column:** Must always use `UnicodeWidthChar::width()` or `UnicodeWidthStr::width()`.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Grapheme cluster splitting | Manual char-by-char iteration | `unicode-segmentation::UnicodeSegmentation::graphemes()` | Grapheme clusters span multiple Unicode scalar values (emoji sequences, combining marks) |
| Display width calculation | Counting chars or bytes | `unicode-width::UnicodeWidthStr::width()` / `UnicodeWidthChar::width()` | CJK = 2 columns, zero-width joiners = 0, combining marks = 0 |
| System clipboard access | shelling out to pbcopy/xclip | `arboard::Clipboard` | Cross-platform, handles Wayland, X11, macOS, Windows |
| Word boundary detection | Regex or manual splitting | Match ratatui's whitespace-based approach in WordWrapper | Must produce identical results |

**Key insight:** The WrapMap MUST produce identical line breaks to ratatui's WordWrapper. Any divergence means screen coordinates will map to wrong text positions. The wrapping logic itself must be hand-built (since WordWrapper is private), but it must match ratatui's algorithm exactly.

## Common Pitfalls

### Pitfall 1: Byte Length vs Display Width Mismatch
**What goes wrong:** Coordinate translation returns wrong positions for any non-ASCII text. Scroll calculations are off.
**Why it happens:** Easy to use `.len()` (bytes) or `.chars().count()` instead of `UnicodeWidthStr::width()`.
**How to avoid:** Use display-width functions everywhere. Ban raw `.len()` on text content in code review. Fix the existing bug in `styled_total_visual()` first.
**Warning signs:** Selection highlight misaligned on CJK text, scroll jumps on emoji content.

### Pitfall 2: Grapheme Cluster vs Char Boundary
**What goes wrong:** String slicing panics or produces wrong results when splitting at char boundaries that break grapheme clusters.
**Why it happens:** Emoji like flags, family emoji, and characters with combining marks are multiple Unicode scalar values but one visual unit.
**How to avoid:** Always iterate grapheme clusters. Store byte offsets from grapheme iteration for slicing.
**Warning signs:** Panics on emoji text, garbled characters when selecting emoji.

### Pitfall 3: WrapMap Divergence from ratatui
**What goes wrong:** Screen position maps to wrong text -- off by one line or several characters.
**Why it happens:** Subtle differences in how word boundaries, whitespace trimming, or overflow are handled.
**How to avoid:** Port ratatui's WordWrapper logic directly, preserving its structure. Test with identical inputs and compare output line counts and break positions.
**Warning signs:** Tests pass for ASCII but fail for CJK/mixed-width, or fail for long words that must break mid-word.

### Pitfall 4: Markdown Styling Changes Text Layout
**What goes wrong:** `style_markdown_line()` transforms text before display (e.g., `"> text"` becomes right-aligned with padding, headers strip `# ` prefix, code blocks add `"  "` indent).
**Why it happens:** WrapMap must account for the styled output, not the raw scrollback text.
**How to avoid:** WrapMap should operate on the styled Line spans, not raw text. Or maintain a parallel mapping from styled positions back to raw positions.
**Warning signs:** Clicking on a header selects text offset by 2-3 characters.

### Pitfall 5: Scroll Offset Direction
**What goes wrong:** Coordinates are off by the entire viewport height.
**Why it happens:** `ScrollbackBuffer.scroll_offset` counts from the bottom (0 = at bottom). ratatui's `Paragraph::scroll` counts from the top. The conversion already exists in `render_output_content()` but WrapMap must use the same convention.
**How to avoid:** Clearly document which convention each function uses. Convert at the boundary.
**Warning signs:** Selection works only when not scrolled, breaks when scrolled up.

## Code Examples

### Existing Bug Fix: styled_total_visual

Current broken code (render.rs line 854-861):
```rust
// BUG: uses byte length, not display width
fn styled_total_visual(lines: &[Line], inner_width: usize) -> usize {
    lines.iter()
        .map(|l| {
            let len: usize = l.spans.iter().map(|s| s.content.len()).sum();
            wrapped_line_height(len, inner_width)
        })
        .sum()
}
```

Fixed version:
```rust
fn styled_total_visual(lines: &[Line], inner_width: usize) -> usize {
    lines.iter()
        .map(|l| {
            let display_width: usize = l.spans.iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            wrapped_line_height(display_width, inner_width)
        })
        .sum()
}
```

Note: Even the fixed version is an approximation -- it uses simple division which doesn't account for word boundaries. The accurate approach is to run the WordWrapper and count output lines, which is what WrapMap will do.

### wrapped_line_height Also Needs Fixing

The existing `wrapped_line_height` (line 596-601) does simple ceiling division:
```rust
fn wrapped_line_height(line_len: usize, width: usize) -> usize {
    if width == 0 || line_len == 0 { return 1; }
    (line_len + width - 1) / width
}
```

This is only correct for lines with no word boundaries (long single words). For word-wrapped text, actual visual lines depend on where breaks land. Once WrapMap exists, `styled_total_visual` should use `wrap_map.total_visual_rows()` instead.

### ratatui WordWrapper Core Logic (Simplified Reference)

Key behavior to replicate (from `reflow.rs`, with `trim: false`):

1. Iterate grapheme clusters of each span
2. Track `pending_word` and `pending_whitespace` buffers
3. On whitespace-to-word transition: flush pending word to current line
4. On line overflow: push current line to output, start new line
5. When a single word exceeds max width: break at width boundary
6. With `trim: false`: preserve leading whitespace on continuation lines
7. Empty input lines produce one empty visual row

### arboard Usage

```rust
use arboard::Clipboard;

// Initialize (can fail on headless systems)
let mut clipboard = Clipboard::new()?;

// Set text
clipboard.set_text("copied text".to_string())?;

// Get text (not needed for Phase 1 but shown for completeness)
let text = clipboard.get_text()?;
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `unicode-width` 0.1 | `unicode-width` 0.2 | 2024 | New emoji widths, CJK variant selectors |
| ratatui Paragraph without line_count | `Paragraph::line_count()` (unstable) | ratatui 0.28+ | Could be used to verify WrapMap total, but requires unstable feature flag |
| arboard 2.x | arboard 3.6 | 2024 | API stability, Wayland improvements |

**Key note on ratatui's `line_count()`:** ratatui 0.29 has an unstable `rendered-line-info` feature that exposes `Paragraph::line_count(width)`. This runs WordWrapper internally and returns the count. We could use this as a **verification tool** in tests -- compare our WrapMap's total row count against `Paragraph::line_count()`. However, it only gives total count, not per-row mappings, so we still need our own WrapMap.

## Open Questions

1. **Where should WrapMap live?**
   - What we know: It's TUI-specific (depends on ratatui types) but could be tested independently
   - What's unclear: Whether it should go in `apps/tui/src/wrap_map.rs` or a shared location
   - Recommendation: Keep in `apps/tui/src/wrap_map.rs` -- it's TUI-specific. CLAUDE.md says "keep shared domain logic in core crates" but wrapping is purely a display concern.

2. **Should WrapMap operate on raw text or styled Lines?**
   - What we know: `style_markdown_line()` transforms text (adds padding for `>` blocks, strips `#` prefixes, adds `"  "` for code blocks). Display width of styled output differs from raw text.
   - What's unclear: Whether to run wrapping on pre-styled or post-styled text
   - Recommendation: Operate on the same styled Line spans that ratatui receives. This ensures width calculations match. Store a mapping back to raw scrollback positions for text extraction.

3. **Performance: rebuild WrapMap on every frame?**
   - What we know: Only visible lines matter (inner_height + some buffer). Typical viewport is ~30-50 lines.
   - What's unclear: Whether streaming content causes excessive rebuilds
   - Recommendation: Cache WrapMap keyed by (content hash, pane width). Invalidate on content change or resize. For Phase 1, rebuild on change is fine -- optimize later if profiling shows issues.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust built-in `#[cfg(test)]` + `cargo test` |
| Config file | None needed -- Cargo workspace handles it |
| Quick run command | `cargo test -p kommand0-tui` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| CORD-01 | Screen (x,y) maps to logical (line, char) with wrapping | unit | `cargo test -p kommand0-tui wrap_map -- --nocapture` | No -- Wave 0 |
| CORD-02 | Unicode (CJK, emoji) coordinate translation | unit | `cargo test -p kommand0-tui wrap_map::tests::cjk -- --nocapture` | No -- Wave 0 |
| CORD-03 | Border padding + scroll offset accounted for | unit | `cargo test -p kommand0-tui wrap_map::tests::scroll -- --nocapture` | No -- Wave 0 |
| CLIP-03 | arboard clipboard init + write | unit + manual | `cargo test -p kommand0-tui clipboard -- --nocapture` | No -- Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p kommand0-tui`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before verification

### Wave 0 Gaps
- [ ] `apps/tui/src/wrap_map.rs` -- WrapMap struct + tests covering ASCII wrap, CJK wrap, emoji wrap, scroll offset, word boundary breaks
- [ ] `apps/tui/src/selection.rs` -- SelectionState tests for None/Cursor/Range transitions and ordered_range
- [ ] `apps/tui/src/clipboard.rs` -- ClipboardBridge basic init test (set_text is platform-dependent, may need `#[ignore]` on CI)
- [ ] Verification tests comparing WrapMap row count against ratatui `Paragraph::line_count()` (requires enabling unstable feature)

## Sources

### Primary (HIGH confidence)
- ratatui 0.29.0 source: `reflow.rs` WordWrapper implementation -- read directly from cargo registry
- ratatui 0.29.0 source: `paragraph.rs` render_paragraph + line_count -- read directly from cargo registry
- Project source: `render.rs`, `scrollback.rs`, `mouse.rs`, `main.rs` -- read directly

### Secondary (MEDIUM confidence)
- [arboard docs.rs](https://docs.rs/arboard) -- API: Clipboard::new(), set_text(), v3.6.1
- [arboard GitHub](https://github.com/1Password/arboard) -- Platform support, maintenance status

### Tertiary (LOW confidence)
- None

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- read ratatui source directly, verified unicode-width/segmentation usage
- Architecture: HIGH -- WrapMap design directly follows ratatui's WordWrapper structure
- Pitfalls: HIGH -- identified from reading actual source code and existing bugs
- Clipboard: HIGH -- arboard API is minimal and well-documented

**Research date:** 2026-03-23
**Valid until:** 2026-04-23 (stable domain, no fast-moving dependencies)
