# Phase 4: UX Polish - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

The TUI has consistent keyboard navigation, contextual help, focused pane management, a zoomed output view, improved chat message visibility, fixed scrolling, and composer polish. This phase does NOT add new CRUD operations in TUI (repo/workspace add/delete/archive), git worktree integration, Claude Code autocomplete, or slash command passthrough — those are future phases.

</domain>

<decisions>
## Implementation Decisions

### Key binding scheme
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

### Shift+Enter fix
- Shift+Enter in Composer must add a newline (currently sends message — this is a bug)
- Enter sends the message

### Help overlay
- Triggered by '?' key (toggle: ? opens, ? or Esc closes)
- Centered modal overlay on top of TUI (like vim's :help), semi-transparent dark background
- Dark background box with bright text, grouped by section with headers
- Shows which pane is currently active at top (e.g. "Help — Tree Pane (highlighted)")
- Context-aware: highlights keys relevant to current focus, but shows all keys
- Key format: bracketed — [Tab] Switch pane  [j/k] Navigate  [?] Help
- No persistent status bar — help overlay is sufficient
- Mention mouse support placeholder for future

### Pane focus indicators
- Focused pane: cyan border color + cyan title text
- Unfocused panes: gray/dark borders, clearly receded but visible
- Same border style (no thick/double-line change), only color differs
- Tree pane: selected item always visible with background highlight; dimmed when Tree unfocused
- Composer: blinking cursor when focused, plus cyan border

### Zoomed output view
- 'z' key triggers zoom (only when Output pane is focused)
- Shows: output + composer pinned at bottom + minimal status bar
- Status bar content: workspace name + session status + scroll position (e.g. "my-feature ▶ line 142/500")
- Exit zoom: 'z' again OR Esc
- Instant snap transition (no animation)

### Chat message visibility
- Chat bubble style: user messages right-aligned with subtle background color
- Claude output left-aligned, plain (no background)
- No labels ("You:", "Claude:") — alignment + background is the distinction
- No colored left-border stripes

### Scrolling fix
- Fix both auto-scroll and manual scroll (both currently broken)
- Auto-scroll: always tail new output when at bottom
- Manual scroll: j/k/arrows line-by-line, Page Up/Down pages, g/G and Home/End for top/bottom
- Scrollbar: thin unicode block character track (vim-style, minimal)
- Scrollbar only visible when content exceeds viewport

### Session status colors
- Red accent for failed sessions
- Yellow for stopped sessions
- Green for running sessions
- Apply to status indicators in tree view and pane borders/titles when relevant

### Error/empty states
- Keep existing behavior but apply new focus/color scheme consistently
- No new empty state designs — just visual consistency with the polish

### Composer appearance
- Placeholder text: "Type a message..." when empty
- Auto-expand height: starts small, grows up to 6 lines, then scrolls internally
- Own border — separate box with horizontal divider from output
- Small character/line count in bottom-right corner

### Claude's Discretion
- Exact chat bubble background color (subtle, not distracting)
- Scrollbar unicode characters and exact track style
- Help overlay dimensions and padding
- Auto-expand step behavior (grow per-line or in chunks)
- Exact gray shade for unfocused borders
- How to detect "at bottom" for auto-scroll resume

</decisions>

<specifics>
## Specific Ideas

- Chat bubble style inspired by common AI chat apps (ChatGPT, Claude web) — user messages right-aligned with background
- Help overlay should feel like vim's :help — informative but not cluttered
- Focus indicators should make it instantly obvious which pane you're in, even at a glance
- Zoom mode is for reading long output — status bar shows position so you know where you are
- Instant transitions everywhere — this is a keyboard-first power-user tool, no animations

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `Focus` enum (apps/tui/src/main.rs): Already has Tree, Output, Composer — extend with zoom state or use separate bool
- `Composer` widget (apps/tui/src/composer.rs): Wraps tui-textarea — modify for auto-expand, placeholder, Shift+Enter fix
- `ScrollbackBuffer` (apps/tui/src/scrollback.rs): VecDeque-based — fix scroll tracking, add scrollbar position calculation
- `SessionManager` (apps/tui/src/session_manager.rs): Session lifecycle — wire status colors from SessionStatus enum
- `SessionStatus` enum (crates/core/src/session.rs): Running/Stopped/Failed/Exited — map to green/yellow/red colors

### Established Patterns
- `tokio::select!` event loop (apps/tui/src/main.rs): Key handling dispatched by Focus state — extend for new bindings
- `ui()` function (apps/tui/src/main.rs): Renders all panes — add focus-aware border styling, zoom mode layout
- ratatui `Block::default().borders(Borders::ALL)` — add `.border_style()` based on focus
- `TreeNode` enum for tree view rendering — add session status color to workspace items

### Integration Points
- Key event handler in `run()` — restructure for global vs focused key dispatch
- `ui()` layout — add zoom mode branch (full-screen output+composer+status)
- Composer widget — placeholder text, auto-expand, Shift+Enter newline, char count
- Output rendering — chat bubble alignment, scrollbar widget, scroll position tracking

</code_context>

<deferred>
## Deferred Ideas

- Add repositories from TUI (new CRUD) — future phase
- Add workspaces to repos from TUI — future phase
- Archive/delete/reorder workspaces from TUI — future phase
- x/Delete on repo for cascade delete (repo + workspaces + sessions) — future phase
- Settings for kommand0 root working dir (git worktree base path) — future phase (v2 TREE-01/TREE-02)
- Tab autocomplete like Claude Code — future phase
- '/' key for Claude Code actions/skills passthrough — future phase
- Shell tab completion for workspace names — deferred from Phase 2
- Text selection/copy mode — v2 (ASESS-05)

</deferred>

---

*Phase: 04-ux-polish*
*Context gathered: 2026-03-08*
