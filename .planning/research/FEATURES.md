# Features Research

**Project:** Inline Tree Icon-Buttons for kommand0 TUI
**Confidence:** HIGH

## Table Stakes (must have or interaction feels broken)

| Feature | Complexity | Dependencies |
|---------|-----------|-------------|
| State-dependent icon visibility (different icons per session state) | Low | Session state lookup |
| Click-to-act (click icon → execute action) | Medium | Hit regions, action dispatch |
| Hover highlight (cyan on hover, matching existing buttons) | Low | Existing mouse_pos tracking |
| Workspace name truncation (when icons need space) | Medium | Unicode-safe width calculation |
| Layout-safe positioning (relative to tree pane width) | Medium | Pane area dimensions |
| Keyboard parity (icons don't add actions unavailable via keyboard) | None | Already implemented |

## Differentiators (polish that elevates UX)

| Feature | Complexity | Dependencies |
|---------|-----------|-------------|
| Animated braille spinner for thinking state | Low | Existing tick mechanism |
| Right-alignment with gap fill (clean visual alignment) | Medium | Width calculation |
| Focus-composer icon (▶ on running session focuses input) | Low | Existing focus system |
| Tooltip on hover (show action name) | Medium | New tooltip rendering |
| Graceful narrow-width degradation (hide icons if too narrow) | Low | Width threshold check |

## Anti-Features (deliberately NOT building)

| Feature | Reason |
|---------|--------|
| Right-click context menu | v2 — ship inline icons first, validate pattern |
| Drag-to-reorder workspaces | Not requested, high complexity |
| Icon customization/theming | Unnecessary complexity for TUI |
| Inline text editing on rows | Confuses click semantics |
| Per-icon keyboard shortcuts | Already have global shortcuts |
| Icons on repo rows | Repos don't have session actions |

## MVP Definition

Table stakes + animated spinner + focus-composer icon = shippable v1.

## Feature Dependencies

```
Layout-safe positioning
  └─ Name truncation
       └─ State-dependent icons
            ├─ Click-to-act (needs hit regions)
            │    └─ Hover highlight
            └─ Animated spinner
```

---
*Research: 2026-03-12*
