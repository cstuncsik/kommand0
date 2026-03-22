# Project Research Summary

**Project:** dalat — TUI Text Selection & Clipboard
**Domain:** Terminal UI interaction — text selection, coordinate mapping, system clipboard
**Researched:** 2026-03-22
**Confidence:** MEDIUM-HIGH

## Executive Summary

This milestone adds text selection and clipboard copy to an existing ratatui TUI. The work splits cleanly into two contexts with very different complexity profiles. Composer (input) selection is nearly free — `tui-textarea` 0.7 already ships full selection support with keyboard and mouse handling built in; the only work is bridging its internal yank buffer to the system clipboard via `arboard`. Output pane selection is significantly harder: ratatui's `Paragraph` widget wraps lines internally but exposes no wrap state, so the implementation must build its own wrap map per frame, translating screen coordinates to logical text positions. This is the critical path and the dominant engineering risk.

The recommended approach is to ship `arboard` as the single new dependency and implement output selection as a new `selection.rs` + `wrap_map.rs` module pair fully contained in `apps/tui/src/`. The core crate is untouched. Build order matters: `WrapMap` first to de-risk the hardest component, then selection state, then rendering, then input handling, then composer. Reversing this order means discovering WrapMap problems late when everything depends on it.

The dominant risks are coordinate system confusion (byte offsets vs. char indices vs. display columns — three different spaces that must never be mixed), wrap map accuracy (the map must exactly replicate ratatui's wrapping or highlights will drift), and the Ctrl+C semantic change (copying must never silently block the user's ability to kill a hung process). All three are manageable with targeted unit tests built early.

## Key Findings

### Recommended Stack

The existing stack already provides everything needed except system clipboard access. `tui-textarea` 0.7 has 12 selection methods including `start_selection()`, `select_all()`, `copy()`, and `set_selection_style()`. `crossterm` 0.28 exposes all required mouse events including `MouseUp` (currently unhandled). `ratatui` 0.29 has no native selection — this must be custom-built.

**Core technologies:**
- `arboard` 3.6: System clipboard read/write — maintained by 1Password, cross-platform, simple API, only new dep needed
- `tui-textarea` 0.7 (existing): Full composer selection — built-in support reduces composer work to a bridge call
- `crossterm` 0.28 (existing): Mouse events — `MouseUp` handler must be added, everything else is already wired
- `unicode-width` (existing): Display column calculation — critical for correct wrap map construction

### Expected Features

All v1 scope is table stakes. Users expect terminal-emulator behavior (mouse drag, Ctrl+C) — not mode-based selection like vim or tmux.

**Must have (table stakes — all v1):**
- Mouse drag selection in output pane — core interaction
- Visual selection highlight (cyan bg / black text) — users expect visual feedback
- Ctrl+C / Cmd+C copies selection to system clipboard — primary copy mechanism
- Keyboard selection (Shift+arrows) in output pane — accessibility and precision
- Select All (Ctrl+A) in focused pane — standard shortcut
- Ctrl+C safety — no-op when no selection active
- Ctrl+Q replaces stop-session — frees Ctrl+C for copy
- Composer selection via tui-textarea — input pane must match output pane behavior

**Should have (v1.x, after stability):**
- Word selection (double-click) — quality of life
- Line selection (triple-click) — quality of life

**Defer (v2+):**
- Selection that survives streaming output — requires anchor recalculation under live content
- Block/column selection — complex custom rendering
- Markdown-aware selection / formatted copy — requires AST integration

**Anti-features — do not build:**
- Cross-pane selection, right-click menus, Paste (Ctrl+V), auto-copy on select, selection persisting across scroll

### Architecture Approach

All new code is confined to `apps/tui/src/`. Zero changes to `crates/core`. Three new files (`selection.rs`, `wrap_map.rs`, `clipboard.rs`) plus modifications to existing `render.rs`, `main.rs`, `mouse.rs`, `composer.rs`, and `scrollback.rs`. The data flow is linear: event handler updates `SelectionState`, render pipeline reads it through `WrapMap` to produce highlighted `Line` spans, and copy action reads selection range from `ScrollbackBuffer` and pushes to `ClipboardBridge`.

