# Pitfalls Research

**Project:** Inline Tree Icon-Buttons for kommand0 TUI
**Confidence:** HIGH

## Critical Pitfalls

### 1. Hit Region Coordinates Drift from Rendered Content

**Risk:** HIGH
**What:** Ratatui's `List` widget renders items internally and never reports back where spans land. Hit regions must be calculated separately, creating two sources of truth. The existing `buttons.rs` uses `.len()` (byte length) instead of display width, which will break with multi-byte unicode tree connectors already in use.

**Warning signs:** Icons clickable but wrong action fires; click lands on adjacent icon.
**Prevention:** Use `unicode_width::UnicodeWidthStr` for all width calculations. Compute hit region x-positions from the same width values used for rendering.
**Phase:** Must be correct from Phase 1.

### 2. List Widget Cannot Right-Align Spans

**Risk:** HIGH
**What:** `Line` only supports a single alignment direction. Icons must be positioned using calculated fill spaces within each `Line`, or via buffer overlay.

**Warning signs:** Icons appear left-justified or overlap workspace names.
**Prevention:** Calculate fill width = available_width - prefix_width - name_width - icon_width. Insert padding `Span` between name and icons.
**Phase:** Phase 1 core rendering.

### 3. `truncate_path` Panics on Non-ASCII

**Risk:** HIGH
**What:** The existing function at `render.rs:13-22` slices by byte offset. This is a latent bug that will surface when workspace names need truncation to make room for icons.

**Warning signs:** Panic on workspace names with unicode characters.
**Prevention:** Replace byte slicing with `char_indices()` or `unicode-segmentation` based truncation.
**Phase:** Phase 1 — fix before adding truncation for icons.

### 4. Action-Target Mismatch (HIGHEST RISK)

**Risk:** CRITICAL
**What:** The current `handle_click` checks hit regions before updating `selected_index`, so clicking an icon on an unselected row would act on the wrong workspace. `HitAction` must carry the workspace ID.

**Warning signs:** Silent wrong-workspace actions — user clicks stop on workspace A, workspace B stops.
**Prevention:** Include `workspace_id: String` in each icon HitAction variant. Dispatch action using the carried ID, not the selected workspace.
**Phase:** Phase 1 — must be designed correctly from the start.

### 5. Stale Hit Regions After Scroll

**Risk:** MODERATE
**What:** The existing `hit_regions.clear()` at render start is correct, but icon hit regions must be recalculated on every render pass (they move with scroll).

**Warning signs:** Click on icon after scrolling hits wrong target or nothing.
**Prevention:** Always rebuild icon hit regions during render. Never cache across frames.
**Phase:** Phase 1 — inherent to the rendering approach.

## Moderate Pitfalls

### 6. Terminal Font Rendering of Unicode Icons

**Risk:** MODERATE
**What:** Unicode symbols like ▶ ■ ↺ ⠸ may render as double-width or invisible in some terminal emulators, breaking layout calculations.

**Warning signs:** Icons overlap text or leave gaps in specific terminals.
**Prevention:** Test in iTerm2, Terminal.app, and common terminal emulators. Have ASCII fallback ready (> X R ~).
**Phase:** Phase 2 polish.

### 7. Performance with Many Workspaces

**Risk:** LOW
**What:** Two-pass rendering (List + buffer overlay) doubles the rendering work for workspace rows. With many workspaces, this could impact the 20 FPS redraw cycle.

**Warning signs:** Visible lag when scrolling tree with 50+ workspaces.
**Prevention:** Only overlay icons for visible rows (use `list_state.offset()` + viewport height).
**Phase:** Phase 2 if needed.

---
*Research: 2026-03-12*
