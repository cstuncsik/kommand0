# Stack Research

**Project:** Inline Tree Icon-Buttons for kommand0 TUI
**Confidence:** HIGH

## Key Findings

### No New Dependencies Needed

The existing stack (ratatui 0.29, crossterm 0.28) provides everything required:

- **`Frame::buffer_mut()` + `Buffer::set_span()`** — Exact-position rendering after List widget renders. Solves "left-aligned name + right-aligned icons" cleanly.
- **Two-pass rendering pattern** — Render List normally (Pass 1), overlay icons at calculated right-aligned positions using direct buffer writes (Pass 2). Avoids reimplementing List scrolling/selection.
- **Existing `HitRegion` / `HitAction` system** — Extend `HitAction` with new variants (`FocusComposer`, `RetrySession`), push icon hit regions during render. Click handler already iterates hit regions.

### Why Two-Pass Rendering

`Line` only supports single alignment — you cannot mix left-aligned names and right-aligned icons in a single ListItem span array. Buffer overlay is the standard ratatui workaround.

### Scroll Offset Handling

After `render_stateful_widget`, use `list_state.offset()` to determine which tree items are visible, so icon overlay positions match the scrolled list rows.

## Recommendations

| Component | Recommendation | Confidence |
|-----------|---------------|------------|
| Rendering | Two-pass: List widget + buffer overlay for icons | HIGH |
| Hit regions | Extend existing HitAction enum, reuse hit_regions vec | HIGH |
| Icon positioning | `Buffer::set_span()` at calculated right-aligned x | HIGH |
| Scroll sync | `ListState::offset()` for visible row mapping | HIGH |
| Unicode glyphs | ▶ ■ ↺ ⠸ — needs terminal testing (iTerm2, Terminal.app) | MEDIUM |
| Name truncation | Truncate in Pass 1 to avoid Unicode corruption at overlay boundaries | HIGH |

## What NOT to Use

- **Custom widget replacing List** — Too much reimplementation of scroll/selection logic
- **Paragraph widget per row** — Performance overhead, doesn't integrate with tree selection
- **New dependencies** — ratatui's built-in APIs are sufficient

## Roadmap Implications

- Purely additive change to `render.rs` and `buttons.rs` — no architectural changes needed
- Buffer overlay pattern is standard ratatui approach for mixed-alignment content
- Existing mouse/hover infrastructure (`mouse_pos`, `is_hovered`) means hover highlighting comes for free

---
*Research: 2026-03-12*