**Major components:**
1. `SelectionState` (`selection.rs`) — logical coordinate cursor and anchor; pure data, no dependencies
2. `WrapMap` (`wrap_map.rs`) — per-frame screen-to-logical coordinate map; hardest component, highest risk
3. `ClipboardBridge` (`clipboard.rs`) — long-lived `arboard::Clipboard` wrapper with graceful fallback
4. Highlight Renderer (in `render.rs`) — span splitter that applies selection style at character boundaries

### Critical Pitfalls

1. **Wrap map accuracy** — ratatui wraps internally and exposes nothing. Custom WrapMap must exactly replicate the algorithm using `unicode-width`. A mismatch causes selection highlights to drift from the actual characters. Build WrapMap first and test against CJK, emoji, and tabs before building anything that depends on it.

2. **Unicode coordinate confusion** — byte offsets, char indices, and display columns are three distinct coordinate spaces. Rust strings are UTF-8 bytes, screen positions use display width, and CJK/emoji occupies 2 columns per character. Mixing these silently produces wrong selections on non-ASCII text. Define and enforce explicit conversion functions; never convert implicitly.

3. **Span splitting at selection boundaries** — selection may begin or end mid-span inside a `Vec<Span>`. The splitter must handle split-at-0, split-at-end, and empty spans without panicking. Test the splitter in isolation before integrating with rendering.

4. **Scroll offset mismatch** — screen coords are top-left-relative; scrollback uses lines-from-bottom. Converting between them while accounting for wrapped lines is error-prone. Simplify v1 by clearing selection on scroll and locking auto-scroll when selection is active.

5. **Ctrl+C signal conflict** — crossterm raw mode captures Ctrl+C as a key event, but if the TUI crashes or raw mode exits unexpectedly, Ctrl+C sends SIGINT. Users need an escape hatch. Ctrl+Q for stop-session must be solid before Ctrl+C semantics change, and Ctrl+\ (SIGQUIT) must remain a working kill signal.

## Implications for Roadmap

Based on research, suggested phase structure:

### Phase 1: Foundation — SelectionState, WrapMap, ClipboardBridge
**Rationale:** All subsequent work depends on correct coordinate translation and clipboard access. Building these first creates isolated, unit-testable components and surfaces the highest-risk problem (WrapMap accuracy) before anything depends on it.
**Delivers:** Core data structures and coordinate mapping with no visible UI change
**Addresses:** Coordinate translation pitfall, unicode confusion pitfall, arboard lifetime pitfall
**Avoids:** Discovering WrapMap bugs after mouse/keyboard handlers are already wired to it

### Phase 2: Output Pane Highlight Rendering
**Rationale:** With WrapMap and SelectionState in place, span splitting is the next isolated, testable unit. Visual feedback is needed before selection interaction can be validated.
**Delivers:** Selection highlights visible in output pane when SelectionState contains a range
**Uses:** `WrapMap`, `SelectionState`, `unicode-width`
**Implements:** Highlight Renderer in `render.rs`
**Avoids:** Span-boundary panic pitfall — unit test splitter before integration

### Phase 3: Mouse Selection in Output Pane
**Rationale:** Mouse drag is the primary user interaction. Requires WrapMap (phase 1) and highlight rendering (phase 2) to be verifiable. MouseDown, MouseDrag, MouseUp event handling.
**Delivers:** Mouse drag selects text, highlights appear in real time
**Uses:** `crossterm` MouseUp handler (currently missing), `WrapMap`
**Avoids:** Scroll offset mismatch — clear selection on scroll in v1

### Phase 4: Keyboard Selection and Copy in Output Pane
**Rationale:** Keyboard selection (Shift+arrows) plus Ctrl+A and Ctrl+C copy completes the core output pane feature set. Ctrl+Q must be wired here to safely free Ctrl+C.
**Delivers:** Full keyboard selection, Ctrl+A select-all, Ctrl+C copies to system clipboard, Ctrl+Q stops session
**Uses:** `ClipboardBridge`, `SelectionState`, `ScrollbackBuffer` text extraction
**Avoids:** Ctrl+C signal conflict — Ctrl+Q must be verified before Ctrl+C semantics change

### Phase 5: Composer Selection
**Rationale:** Lower risk than output pane — tui-textarea handles the hard parts. Validate the API actually works, then bridge the internal yank buffer to `ClipboardBridge`.
**Delivers:** Composer input text selection and clipboard copy matching output pane UX
**Uses:** `tui-textarea` selection methods, `ClipboardBridge`
**Avoids:** tui-textarea API assumption pitfall — prototype first; have a fallback plan if API is broken

### Phase Ordering Rationale

- WrapMap must come before any interaction handler — it is the dependency foundation and the highest-risk item
- Rendering before interaction — visual feedback needed to validate selection state during development
- Output pane before composer — output pane is higher risk; composer risk is low and is isolated; do hard things first
- Mouse before keyboard — mouse selection validates coordinate translation end-to-end; keyboard selection only adds state transitions
- Ctrl+Q before Ctrl+C — cannot safely change Ctrl+C semantics until the stop-session escape hatch is in place

### Research Flags

Phases likely needing deeper research during planning:
- **Phase 1 (WrapMap):** ratatui's exact wrapping algorithm is undocumented. Must read `Paragraph` source to verify whether `Wrap { trim: false }` uses pure unicode-width or has word-wrap heuristics. This affects WrapMap correctness.
- **Phase 5 (Composer):** tui-textarea yank buffer access API needs verification. Check if `copy()` result is accessible programmatically and inspect GitHub issues for known selection bugs in 0.7.

Phases with standard patterns (skip research-phase):
- **Phase 2 (Highlight Rendering):** Span splitting is a well-understood operation. No research needed — write and test directly.
- **Phase 3 (Mouse Selection):** crossterm mouse event model is well-documented. Event types and fields are known.
- **Phase 4 (Keyboard + Copy):** arboard and crossterm key event handling are well-documented. Standard integration work.

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | arboard is the right choice; tui-textarea selection confirmed in 0.7 changelog; crossterm mouse events well-documented |
| Features | HIGH | User expectations well-understood; anti-features clearly scoped; MVP boundary is clean |
| Architecture | MEDIUM | Component boundaries are clear; WrapMap internals need ratatui source verification; tui-textarea yank bridge needs prototyping |
| Pitfalls | HIGH | Coordinate issues and span splitting are canonical hard problems with known mitigations; well-documented in Rust TUI community |

**Overall confidence:** MEDIUM-HIGH

### Gaps to Address

- **ratatui wrapping algorithm:** `Paragraph` wrap behavior with `Wrap { trim: false }` must be confirmed to use pure unicode-width with no word-break heuristics. Validate by reading ratatui 0.29 source during WrapMap implementation. If heuristics exist, WrapMap must replicate them exactly.
- **tui-textarea yank buffer access:** `copy()` stores text in an internal buffer. The API to read that buffer back (for bridging to arboard) needs verification. If unavailable, fall back to implementing custom selection overlay on composer.
- **arboard on headless Linux:** CI and SSH environments may lack clipboard access. Graceful fallback (log warning, no-op) must be tested, not assumed.

## Sources

### Primary (HIGH confidence)
- `tui-textarea` 0.7 crate documentation — selection API methods confirmed
- `arboard` 3.6 crate documentation — clipboard API, platform support, lifetime requirements
- `crossterm` 0.28 event model — mouse event types including MouseUp

### Secondary (MEDIUM confidence)
- ratatui 0.29 `Paragraph` widget — wrap behavior; source read needed to confirm exact algorithm
- `unicode-width` crate — used by ratatui internally; same crate must be used in WrapMap

### Tertiary (LOW confidence)
- tui-textarea GitHub issues — known selection bugs in 0.7; needs direct inspection during phase 5 planning

---
*Research completed: 2026-03-22*
*Ready for roadmap: yes*
