mod buttons;
mod diff;
mod help;
mod keymap;
mod modal;
mod mouse;
mod notify;
mod palette;
// PTY-passthrough embedded `claude` pane — the app's only session view. The
// module exposes a small terminal API (resize/blit/send/…); a few accessors are
// kept for tests and future wiring, hence the module-level allow.
#[allow(dead_code)]
mod pane;
mod render;
mod settings;
mod theme;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, KeyEventKind};
use kommand0_core::{AppState, Config, DEFAULT_PROFILE, RepoEntry, SessionStatus, Workspace};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{Event, KeyCode, KeyModifiers, MouseEvent},
};

/// Height of the session tab strip at the top of the right pane.
const TAB_BAR_HEIGHT: u16 = 1;

/// Maximum session tabs per workspace (keeps single-digit `1`–`9` shortcuts and
/// single-column tab labels).
pub(crate) const MAX_SESSION_TABS: usize = 9;

/// How often the periodic git-status refresh runs (off the render loop). Git
/// status is cheap but can stall on large/cold worktrees, so it never runs on
/// the 50ms tick; on-demand triggers (workspace create/close) keep it snappy.
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// How often the periodic PR/CI-status refresh runs (off the render loop). A `gh
/// pr list` per repo is a network call, so it runs far less often than the local
/// git status.
const PR_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Carries a status-refresh result to the event loop, sending on drop so the
/// loop always gets a message (and clears `status_inflight`) even if the worker
/// thread panics before finishing.
struct StatusRefreshGuard {
    tx: tokio::sync::mpsc::UnboundedSender<HashMap<String, kommand0_core::BranchStatus>>,
    result: Option<HashMap<String, kommand0_core::BranchStatus>>,
}

impl Drop for StatusRefreshGuard {
    fn drop(&mut self) {
        if let Some(map) = self.result.take() {
            let _ = self.tx.send(map);
        }
    }
}

/// Carries a PR-status refresh result (`ws_id` → [`kommand0_core::PrStatus`]) to
/// the event loop, sending on drop so a worker panic still clears
/// `pr_status_inflight`.
struct PrStatusRefreshGuard {
    tx: tokio::sync::mpsc::UnboundedSender<HashMap<String, kommand0_core::PrStatus>>,
    result: Option<HashMap<String, kommand0_core::PrStatus>>,
}

impl Drop for PrStatusRefreshGuard {
    fn drop(&mut self) {
        if let Some(map) = self.result.take() {
            let _ = self.tx.send(map);
        }
    }
}

/// Carries a cleanup result `(workspace_id, Ok(()) | Err(msg))` to the event
/// loop, sending on drop so a worker panic still clears `cleanup_inflight`.
struct CleanupGuard {
    tx: tokio::sync::mpsc::UnboundedSender<(String, Result<(), String>)>,
    payload: Option<(String, Result<(), String>)>,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if let Some(p) = self.payload.take() {
            let _ = self.tx.send(p);
        }
    }
}

/// Claude's message when a `--resume <id>` target no longer exists. Interactive
/// `claude` prints this and STAYS ALIVE (it does not exit), so resume failure
/// must be detected from the pane's output, not just from a fast non-zero exit.
const RESUME_MISS_MARKER: &str = "No conversation found with session ID";

/// Shown when a resume fails so the user knows reopening starts fresh.
const RESUME_FAIL_MSG: &str =
    "Couldn't resume the previous session (it may have been cleared): reopen to start fresh.";

/// A resumed tab still showing the resume-miss marker within this window of spawn
/// is a genuine miss (claude prints it at startup). After the window we stop
/// scanning so a live session's own output can't false-positive.
const RESUME_CHECK_WINDOW: Duration = Duration::from_secs(8);

/// The miss scan serializes each resumed pane's full grid under the parser
/// mutex its reader thread holds during replay, so running it every 50ms tick
/// stalled the UI thread during multi-resume storms (4 resumes = 80 full-grid
/// serializations/s). Healing a miss is not latency-sensitive; scan at most
/// this often within [`RESUME_CHECK_WINDOW`].
const RESUME_MISS_SCAN_EVERY: Duration = Duration::from_millis(500);

/// Decide the spawn args for a tab of `kind`, given the persisted entry to
/// reopen (`prior_id`, kind-prefixed) or `None` for a brand-new tab.
///
/// Returns `(args, minted)`: `minted` is the freshly-minted prefixed entry to
/// persist on a successful spawn (`None` when a reopen keeps its prior entry).
/// Resumable kinds resume with the BARE uuid (prefix stripped); anything but a
/// kommand0-minted uuid falls through to a fresh mint, so a hand-edited entry
/// can't smuggle flags into the argv. Non-resumable kinds spawn bare.
///
/// Known edge: if the very first launch is abandoned before any turn, the id is
/// still persisted but no conversation exists on disk. Reopening then runs
/// `--resume <id>`, which the tool rejects and exits non-zero, caught by
/// [`resume_failed`], which forgets the id so the next open starts fresh. So
/// the worst case self-heals in one reopen.
fn session_args(kind: TabKind, prior_id: Option<&str>) -> (Vec<String>, Option<String>) {
    if !kind.resumable() {
        // Fresh every open; a brand-new tab mints its persisted sentinel here.
        return (
            Vec::new(),
            prior_id
                .is_none()
                .then(|| AppState::new_prefixed_session_id(kind.id_prefix())),
        );
    }
    if let Some(id) = prior_id {
        let bare = id.strip_prefix(kind.id_prefix()).unwrap_or(id);
        if AppState::is_valid_session_uuid(bare) {
            return (vec!["--resume".to_string(), bare.to_string()], None);
        }
    }
    let id = AppState::new_prefixed_session_id(kind.id_prefix());
    let bare = id.strip_prefix(kind.id_prefix()).unwrap_or(&id).to_string();
    (vec!["--session-id".to_string(), bare], Some(id))
}

/// Resolve the binary for a tab kind: the kind's `KOMMAND0_*` env override
/// (used by tests and ad-hoc overrides; read by the caller, kept a parameter
/// here for testability) wins, then the config's override, then the default.
/// Shell falls back to `$SHELL` then `/bin/sh`; every other kind defaults to
/// its tool name.
fn pick_bin(kind: TabKind, env_bin: Option<String>, config_bin: Option<&str>) -> String {
    let picked = env_bin
        .filter(|s| !s.is_empty())
        .or_else(|| config_bin.filter(|s| !s.is_empty()).map(str::to_string));
    match kind {
        TabKind::Shell => picked
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/sh".to_string()),
        _ => picked.unwrap_or_else(|| kind.label().to_string()),
    }
}

/// Whether an exited embedded pane looks like a failed `--resume` (the Claude
/// session was purged): it was resumed, died within the window, and exited with
/// a non-zero code (a clean `/exit` is code 0 and must not trip this).
fn resume_failed(spawned: Instant, was_resume: bool, now: Instant, exit_code: Option<i32>) -> bool {
    const RESUME_FAIL_WINDOW: Duration = Duration::from_millis(2000);
    was_resume
        && now.saturating_duration_since(spawned) < RESUME_FAIL_WINDOW
        && !matches!(exit_code, Some(0) | None)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Focus {
    Tree,
    /// An embedded interactive `claude` pane (PTY passthrough) owns the keyboard.
    Embedded,
}

/// The rect the active session's pane occupies inside the right pane: the
/// border-excluded area, minus the session tab strip at the top. The single
/// source of truth for the pane geometry (spawn size, blit, mouse translation).
fn pane_content_rect(right_pane_area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let inner = right_pane_area.inner(ratatui::layout::Margin::new(1, 1));
    // A tiny pane can't reserve more rows for the tab strip than it has.
    let tab_h = TAB_BAR_HEIGHT.min(inner.height);
    ratatui::layout::Rect {
        x: inner.x,
        y: inner.y + tab_h,
        width: inner.width,
        height: inner.height - tab_h,
    }
}

/// Whether a workspace matches a (lowercased) tree-filter query by its name or
/// its branch.
fn ws_matches_query(w: &Workspace, q: &str) -> bool {
    w.name.to_lowercase().contains(q)
        || w
            .branch_name
            .as_deref()
            .is_some_and(|b| b.to_lowercase().contains(q))
}

/// Translate an absolute terminal mouse position into the active pane's
/// coordinate space. Returns `None` when the position is outside the pane's
/// content area (border, tab strip, or tree), so those clicks aren't forwarded.
fn translate_mouse(
    right_pane_area: ratatui::layout::Rect,
    col: u16,
    row: u16,
) -> Option<(u16, u16)> {
    let inner = pane_content_rect(right_pane_area);
    if col < inner.x || col >= inner.x + inner.width || row < inner.y || row >= inner.y + inner.height
    {
        return None;
    }
    Some((col - inner.x, row - inner.y))
}

#[derive(Clone)]
pub(crate) enum TreeNode {
    Repo {
        id: String,
        name: String,
    },
    Workspace {
        ws: Workspace,
        repo_name: String,
    },
    Hint {
        text: String,
    },
}

/// A visible row of the two-pane diff dialog's left file tree (flattened from the
/// per-file paths, respecting collapsed folders — see `rebuild_diff_rows`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffRow {
    Folder { path: String, name: String, depth: u16 },
    File { file_idx: usize, name: String, depth: u16 },
}

/// Which pane of the diff dialog has keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffFocus {
    Files,
    Diff,
}

/// What a session tab runs. Every kind persists an entry across restarts:
/// resumable kinds reopen with `--resume <uuid>` (the conversation continues),
/// the rest respawn fresh (a shell's process, unlike a conversation, can't be
/// resumed). All kind-specific knobs live in the accessors below, one match
/// arm each, so adding a kind is a compile-guided fill-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabKind {
    Claude,
    Shell,
}

impl TabKind {
    /// Lowercase tool name; doubles as the default binary for non-shell kinds.
    fn label(self) -> &'static str {
        match self {
            TabKind::Claude => "claude",
            TabKind::Shell => "shell",
        }
    }

    /// Prefix of this kind's persisted entries; claude's is empty (a bare
    /// uuid: legacy entries predate the prefixes). UUIDs contain no ':', so
    /// prefixed sentinels can never collide with claude ids.
    fn id_prefix(self) -> &'static str {
        match self {
            TabKind::Claude => "",
            TabKind::Shell => "shell:",
        }
    }

    /// Whether a reopened tab resumes its conversation (`--resume`) rather
    /// than spawning fresh; also keeps an exited tab's persisted id.
    fn resumable(self) -> bool {
        match self {
            TabKind::Claude => true,
            TabKind::Shell => false,
        }
    }

    /// The env var overriding this kind's binary (tests, ad-hoc overrides).
    fn bin_env(self) -> &'static str {
        match self {
            TabKind::Claude => "KOMMAND0_CLAUDE_BIN",
            TabKind::Shell => "KOMMAND0_SHELL",
        }
    }

    /// The configured binary override for this kind.
    fn config_bin(self, cfg: &Config) -> Option<&str> {
        match self {
            TabKind::Claude => cfg.claude_bin.as_deref(),
            TabKind::Shell => cfg.shell.as_deref(),
        }
    }

    /// The configured passthrough args appended to every spawn of this kind.
    fn config_args(self, cfg: &Config) -> &[String] {
        match self {
            TabKind::Claude => &cfg.claude_args,
            TabKind::Shell => &[],
        }
    }

    /// One-char tab-strip suffix marking the kind; claude is unmarked.
    fn marker(self) -> &'static str {
        match self {
            TabKind::Claude => "",
            TabKind::Shell => "$",
        }
    }

    /// Classify a persisted entry by its prefix; a bare uuid (or any unknown
    /// form) is claude's — legacy tolerance, and unknown junk fails fast into
    /// the existing forget/heal nets.
    fn from_session_id(id: &str) -> Self {
        [TabKind::Shell]
            .into_iter()
            .find(|k| id.starts_with(k.id_prefix()))
            .unwrap_or(TabKind::Claude)
    }
}

/// One session tab within a workspace: a live PTY pane plus the metadata
/// to persist/resume it and to detect a failed resume.
pub(crate) struct SessionTab {
    /// The persisted embedded-session entry: a bare uuid for a claude tab,
    /// else a kind-prefixed sentinel (`shell:<uuid>`, …), the stable key for
    /// activity tracking either way.
    pub(crate) id: String,
    pub(crate) pane: pane::Pane,
    was_resume: bool,
    spawned: Instant,
    pub(crate) kind: TabKind,
}

/// A workspace's open session tabs (tab order = creation order) and the active
/// tab. Kept in `App.embedded` only while non-empty.
#[derive(Default)]
pub(crate) struct WorkspaceSessions {
    pub(crate) tabs: Vec<SessionTab>,
    pub(crate) active: usize,
    /// The previously active tab's id — `Ctrl+A l` (tmux prefix-l) jumps back
    /// to it. By id, not index: indices shift on close, and a closed tab's id
    /// simply stops resolving, so the chord degrades to a no-op.
    last_active: Option<String>,
}

impl WorkspaceSessions {
    pub(crate) fn active_pane_mut(&mut self) -> Option<&mut pane::Pane> {
        self.tabs.get_mut(self.active).map(|t| &mut t.pane)
    }
    pub(crate) fn active_tab(&self) -> Option<&SessionTab> {
        self.tabs.get(self.active)
    }
    /// Every deliberate tab switch routes through here so the tab we left is
    /// remembered for `Ctrl+A l`. (The reap path assigns `active` directly:
    /// re-clamping onto the same logical tab is not a switch.)
    fn switch_to(&mut self, idx: usize) {
        if idx != self.active
            && let Some(prev) = self.tabs.get(self.active)
        {
            self.last_active = Some(prev.id.clone());
        }
        self.active = idx;
    }
    /// Switch to the previously active tab; silently a no-op when there is
    /// none (fresh pane) or it no longer exists (closed).
    fn select_last_active(&mut self) {
        if let Some(id) = self.last_active.clone()
            && let Some(idx) = self.tabs.iter().position(|t| t.id == id)
        {
            self.switch_to(idx); // records the outgoing tab: A <-> B toggles
        }
    }
    fn push(&mut self, tab: SessionTab) {
        self.tabs.push(tab);
        self.switch_to(self.tabs.len() - 1);
    }
    fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.switch_to((self.active + 1) % self.tabs.len());
        }
    }
    fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.switch_to((self.active + self.tabs.len() - 1) % self.tabs.len());
        }
    }
    fn select(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.switch_to(idx);
        }
    }
    /// Remove the tab at `idx` (order-preserving), re-clamping `active`. Returns
    /// whether the workspace is now empty (the caller removes the entry).
    fn remove_tab(&mut self, idx: usize) -> bool {
        if idx >= self.tabs.len() {
            return self.tabs.is_empty();
        }
        self.tabs.remove(idx);
        if idx < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.tabs.is_empty()
    }
}

/// Tree-pane width bounds and step (percent of the terminal). The live width is
/// always clamped to `[TREE_WIDTH_MIN, TREE_WIDTH_MAX]`.
const TREE_WIDTH_MIN: u16 = 15;
const TREE_WIDTH_MAX: u16 = 60;
const TREE_WIDTH_DEFAULT: u16 = 30;
const TREE_WIDTH_STEP: u16 = 5;

/// Resolve the startup tree width from the config knob: default when unset,
/// silently clamped into range (an out-of-range width isn't a typo worth a
/// warning, unlike a bad `theme`/`notify` value).
fn seed_tree_width(cfg: Option<u16>) -> u16 {
    cfg.unwrap_or(TREE_WIDTH_DEFAULT)
        .clamp(TREE_WIDTH_MIN, TREE_WIDTH_MAX)
}

/// The ancestor folder paths of a file path, outermost first: `"a/b/c.txt"` →
/// `["a", "a/b"]`. A top-level file (`"c.txt"`) yields none.
fn folder_prefixes(path: &str) -> Vec<String> {
    let comps: Vec<&str> = path.split('/').collect();
    let mut out = Vec::new();
    let mut prefix = String::new();
    for comp in &comps[..comps.len().saturating_sub(1)] {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(comp);
        out.push(prefix.clone());
    }
    out
}

/// Best-effort: open `url` with the OS browser opener, detached (stdio to null,
/// child dropped un-waited). Errors are ignored — a missing opener is a no-op,
/// not a crash. macOS uses `open`; everything else `xdg-open` (the app targets
/// macOS + Linux).
fn open_url(url: &str) {
    // Only hand https URLs to the OS opener — closes the scheme/flag edge (e.g. a
    // `-`-leading or `file://`/`javascript:` url) even though the caller's url is
    // gh-sourced.
    if !url.starts_with("https://") {
        return;
    }
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    if let Ok(mut child) = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        // Reap it off-thread so each `p` press doesn't leave a zombie; detached
        // and best-effort (we don't care about the opener's exit).
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

pub(crate) struct App {
    pub(crate) repos: Vec<RepoEntry>,
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) state: AppState,
    /// The state as we loaded it (the merge baseline). `save_state` merges our
    /// in-memory state over the current on-disk file relative to this, so a `kmd`
    /// command that wrote `state.json` while we were open isn't clobbered.
    pub(crate) state_baseline: AppState,
    pub(crate) expanded: HashSet<String>,
    pub(crate) tree_items: Vec<TreeNode>,
    pub(crate) selected_index: usize,

    pub(crate) focus: Focus,

    /// The effective non-default profile. Not just cosmetic: shown in the
    /// tree title AND exported to pane children as `KOMMAND0_PROFILE` (via
    /// `spawn_pane_cmd`) so a nested `kmd`/`kommand0` targets the same
    /// profile. `None` for the default profile — label hidden, and the var
    /// is REMOVED from children.
    pub(crate) profile_label: Option<String>,

    // UX state
    pub(crate) show_help: bool,
    pub(crate) help_scroll: u16,
    /// Review-diff dialog: the selected workspace's PR-style diff, as a
    /// collapsible file tree (left) + the selected file's diff (right).
    pub(crate) show_diff: bool,
    pub(crate) diff_title: String,
    /// What the left pane shows when there are no file rows — distinguishes a
    /// fallback workspace (no branch), a non-repo/uncomputable diff, and a
    /// genuinely-empty diff. Set in `open_diff`; empty when rows render.
    pub(crate) diff_note: String,
    /// The per-file diff sections (from `diff_files_vs_default_branch`).
    pub(crate) diff_files: Vec<kommand0_core::FileDiff>,
    /// The flattened, visible file tree — rebuilt on expand/collapse.
    pub(crate) diff_rows: Vec<DiffRow>,
    /// Expanded folder paths (default: all folders expanded when a diff opens).
    pub(crate) diff_expanded: HashSet<String>,
    /// Selected index into `diff_rows`.
    pub(crate) diff_selected: usize,
    /// Left file-list scroll (follows the selection).
    pub(crate) diff_list_scroll: u16,
    /// Right diff-pane scroll (reset when the selected FILE changes).
    pub(crate) diff_scroll: u16,
    pub(crate) diff_focus: DiffFocus,
    /// Rendered pane rects, set at render time for mouse hit-testing.
    pub(crate) diff_list_area: ratatui::layout::Rect,
    pub(crate) diff_body_area: ratatui::layout::Rect,
    /// True when a `g` was pressed and we're waiting for a second `g` (vim `gg`).
    pub(crate) pending_g: bool,
    /// Tree filter query (case-insensitive; empty = no filter). Matches a
    /// workspace by name or branch, or a repo by name (showing all its rows).
    pub(crate) filter_query: String,
    /// True while the `/` filter box is capturing keystrokes.
    pub(crate) filter_input: bool,
    /// Claude session ids (one per tab) whose pane produced output over the last
    /// couple of ticks — drives the activity spinner. Keyed by session id.
    /// Recomputed each tick from per-pane output deltas.
    pub(crate) waiting_response: HashSet<String>,
    pub(crate) spinner_tick: u8,
    /// Per-session last-observed `output_seq`, to detect new output between ticks.
    pane_seen: HashMap<String, u64>,
    /// Sessions that produced output on the previous tick but aren't armed yet — a
    /// one-tick debounce so a single keystroke echo or redraw doesn't flash active.
    pane_pending: HashSet<String>,
    /// Per-session instant until which the session counts as active, refreshed
    /// while its `output_seq` keeps advancing.
    pane_active_until: HashMap<String, Instant>,
    /// Per-session `output_seq` at the moment the user last *viewed* it (it was
    /// the focused workspace's active tab). Output past this point is "unseen".
    viewed_seq: HashMap<String, u64>,
    /// Per-session instant of the most recent new output (any delta, no
    /// debounce) — used to decide a session has gone quiet ("settled").
    last_output_at: HashMap<String, Instant>,
    /// Last time [`Self::reap_embedded`] ran the resume-miss screen scan
    /// (paced by `RESUME_MISS_SCAN_EVERY`; exit reaping stays per-tick).
    last_resume_scan: Instant,
    /// Sessions that produced unseen output and then went quiet — they "need
    /// you". A latched set: a session stays here until the user views it (or its
    /// pane is gone), so a mid-turn pause that resumes can't strobe it on/off.
    pub(crate) attention: HashSet<String>,
    pub(crate) pane_areas: mouse::PaneAreas,
    /// The tree list's scroll offset from the last render, so a mouse click on a
    /// visible row maps to the right `tree_items` index once the tree scrolls.
    pub(crate) tree_scroll_offset: usize,
    pub(crate) mouse_pos: Option<(u16, u16)>,
    /// True while the tree/content border is being dragged to resize the tree
    /// (ephemeral, not persisted — like `mouse_pos`).
    pub(crate) dragging_divider: bool,
    /// Drag-selection over the active embedded pane (highlight + OSC 52 copy);
    /// only ever refers to the VISIBLE pane — every route that could change
    /// which pane renders (keys, mouse Down, overlays, reap, background
    /// cleanup) clears it.
    pub(crate) pane_selection: Option<mouse::PaneSelection>,
    pub(crate) hit_regions: Vec<buttons::HitRegion>,
    pub(crate) pending_button_action: Option<buttons::HitAction>,
    pub(crate) modal: modal::ModalState,
    /// Active "go to workspace" command palette overlay, if open (`:`).
    pub(crate) palette: Option<palette::Palette>,
    pub(crate) expanded_icon_rows: HashSet<String>,
    pub(crate) last_pane_width: u16,
    /// Tree (left) pane width as a percent of the terminal. Seeded from the
    /// `tree_width_pct` config knob at startup; live `<`/`>` adjust it (ephemeral).
    // invariant: always in [TREE_WIDTH_MIN, TREE_WIDTH_MAX]
    pub(crate) tree_width_pct: u16,
    tick_counter: u8,

    // Embedded interactive `claude` sessions, as tabs per workspace, composited
    // into the right pane.
    pub(crate) embedded: HashMap<String, WorkspaceSessions>,
    /// Reader-thread → event-loop repaint signal (set in `main` before the loop).
    pub(crate) embedded_wake: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    /// Last-rendered right-pane rect, so a new embedded pane spawns at its final
    /// size (a resize-after-spawn makes claude drop its first screen, e.g. the
    /// trust prompt).
    pub(crate) right_pane_area: ratatui::layout::Rect,
    /// True when the embedded-pane prefix (Ctrl+A) was pressed and the next key
    /// is a kommand0 command rather than forwarded to claude.
    pub(crate) embedded_prefix: bool,
    /// Whether kommand0's own terminal window has focus (crossterm
    /// `FocusGained`/`FocusLost`, mode 1004). One dimension of the composite
    /// focus synthesized for embedded children in [`App::sync_focus_states`].
    /// Defaults to true — a terminal that never reports simply keeps it there.
    pub(crate) terminal_focused: bool,
    /// Last embedded-pane spawn failure as `(workspace_id, message)`, surfaced
    /// in that workspace's detail pane only.
    pub(crate) embed_error: Option<(String, String)>,

    /// Per-workspace git status (branch, ahead/behind, dirty), refreshed off the
    /// render loop. Keyed by workspace id; absent until the first refresh lands.
    pub(crate) branch_status: HashMap<String, kommand0_core::BranchStatus>,
    /// A status-refresh worker is running; gates re-spawning (cleared when its
    /// result arrives — and it always sends, even on a partial/empty result).
    status_inflight: bool,
    /// Worker → event-loop channel carrying a fresh status map (set in `run`).
    status_tx: Option<tokio::sync::mpsc::UnboundedSender<HashMap<String, kommand0_core::BranchStatus>>>,
    /// When the periodic status refresh last fired (time-based, so it doesn't
    /// depend on the wrapping tick counter).
    last_status_refresh: Option<Instant>,

    /// Per-workspace PR/CI status, refreshed off the render loop via one
    /// `gh pr list` per repo. Keyed by workspace id; absent until a PR exists on
    /// the workspace's branch (or until the first refresh lands).
    pub(crate) pr_status: HashMap<String, kommand0_core::PrStatus>,
    /// A PR-status refresh worker is running; gates re-spawning (always cleared
    /// when its result arrives, even on a partial/empty map).
    pr_status_inflight: bool,
    /// Worker → event-loop channel carrying a fresh `ws_id` → PrStatus map.
    pr_status_tx: Option<tokio::sync::mpsc::UnboundedSender<HashMap<String, kommand0_core::PrStatus>>>,
    /// When the periodic PR-status refresh last fired (time-based).
    last_pr_status_refresh: Option<Instant>,

    /// Workspaces with a cleanup in flight (gates re-triggering; shows progress).
    pub(crate) cleanup_inflight: HashSet<String>,
    /// Last cleanup *failure* per workspace (a success deletes the workspace).
    pub(crate) cleanup_result: HashMap<String, String>,
    /// Cleanup worker → event-loop channel carrying `(workspace_id, result)`.
    cleanup_tx: Option<tokio::sync::mpsc::UnboundedSender<(String, Result<(), String>)>>,

    /// User config (claude passthrough + tunables), loaded once at startup.
    pub(crate) config: Config,
    /// Where the settings page writes config fields back to. Resolved once at
    /// startup (`Config::effective_path()`); tests point it at a temp file.
    pub(crate) config_path: std::path::PathBuf,
    /// The settings page, when open (owns the screen like the other overlays).
    pub(crate) settings: Option<settings::SettingsState>,
    /// Set when a present `config.json` failed to parse, or a keybinding was
    /// unknown/invalid — surfaced in the tree border so it isn't silently ignored.
    pub(crate) config_warning: Option<String>,
    /// Rebindable tree-pane key map (defaults until `run` applies config).
    pub(crate) keymap: keymap::KeyMap,
    /// Color theme for the chrome (defaults until `run` applies config).
    pub(crate) theme: theme::Theme,
    /// How to alert when a backgrounded session needs you (defaults to `Off`
    /// until `run` applies config).
    pub(crate) notify_mode: notify::NotifyMode,
}

impl App {
    fn new(state: AppState) -> Self {
        let repos = state.repos.clone();
        let workspaces = state.workspaces.clone();
        let state_baseline = state.clone();

        let mut app = Self {
            repos,
            workspaces,
            state,
            state_baseline,
            expanded: HashSet::new(),
            tree_items: Vec::new(),
            selected_index: 0,
            focus: Focus::Tree,
            profile_label: None,
            show_help: false,
            help_scroll: 0,
            show_diff: false,
            diff_title: String::new(),
            diff_note: String::new(),
            diff_files: Vec::new(),
            diff_rows: Vec::new(),
            diff_expanded: HashSet::new(),
            diff_selected: 0,
            diff_list_scroll: 0,
            diff_scroll: 0,
            diff_focus: DiffFocus::Files,
            diff_list_area: ratatui::layout::Rect::default(),
            diff_body_area: ratatui::layout::Rect::default(),
            pending_g: false,
            filter_query: String::new(),
            filter_input: false,
            waiting_response: HashSet::new(),
            spinner_tick: 0,
            pane_seen: HashMap::new(),
            pane_pending: HashSet::new(),
            pane_active_until: HashMap::new(),
            viewed_seq: HashMap::new(),
            last_output_at: HashMap::new(),
            last_resume_scan: Instant::now(),
            attention: HashSet::new(),
            pane_areas: mouse::PaneAreas::default(),
            tree_scroll_offset: 0,
            mouse_pos: None,
            dragging_divider: false,
            pane_selection: None,
            hit_regions: Vec::new(),
            pending_button_action: None,
            modal: modal::ModalState::default(),
            palette: None,
            expanded_icon_rows: HashSet::new(),
            last_pane_width: 0,
            tree_width_pct: TREE_WIDTH_DEFAULT,
            tick_counter: 0,
            embedded: HashMap::new(),
            embedded_wake: None,
            right_pane_area: ratatui::layout::Rect::default(),
            embedded_prefix: false,
            terminal_focused: true,
            embed_error: None,
            branch_status: HashMap::new(),
            status_inflight: false,
            status_tx: None,
            last_status_refresh: None,
            pr_status: HashMap::new(),
            pr_status_inflight: false,
            pr_status_tx: None,
            last_pr_status_refresh: None,
            cleanup_inflight: HashSet::new(),
            cleanup_result: HashMap::new(),
            cleanup_tx: None,
            config: Config::default(),
            config_path: Config::effective_path(),
            settings: None,
            config_warning: None,
            keymap: keymap::KeyMap::default(),
            theme: theme::Theme::default(),
            notify_mode: notify::NotifyMode::default(),
        };
        app.rebuild_tree();
        if !app.tree_items.is_empty() {
            app.selected_index = 0;
        }
        app
    }

    fn rebuild_tree(&mut self) {
        self.tree_items.clear();
        let q = self.filter_query.to_lowercase();
        let filtering = !q.is_empty();
        for repo in &self.repos {
            // When a repo's name matches, show all its workspaces; otherwise only
            // those whose name/branch matches. No filter => all of them.
            let repo_matches = filtering && repo.name.to_lowercase().contains(&q);
            let repo_workspaces: Vec<&Workspace> = self
                .workspaces
                .iter()
                .filter(|w| w.repo_id == repo.id)
                .filter(|w| !filtering || repo_matches || ws_matches_query(w, &q))
                .collect();

            // A filtered-out repo (no matching workspaces, name doesn't match) is
            // omitted entirely so search narrows across collapsed repos too.
            if filtering && repo_workspaces.is_empty() {
                continue;
            }

            self.tree_items.push(TreeNode::Repo {
                id: repo.id.clone(),
                name: repo.name.clone(),
            });

            // Matched repos are force-expanded so the matches are visible.
            if !filtering && !self.expanded.contains(&repo.id) {
                continue;
            }
            if filtering {
                for ws in &repo_workspaces {
                    self.tree_items.push(TreeNode::Workspace {
                        ws: (*ws).clone(),
                        repo_name: repo.name.clone(),
                    });
                }
            } else if repo_workspaces.is_empty() {
                self.tree_items.push(TreeNode::Hint {
                    text: "(no workspaces — press w to add)".into(),
                });
            } else {
                let all_archived = repo_workspaces.iter().all(|w| !w.active);
                for ws in &repo_workspaces {
                    self.tree_items.push(TreeNode::Workspace {
                        ws: (*ws).clone(),
                        repo_name: repo.name.clone(),
                    });
                }
                if all_archived {
                    self.tree_items.push(TreeNode::Hint {
                        text: "(all archived)".into(),
                    });
                }
            }
        }
    }

    /// Apply the current filter: rebuild and land selection on the first match
    /// (the tree shrinks as the query narrows, so selection must be re-seated).
    fn apply_filter(&mut self) {
        self.rebuild_tree();
        self.selected_index = self
            .tree_items
            .iter()
            .position(|n| matches!(n, TreeNode::Workspace { .. }))
            .or_else(|| {
                self.tree_items
                    .iter()
                    .position(|n| !matches!(n, TreeNode::Hint { .. }))
            })
            .unwrap_or(0);
        self.update_active_session();
    }

    /// Re-seat `selected_index` after a rebuild that may have shrunk the tree:
    /// clamp into range and off any hint row.
    fn clamp_selection(&mut self) {
        if self.tree_items.is_empty() {
            self.selected_index = 0;
            return;
        }
        if self.selected_index >= self.tree_items.len() {
            self.selected_index = self.tree_items.len() - 1;
        }
        if self.is_hint(self.selected_index) {
            self.move_up();
        }
        self.update_active_session();
    }

    pub(crate) fn is_hint(&self, index: usize) -> bool {
        matches!(self.tree_items.get(index), Some(TreeNode::Hint { .. }))
    }

    pub(crate) fn move_up(&mut self) {
        if self.tree_items.is_empty() {
            return;
        }
        let len = self.tree_items.len();
        let mut next = if self.selected_index == 0 {
            len - 1
        } else {
            self.selected_index - 1
        };
        let mut attempts = 0;
        while self.is_hint(next) && attempts < len {
            next = if next == 0 { len - 1 } else { next - 1 };
            attempts += 1;
        }
        self.selected_index = next;
        self.update_active_session();
    }

    pub(crate) fn move_down(&mut self) {
        if self.tree_items.is_empty() {
            return;
        }
        let len = self.tree_items.len();
        let mut next = if self.selected_index >= len - 1 {
            0
        } else {
            self.selected_index + 1
        };
        let mut attempts = 0;
        while self.is_hint(next) && attempts < len {
            next = if next >= len - 1 { 0 } else { next + 1 };
            attempts += 1;
        }
        self.selected_index = next;
        self.update_active_session();
    }

    /// The single clamp home for `tree_width_pct` — keeps the `[15,60]`
    /// invariant for every write-path (seed, keys, drag).
    pub(crate) fn set_tree_width_pct(&mut self, pct: u16) {
        self.tree_width_pct = pct.clamp(TREE_WIDTH_MIN, TREE_WIDTH_MAX);
    }

    /// Persist one settings-page field: validate, write `config.json`, mirror
    /// into the in-memory config, and re-apply the live knobs. `Err` is the
    /// user-facing message for the settings error line; on failure neither the
    /// file nor the in-memory config has changed.
    fn commit_setting(&mut self, field: settings::Field, raw: &str) -> Result<(), String> {
        let value = field.parse(raw)?;
        Config::update_file(&self.config_path, field.key(), value.clone())
            .map_err(|e| format!("{e:#}"))?;
        field.store(&mut self.config, value.as_ref());
        match field {
            settings::Field::Theme => {
                self.theme =
                    theme::Theme::build(self.config.theme.as_deref(), &self.config.theme_colors).0;
            }
            settings::Field::TreeWidthPct => {
                self.set_tree_width_pct(seed_tree_width(self.config.tree_width_pct));
            }
            settings::Field::Notify => {
                self.notify_mode = notify::NotifyMode::parse(self.config.notify.as_deref()).0;
            }
            // claude_args/claude_bin/shell apply at the next spawn;
            // status_refresh_secs is read from config every tick.
            _ => {}
        }
        self.refresh_config_warning();
        Ok(())
    }

    /// Recompute the tree-border config warning from the in-memory config, so a
    /// just-fixed issue stops showing (and remaining ones keep showing). The
    /// startup-only notices (file parse error, corrupt state) are dropped: a
    /// successful commit proves the file parses, and both are in the log.
    fn refresh_config_warning(&mut self) {
        let (_, key_warnings) = keymap::KeyMap::build(&self.config.keybindings);
        let (_, theme_warnings) =
            theme::Theme::build(self.config.theme.as_deref(), &self.config.theme_colors);
        let (_, notify_warning) = notify::NotifyMode::parse(self.config.notify.as_deref());
        let mut warnings: Vec<String> = key_warnings;
        warnings.extend(theme_warnings);
        warnings.extend(notify_warning);
        self.config_warning = match warnings.len() {
            0 => None,
            1 => Some(warnings.remove(0)),
            n => Some(format!("{n} config issues — see kommand0.log")),
        };
    }

    pub(crate) fn widen_tree(&mut self) {
        self.set_tree_width_pct(self.tree_width_pct.saturating_add(TREE_WIDTH_STEP));
    }

    pub(crate) fn shrink_tree(&mut self) {
        self.set_tree_width_pct(self.tree_width_pct.saturating_sub(TREE_WIDTH_STEP));
    }

    pub(crate) fn toggle_expand(&mut self) {
        // While filtering, repos are force-expanded; don't mutate the user's
        // saved expand state (it's restored when the filter clears).
        if !self.filter_query.is_empty() {
            return;
        }
        if let Some(TreeNode::Repo { id, .. }) = self.tree_items.get(self.selected_index) {
            let id = id.clone();
            if self.expanded.contains(&id) {
                self.expanded.remove(&id);
            } else {
                self.expanded.insert(id.clone());
            }
            let repo_id = id;
            self.rebuild_tree();
            for (i, node) in self.tree_items.iter().enumerate() {
                if let TreeNode::Repo { id, .. } = node
                    && *id == repo_id
                {
                    self.selected_index = i;
                    break;
                }
            }
            if !self.tree_items.is_empty() {
                self.selected_index = self.selected_index.min(self.tree_items.len() - 1);
            }
        }
    }

    /// Vim `h`: collapse the selected repo, or jump from a workspace to its parent repo.
    pub(crate) fn tree_collapse_or_parent(&mut self) {
        match self.tree_items.get(self.selected_index) {
            Some(TreeNode::Repo { id, .. }) => {
                if self.expanded.contains(id) {
                    self.toggle_expand();
                }
            }
            Some(TreeNode::Workspace { ws, .. }) => {
                let repo_id = ws.repo_id.clone();
                if let Some(i) = self
                    .tree_items
                    .iter()
                    .position(|n| matches!(n, TreeNode::Repo { id, .. } if *id == repo_id))
                {
                    self.selected_index = i;
                    self.update_active_session();
                }
            }
            _ => {}
        }
    }

    /// Vim `l`: expand the selected repo, or step into its first child when already expanded.
    pub(crate) fn tree_expand_or_enter(&mut self) {
        if let Some(TreeNode::Repo { id, .. }) = self.tree_items.get(self.selected_index) {
            // A filter force-expands repos, so step into the children directly.
            if !self.filter_query.is_empty() || self.expanded.contains(id) {
                self.move_down();
            } else {
                self.toggle_expand();
            }
        }
    }

    /// Vim `gg`: select the first non-hint tree item.
    pub(crate) fn tree_select_first(&mut self) {
        if let Some(i) = (0..self.tree_items.len()).find(|&i| !self.is_hint(i)) {
            self.selected_index = i;
            self.update_active_session();
        }
    }

    /// Vim `G`: select the last non-hint tree item.
    pub(crate) fn tree_select_last(&mut self) {
        if let Some(i) = (0..self.tree_items.len()).rev().find(|&i| !self.is_hint(i)) {
            self.selected_index = i;
            self.update_active_session();
        }
    }

    /// Move the selection to the nearest repo header before/after the current
    /// row, skipping workspace rows (vim `{`/`}` style: clamps at the
    /// outermost repo, no wrap). Operates on the rendered `tree_items`, so it
    /// respects an active filter.
    pub(crate) fn jump_repo(&mut self, forward: bool) {
        let target = if forward {
            (self.selected_index + 1..self.tree_items.len())
                .find(|&i| matches!(self.tree_items[i], TreeNode::Repo { .. }))
        } else {
            (0..self.selected_index)
                .rev()
                .find(|&i| matches!(self.tree_items[i], TreeNode::Repo { .. }))
        };
        if let Some(i) = target {
            self.selected_index = i;
            self.update_active_session();
        }
    }

    /// Merge this state over disk and, on success, advance the merge baseline
    /// to the in-memory state just written (git's fetch-updates-tracking-ref
    /// model). The baseline must track the last state of OURS known to be on
    /// disk: left at the startup snapshot, a key first persisted mid-run reads
    /// as a concurrent add once we delete it (mine=absent, baseline=absent,
    /// disk=present) and the closed tab resurrects on the next reopen.
    /// Deliberately NOT the merged result: a concurrent `kmd` add lives on disk
    /// but not in `self.state`, and baselining it would flip it into "our
    /// deletion" on the next save.
    fn persist_state(&mut self) -> anyhow::Result<()> {
        // Merge over the current on-disk state (a `kmd` command may have written
        // it while we were open) rather than blindly overwriting it.
        self.state.merge_save(&self.state_baseline)?;
        self.state_baseline = self.state.clone();
        Ok(())
    }

    /// Persist state, logging (not silently dropping) any failure: a save error
    /// is otherwise invisible while the TUI owns the terminal.
    pub(crate) fn save_state(&mut self) {
        if let Err(e) = self.persist_state() {
            tracing::warn!("failed to persist state: {e}");
        }
    }

    /// Flip any persisted `Running` session to `Stopped`. A `Running` left in
    /// `state.json` is stale — a crash/SIGKILL skipped the clean-quit
    /// normalization — and no stream session is ever resurrected, so it would
    /// otherwise show a phantom "running" tree icon. Called once at startup;
    /// returns the number normalized (the caller saves if > 0).
    pub(crate) fn normalize_stale_running(&mut self) -> usize {
        let stale: Vec<String> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Running)
            .map(|s| s.id.clone())
            .collect();
        let n = stale.len();
        for sid in stale {
            let _ = self.state.update_session_status(&sid, SessionStatus::Stopped);
        }
        n
    }

    /// Keep keyboard focus on the tree when the selection is not a workspace row.
    pub(crate) fn update_active_session(&mut self) {
        if !matches!(
            self.tree_items.get(self.selected_index),
            Some(TreeNode::Workspace { .. })
        ) {
            self.focus = Focus::Tree;
        }
    }

    /// Enter (spawning if needed) the embedded interactive `claude` pane for the
    /// selected workspace. Experimental PTY-passthrough toggle (Phase 2).
    /// Open the selected workspace's embedded sessions: if it has none live yet,
    /// restore every persisted entry as a tab of its kind (resumable kinds
    /// resume their session, the rest respawn fresh; or start a first claude
    /// session when none are stored), then focus the first tab.
    fn toggle_embedded(&mut self) {
        let Some(ws) = self.selected_workspace() else {
            return;
        };
        let ws_id = ws.id.clone();
        let ws_name = ws.name.clone();
        let ws_dir = ws.working_dir.clone();
        if !self.embedded.contains_key(&ws_id) {
            // Cleared up front; spawn_tab re-sets it on any failure, so a
            // partial-reopen failure's message survives (a later clear would
            // swallow it).
            self.embed_error = None;
            let persisted: Vec<String> = self.state.embedded_session_ids(&ws_id).to_vec();
            if persisted.is_empty() {
                self.spawn_tab(TabKind::Claude, &ws_id, &ws_dir, &ws_name, None);
            } else {
                for id in persisted.iter().take(MAX_SESSION_TABS) {
                    let kind = TabKind::from_session_id(id);
                    self.spawn_tab(kind, &ws_id, &ws_dir, &ws_name, Some(id));
                }
            }
            // If every spawn failed, embed_error is set — stay on the tree.
            let Some(sessions) = self.embedded.get_mut(&ws_id) else {
                return;
            };
            sessions.switch_to(0); // focus the first tab on open
        } else {
            self.embed_error = None;
        }
        self.focus = Focus::Embedded;
        self.embedded_prefix = false;
    }

    /// Spawn a pane of `kind` (no persistence, no tab append). `prior_id`
    /// reopens that persisted entry (resumable kinds resume it, the rest
    /// respawn fresh); `None` mints a fresh one. Returns the pane plus its
    /// `(session_id, was_resume)`, or the spawn error.
    fn spawn_pane(
        &self,
        kind: TabKind,
        ws_dir: &str,
        prior_id: Option<&str>,
    ) -> anyhow::Result<(pane::Pane, String, bool)> {
        let bin = pick_bin(
            kind,
            std::env::var(kind.bin_env()).ok(),
            kind.config_bin(&self.config),
        );
        let (mut args, minted) = session_args(kind, prior_id);
        // Append the user's configured passthrough args (e.g. `--model sonnet`).
        args.extend(kind.config_args(&self.config).iter().cloned());
        // A resume = a resumable kind reopening a usable prior id (a junk
        // prior id falls through to a fresh mint in session_args and must
        // stay out of the resume-failure nets).
        let was_resume = kind.resumable() && prior_id.is_some() && minted.is_none();
        // The tab keeps its persisted entry as its id; only a brand-new tab
        // takes the minted one.
        let session_id = prior_id
            .map(String::from)
            .unwrap_or_else(|| minted.unwrap());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let pane = self.spawn_pane_cmd(ws_dir, &bin, &arg_refs)?;
        Ok((pane, session_id, was_resume))
    }

    /// Spawn a pane running `bin args` in `ws_dir`, sized to the content area and
    /// wired to the repaint waker. The generic core of both Claude and shell tabs.
    fn spawn_pane_cmd(&self, ws_dir: &str, bin: &str, args: &[&str]) -> anyhow::Result<pane::Pane> {
        let wake: Option<Box<dyn Fn() + Send>> = self.embedded_wake.clone().map(|tx| {
            Box::new(move || {
                let _ = tx.send(());
            }) as Box<dyn Fn() + Send>
        });
        // Spawn at the pane's final inner size so the first render needs no resize
        // (a TUI child drops its first screen on a SIGWINCH mid-render).
        let inner = pane_content_rect(self.right_pane_area);
        let rows = if inner.height > 0 { inner.height } else { 24 };
        let cols = if inner.width > 0 { inner.width } else { 80 };
        pane::Pane::spawn_with_wake(
            bin,
            args,
            std::path::Path::new(ws_dir),
            rows,
            cols,
            wake,
            self.profile_label.as_deref(),
        )
    }

    /// Spawn one tab of `kind` for a workspace and append it. `prior_id`
    /// reopens that persisted entry (resumable kinds resume it, the rest
    /// respawn fresh); `None` mints + persists a fresh one. Returns whether
    /// the pane started (on failure `embed_error` is set, and a reopened
    /// entry is forgotten so the persisted Vec stays aligned).
    fn spawn_tab(
        &mut self,
        kind: TabKind,
        ws_id: &str,
        ws_dir: &str,
        ws_name: &str,
        prior_id: Option<&str>,
    ) -> bool {
        match self.spawn_pane(kind, ws_dir, prior_id) {
            Ok((pane, session_id, was_resume)) => {
                if prior_id.is_none() {
                    self.state.add_embedded_session(ws_id, &session_id);
                    // Merge over disk (like save_state) so this doesn't clobber a
                    // concurrent `kmd` write; keep the Result to surface a failure.
                    if self.persist_state().is_err() {
                        self.embed_error = Some((
                            ws_id.to_string(),
                            "Couldn't persist this tab: it may not reopen after \
                             restarting kommand0."
                                .to_string(),
                        ));
                    }
                }
                self.embedded
                    .entry(ws_id.to_string())
                    .or_default()
                    .push(SessionTab {
                        id: session_id,
                        pane,
                        was_resume,
                        spawned: Instant::now(),
                        kind,
                    });
                true
            }
            Err(e) => {
                // A reopen that couldn't even spawn: forget the entry so the
                // persisted Vec stays aligned with the runtime tabs.
                if let Some(id) = prior_id {
                    self.state.remove_embedded_session(ws_id, id);
                    self.save_state();
                }
                self.embed_error = Some((
                    ws_id.to_string(),
                    format!("Failed to start {} in {ws_name}: {e}", kind.label()),
                ));
                false
            }
        }
    }

    /// Auto-heal a resume that found no session: forget the gone id and replace
    /// its tab in place with a fresh session of the same kind (same slot, so
    /// the active tab and numbering are preserved). Returns `false` if the
    /// fresh spawn itself failed (the caller then drops the tab). `now` stamps
    /// the new tab.
    fn heal_resume(
        &mut self,
        kind: TabKind,
        ws_id: &str,
        gone_id: &str,
        ws_dir: &str,
        now: Instant,
    ) -> bool {
        // Surface the miss loudly (log here, banner below): healing silently
        // would convert a recoverable store mismatch (e.g. a worktree moved
        // out from under claude's cwd-keyed store) into a forgotten id.
        tracing::warn!(
            "resume miss: session {gone_id} not found in {}'s store for {ws_dir}; starting fresh",
            kind.label()
        );
        match self.spawn_pane(kind, ws_dir, None) {
            Ok((pane, new_id, was_resume)) => {
                // In place (same index) so the persisted order matches the
                // in-slot runtime replace below; the helper also moves the
                // user's tab title onto the fresh id.
                self.state.replace_embedded_session(ws_id, gone_id, &new_id);
                self.save_state();
                if let Some(sessions) = self.embedded.get_mut(ws_id)
                    && let Some(slot) = sessions.tabs.iter().position(|t| t.id == gone_id)
                {
                    sessions.tabs[slot] = SessionTab {
                        id: new_id,
                        pane,
                        was_resume,
                        spawned: now,
                        kind,
                    };
                }
                let mut msg = format!(
                    "session {} not found in {}'s store for this directory: started fresh",
                    gone_id.get(..8).unwrap_or(gone_id),
                    kind.label()
                );
                if kind == TabKind::Claude {
                    msg.push_str(
                        " (the old transcript keeps its uuid filename under ~/.claude/projects/)",
                    );
                }
                self.embed_error = Some((ws_id.to_string(), msg));
                true
            }
            Err(_) => {
                // The fresh spawn failed too: forget the gone id (the caller
                // drops the tab) so it isn't re-resumed on the next reopen.
                self.state.remove_embedded_session(ws_id, gone_id);
                self.save_state();
                false
            }
        }
    }

    /// The active session's pane for the selected workspace, if any.
    fn active_pane_mut(&mut self) -> Option<&mut pane::Pane> {
        let ws_id = self.selected_workspace()?.id.clone();
        self.embedded.get_mut(&ws_id)?.active_pane_mut()
    }

    /// The visible pane's `output_seq`: the selected workspace's active tab
    /// (immutable mirror of `active_pane_mut`'s lookup). The event loop's wake
    /// arm compares it against the last drawn value to tell visible output
    /// from background-only output, which needs no repaint.
    fn visible_pane_seq(&self) -> Option<u64> {
        let ws_id = &self.selected_workspace()?.id;
        Some(self.embedded.get(ws_id)?.active_tab()?.pane.output_seq())
    }

    /// Focus the embedded pane from a mouse gesture. Clearing the Ctrl+A
    /// prefix is part of the invariant — a half-typed prefix carried across a
    /// click would mis-fire on the next keystroke. Both mouse routes
    /// (click-to-focus and drag-selection) go through here.
    pub(crate) fn focus_embedded_by_click(&mut self) {
        self.focus = Focus::Embedded;
        self.embedded_prefix = false;
    }

    /// Synthesize terminal focus for embedded children. A child is "focused"
    /// iff kommand0's own terminal has focus AND the embedded pane owns input
    /// AND it is the active tab of the selected workspace. Recompute-and-diff
    /// over every live tab, edge-triggered per pane (`Pane::sync_focus`), so
    /// calling this once per frame covers every transition — terminal
    /// focus, Tree<->Embedded switches, tab switches, workspace moves — with
    /// no per-site wiring. Overlays (help/palette/modal/diff) deliberately do
    /// NOT count as unfocus; wiring overlay state in here is the upgrade path
    /// if that ever matters.
    fn sync_focus_states(&mut self) {
        let focused_ws = (self.terminal_focused && self.focus == Focus::Embedded)
            .then(|| self.selected_workspace().map(|w| w.id.clone()))
            .flatten();
        for (ws_id, sessions) in self.embedded.iter_mut() {
            let active = sessions.active;
            for (idx, tab) in sessions.tabs.iter_mut().enumerate() {
                let focused = focused_ws.as_deref() == Some(ws_id.as_str()) && idx == active;
                tab.pane.sync_focus(focused);
            }
        }
    }

    /// How many of a workspace's session tabs are currently active (a Claude tab
    /// producing output, or a shell tab running a foreground command). Zero means
    /// idle; the row shows the count alongside the spinner when two or more.
    pub(crate) fn ws_active_tab_count(&self, ws_id: &str) -> usize {
        self.embedded
            .get(ws_id)
            .map(|s| {
                s.tabs
                    .iter()
                    .filter(|t| self.waiting_response.contains(&t.id))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Move the tree selection to a workspace row (if present).
    fn select_workspace_row(&mut self, ws_id: &str) {
        if let Some(i) = self
            .tree_items
            .iter()
            .position(|n| matches!(n, TreeNode::Workspace { ws, .. } if ws.id == ws_id))
        {
            self.selected_index = i;
        }
    }

    /// Select a workspace by id (if present in the tree) and open its sessions.
    fn embed_workspace_by_id(&mut self, ws_id: &str) {
        self.select_workspace_row(ws_id);
        self.toggle_embedded();
    }

    /// Build the palette entries: a jump-and-open for every workspace, then the
    /// actions you can run on each (open PR / clean up / archive·activate / new
    /// session), then a jump for each open session tab. Each entry's match text
    /// folds in a verb + the workspace name + branch + repo so any of them
    /// narrows (e.g. "pr foo", "clean", "tab 2").
    fn palette_candidates(&self) -> Vec<palette::Candidate> {
        use palette::{Candidate, PaletteAction};
        let repo_name = |repo_id: &str| {
            self.repos.iter().find(|r| r.id == repo_id).map(|r| r.name.clone()).unwrap_or_default()
        };
        let mut out: Vec<Candidate> = Vec::new();

        // 1) Jump-and-open per workspace — the primary entry, kept first so an
        //    empty query still reads like the original "go to workspace" list.
        for w in &self.workspaces {
            let repo = repo_name(&w.repo_id);
            let match_text = match &w.branch_name {
                Some(b) => format!("{} {} {}", w.name, b, repo),
                None => format!("{} {}", w.name, repo),
            };
            out.push(Candidate {
                label: w.name.clone(),
                detail: repo,
                match_text,
                action: PaletteAction::OpenWorkspace { ws_id: w.id.clone() },
            });
        }

        // 2) Actions, each tagged with a verb so typing it narrows to them.
        for w in &self.workspaces {
            let repo = repo_name(&w.repo_id);
            let mk = |verb: &str, label: String, action: PaletteAction| Candidate {
                label,
                detail: repo.clone(),
                match_text: format!("{verb} {} {}", w.name, repo),
                action,
            };
            out.push(mk(
                "clean up cleanup",
                format!("Clean up — {}", w.name),
                PaletteAction::Cleanup { ws_id: w.id.clone() },
            ));
            let (verb, label) = if w.active {
                ("archive", format!("Archive — {}", w.name))
            } else {
                ("activate archive", format!("Activate — {}", w.name))
            };
            out.push(mk(verb, label, PaletteAction::ArchiveToggle { ws_id: w.id.clone() }));
            out.push(mk(
                "new session",
                format!("New session — {}", w.name),
                PaletteAction::NewSession { ws_id: w.id.clone() },
            ));
        }

        // 3) Jump to a specific session tab of each currently-open workspace.
        for w in &self.workspaces {
            let Some(sessions) = self.embedded.get(&w.id) else {
                continue;
            };
            let repo = repo_name(&w.repo_id);
            for i in 0..sessions.tabs.len() {
                let n = i + 1;
                out.push(Candidate {
                    label: format!("Session {n} — {}", w.name),
                    detail: repo.clone(),
                    match_text: format!("session tab {n} {} {}", w.name, repo),
                    action: PaletteAction::JumpTab { ws_id: w.id.clone(), index: i },
                });
            }
        }

        out
    }

    /// Run a chosen palette entry. Workspace-targeting actions first reveal +
    /// select the workspace so their feedback (PR progress, the cleanup
    /// confirmation, the new tab) lands on the right row even if it was hidden.
    fn dispatch_palette_action(&mut self, action: palette::PaletteAction) {
        use palette::PaletteAction::*;
        match action {
            OpenWorkspace { ws_id } => self.jump_to_workspace(&ws_id),
            Cleanup { ws_id } => {
                if self.reveal_workspace(&ws_id) {
                    self.cleanup_workspace_prompt(&ws_id);
                }
            }
            ArchiveToggle { ws_id } => self.archive_toggle(&ws_id),
            NewSession { ws_id } => {
                if self.reveal_workspace(&ws_id) {
                    self.new_tab(TabKind::Claude, &ws_id);
                }
            }
            JumpTab { ws_id, index } => {
                if self.reveal_workspace(&ws_id) {
                    self.select_session_tab(&ws_id, index);
                }
            }
        }
    }

    /// Jump to a workspace from the palette: make it visible (expanding its repo
    /// and clearing any active filter so its row is in the rebuilt tree), then
    /// open its embedded session. Works for any workspace, even one currently
    /// hidden under a collapsed repo.
    /// Make a workspace visible + selected: expand its repo, clear any active
    /// filter, rebuild the tree, and select its row. Returns false for an
    /// unknown id. Shared by the jump and by palette actions that target a
    /// workspace which may be hidden under a collapsed repo or a filter.
    fn reveal_workspace(&mut self, ws_id: &str) -> bool {
        let Some(repo_id) =
            self.workspaces.iter().find(|w| w.id == ws_id).map(|w| w.repo_id.clone())
        else {
            return false; // unknown workspace
        };
        self.expanded.insert(repo_id);
        self.filter_query.clear();
        self.filter_input = false;
        self.rebuild_tree();
        self.select_workspace_row(ws_id);
        true
    }

    fn jump_to_workspace(&mut self, ws_id: &str) {
        if self.reveal_workspace(ws_id) {
            self.toggle_embedded();
        }
    }

    /// Reveal + open the next (`forward`) or previous workspace flagged "needs
    /// you", scanning from the current selection and wrapping. No-op when nothing
    /// is waiting. Opening the session clears its attention, so repeated presses
    /// cycle through every waiting workspace.
    fn jump_to_waiting(&mut self, forward: bool) {
        let n = self.workspaces.len();
        if n == 0 {
            return;
        }
        let cur = self
            .selected_workspace()
            .and_then(|sel| self.workspaces.iter().position(|w| w.id == sel.id));
        let target = (1..=n).find_map(|step| {
            let idx = match cur {
                Some(c) if forward => (c + step) % n,
                Some(c) => (c + n - step) % n, // backward, wrapping
                None => step - 1,              // no selection: scan from the top
            };
            let w = &self.workspaces[idx];
            self.ws_needs_attention(&w.id).then(|| w.id.clone())
        });
        if let Some(id) = target {
            self.jump_to_workspace(&id);
        }
    }

    /// Archive an active workspace, or re-activate an archived one (the `A`
    /// action and the palette share this). Keeps the row selected across the
    /// rebuild.
    fn archive_toggle(&mut self, ws_id: &str) {
        let Some(ws) = self.workspaces.iter().find(|w| w.id == ws_id).cloned() else {
            return;
        };
        let res = if ws.active {
            self.state.archive_workspace(&ws.id)
        } else {
            self.state.activate_workspace(&ws.id)
        };
        if res.is_ok() {
            self.workspaces = self.state.workspaces.clone();
            self.rebuild_tree();
            self.select_workspace_row(&ws.id);
            self.clamp_selection();
        }
    }

    /// Select session tab `index` of a workspace and focus the embedded pane.
    fn select_session_tab(&mut self, ws_id: &str, index: usize) {
        self.select_workspace_row(ws_id);
        if let Some(sessions) = self.embedded.get_mut(ws_id) {
            sessions.select(index);
            self.focus = Focus::Embedded;
            self.embedded_prefix = false;
        }
    }

    /// The selected workspace's session set, if it has live tabs.
    fn selected_sessions_mut(&mut self) -> Option<&mut WorkspaceSessions> {
        let ws_id = self.selected_workspace()?.id.clone();
        self.embedded.get_mut(&ws_id)
    }

    /// A horizontal-wheel tick (or Shift+vertical, for terminals that don't
    /// translate it) anywhere in the right pane switches the visible
    /// workspace's session tab. Whole right-pane rect on purpose (tab strip +
    /// border included; wheel-over-tab-strip is where the gesture is most
    /// expected). Returns whether the event was consumed.
    fn hscroll_switch_tab(&mut self, mouse: MouseEvent) -> bool {
        use ratatui::crossterm::event::MouseEventKind;
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let prev = match mouse.kind {
            MouseEventKind::ScrollLeft => true,
            MouseEventKind::ScrollRight => false,
            MouseEventKind::ScrollUp if shift => true,
            MouseEventKind::ScrollDown if shift => false,
            _ => return false,
        };
        if !mouse::contains(self.right_pane_area, mouse.column, mouse.row) {
            return false;
        }
        // Consumed from here even with no sessions: reachable only via hover
        // (Embedded focus implies live tabs), where today's behavior is
        // forward-into-nothing, so consume-and-noop is observationally
        // identical and keeps the gesture single-sited.
        if let Some(s) = self.selected_sessions_mut() {
            if prev { s.prev() } else { s.next() }
        }
        // A tab switch invalidates a lingering selection highlight and a
        // half-typed Ctrl+A prefix, matching the sibling switch routes: the
        // key route clears the selection in handle_key, the click route
        // clears the prefix in select_session_tab; the wheel route gets
        // neither, so both happen here.
        self.pane_selection = None;
        self.embedded_prefix = false;
        true
    }

    /// Close the active session tab of the selected workspace: drop its pane,
    /// forget its persisted id, and re-focus the previous tab (or the tree when
    /// the last tab is gone).
    fn close_active_session(&mut self) {
        let Some(ws_id) = self.selected_workspace().map(|w| w.id.clone()) else {
            return;
        };
        let (tab_id, now_empty) = {
            let Some(sessions) = self.embedded.get_mut(&ws_id) else {
                return;
            };
            let active = sessions.active;
            let Some(tab_id) = sessions.tabs.get(active).map(|t| t.id.clone()) else {
                return;
            };
            (tab_id, sessions.remove_tab(active))
        };
        // Closing a tab forgets its persisted entry, whatever its kind: a Claude
        // id won't resume next time, a shell sentinel won't respawn.
        self.state.remove_embedded_session(&ws_id, &tab_id);
        self.save_state();
        if now_empty {
            self.embedded.remove(&ws_id);
            self.focus = Focus::Tree;
        }
        // The session likely committed; refresh this workspace's branch status.
        self.request_branch_status_refresh();
    }

    /// Detach the selected workspace's embedded panes (tmux prefix-d, and the
    /// tree's `x`): kill the processes to free their memory but keep the
    /// persisted session entries, so reopening restores every tab (Claude tabs
    /// resume their conversation, shell tabs respawn fresh; a mid-turn Claude is
    /// interrupted, only what it flushed resumes). The keep-the-ids counterpart
    /// of [`Self::close_active_session`].
    fn detach_selected_workspace(&mut self) {
        let Some(ws_id) = self.selected_workspace().map(|w| w.id.clone()) else {
            return;
        };
        // The workspace-scoped twin of [`Self::shutdown_panes`]: dropping the
        // panes one by one costs a full SIGHUP→250ms→SIGKILL each (a Node
        // `claude` ignores SIGHUP), so a 9-tab detach would freeze the UI
        // ~2.25s. Broadcast the hangup, wait one shared grace, SIGKILL+reap
        // stragglers — the per-pane `Drop` then sees an exited child and
        // returns instantly.
        if let Some(sessions) = self.embedded.get_mut(&ws_id) {
            for tab in sessions.tabs.iter_mut() {
                tab.pane.signal_hangup();
            }
            for _ in 0..5 {
                if sessions.tabs.iter_mut().all(|t| t.pane.has_exited()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            for tab in sessions.tabs.iter_mut() {
                tab.pane.force_kill_and_reap();
            }
        }
        self.embedded.remove(&ws_id);
        // Also flip a legacy `Running` stream session to Stopped: those are
        // never resurrected, so one left `Running` keeps a phantom "running"
        // tree icon until the next startup normalization.
        let legacy = self
            .state
            .find_session_by_workspace(&ws_id)
            .filter(|s| s.status == SessionStatus::Running)
            .map(|s| s.id.clone());
        if let Some(session_id) = legacy {
            let _ = self.state.update_session_status(&session_id, SessionStatus::Stopped);
        }
        self.focus = Focus::Tree;
        // The sessions likely committed; refresh this workspace's branch status.
        self.request_branch_status_refresh();
    }

    /// Open the Rename Session modal for the selected workspace's active tab,
    /// prefilled with its current title. Focus stays on the embedded pane (the
    /// modal renders over it and intercepts keys via the `!modal.is_active()`
    /// guards), so submitting/cancelling drops the user straight back in.
    fn open_rename_active_session(&mut self) {
        let Some(ws_id) = self.selected_workspace().map(|w| w.id.clone()) else {
            return;
        };
        let Some(session_id) = self
            .embedded
            .get(&ws_id)
            .and_then(|s| s.active_tab())
            .map(|t| t.id.clone())
        else {
            return;
        };
        let current = self
            .state
            .embedded_session_title(&ws_id, &session_id)
            .unwrap_or("")
            .to_string();
        let cursor = current.len();
        self.embedded_prefix = false;
        self.modal = modal::ModalState::RenameSession {
            ws_id,
            session_id,
            input: current,
            cursor,
            error: None,
        };
    }

    /// Open an additional tab of `kind` for a workspace (up to the shared cap)
    /// and focus it.
    fn new_tab(&mut self, kind: TabKind, ws_id: &str) {
        self.select_workspace_row(ws_id);
        let count = self.embedded.get(ws_id).map(|s| s.tabs.len()).unwrap_or(0);
        if count >= MAX_SESSION_TABS {
            self.embed_error = Some((
                ws_id.to_string(),
                format!("Maximum {MAX_SESSION_TABS} session tabs reached."),
            ));
            return;
        }
        let Some(ws) = self.workspaces.iter().find(|w| w.id == ws_id).cloned() else {
            return;
        };
        // Cleared BEFORE the spawn so opening a tab drops a stale error without
        // swallowing a persist-failure warning the spawn itself may set.
        self.embed_error = None;
        if self.spawn_tab(kind, ws_id, &ws.working_dir, &ws.name, None) {
            self.focus = Focus::Embedded;
            self.embedded_prefix = false;
        }
    }

    /// Forward a key to the active session of the selected workspace.
    /// Returns false when there is no pane to forward to.
    fn forward_to_embedded(&mut self, key: KeyEvent) -> bool {
        match self.active_pane_mut() {
            Some(pane) => {
                let _ = pane.send_key(key);
                true
            }
            None => false,
        }
    }

    /// Forward a mouse event to the visible embedded pane (the selected
    /// workspace's active tab), translated into the pane's inner coordinate
    /// space. Reached while the pane is focused AND from tree-focused
    /// hover-scroll, so it must not assume `Focus::Embedded`. The pane only
    /// acts on it if claude enabled mouse reporting. Events outside the pane's
    /// inner area are ignored (so a click on the border/tree can't strand the
    /// pane).
    ///
    /// Known limitation: a drag/release that leaves the pane is dropped rather
    /// than clamped to the edge, so a selection started inside and released
    /// outside isn't delivered to claude. Recoverable by clicking inside again.
    fn forward_mouse_to_embedded(&mut self, mouse: MouseEvent) {
        let Some((col, row)) = translate_mouse(self.right_pane_area, mouse.column, mouse.row)
        else {
            return;
        };
        if let Some(pane) = self.active_pane_mut() {
            pane.send_mouse(mouse.kind, mouse.modifiers, col, row);
        }
    }

    /// Handle a mouse event while an embedded pane is focused: a left-click on a
    /// session tab / `[+]` drives kommand0; everything else is forwarded to the
    /// active session (translated to its coords).
    fn handle_embedded_mouse(&mut self, mouse: MouseEvent) {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        // ponytail: deliberately hijacks ScrollLeft/Right (and Shift+wheel)
        // from a focused mouse-mode child (encode_mouse SGR buttons 66/67):
        // claude has no horizontal-scroll UI. Gate the helper on wants_mouse()
        // if a horizontal-scrolling child ever matters.
        if self.hscroll_switch_tab(mouse) {
            return;
        }
        match mouse.kind {
            MouseEventKind::Moved => {
                // Keep mouse_pos current so tab hover styling works in Embedded mode.
                self.mouse_pos = Some((mouse.column, mouse.row));
                self.forward_mouse_to_embedded(mouse);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // A click in the tree pane (including the empty space below the
                // rows) focuses it — the mirror of clicking the content pane to
                // focus claude. Delegate to the tree handler for focus + select.
                if buttons::is_hovered(Some((mouse.column, mouse.row)), self.pane_areas.tree) {
                    mouse::handle_mouse(self, mouse);
                    return;
                }
                let pos = Some((mouse.column, mouse.row));
                let tab_action = self.hit_regions.iter().find_map(|r| {
                    if buttons::is_hovered(pos, r.area)
                        && matches!(
                            r.action,
                            buttons::HitAction::SelectSessionTab { .. }
                                | buttons::HitAction::NewSessionTab { .. }
                        )
                    {
                        Some(r.action.clone())
                    } else {
                        None
                    }
                });
                match tab_action {
                    Some(action) => self.pending_button_action = Some(action),
                    None => self.forward_mouse_to_embedded(mouse),
                }
            }
            _ => self.forward_mouse_to_embedded(mouse),
        }
    }

    /// Drop session tabs whose child has exited, and whole workspaces that were
    /// deleted. Applies the per-tab resume-failure net, keeps the active tab
    /// stable by identity, and leaves Embedded focus if the selected workspace
    /// emptied. Pane's `Drop` terminates the child.
    fn reap_embedded(&mut self, now: Instant) {
        // One pass: collect exited tabs (with the signal-vs-exit distinction in
        // `code`) and live resumed tabs that are showing claude's resume-miss
        // error — claude prints it and STAYS ALIVE, so the exit-code net alone
        // would never see it and the dead id would never be cleared (every reopen
        // would re-resume the same gone session).
        let mut exited: Vec<(String, String, bool, Instant, Option<i32>, TabKind)> = Vec::new();
        let mut resume_missed: Vec<(String, String)> = Vec::new();
        // The miss scan is paced (see RESUME_MISS_SCAN_EVERY); exit reaping
        // below stays per-tick.
        let scan_misses =
            now.saturating_duration_since(self.last_resume_scan) >= RESUME_MISS_SCAN_EVERY;
        if scan_misses {
            self.last_resume_scan = now;
        }
        for (ws_id, sessions) in self.embedded.iter_mut() {
            for tab in sessions.tabs.iter_mut() {
                if let Some(code) = tab.pane.try_wait() {
                    exited.push((
                        ws_id.clone(),
                        tab.id.clone(),
                        tab.was_resume,
                        tab.spawned,
                        code,
                        tab.kind,
                    ));
                } else if scan_misses
                    && tab.kind == TabKind::Claude
                    && tab.was_resume
                    && now.saturating_duration_since(tab.spawned) < RESUME_CHECK_WINDOW
                {
                    // The marker is claude's text, so the scan is claude-only
                    // (which also skips full-grid serialization of the other
                    // kinds' panes); a failed resume of any other resumable
                    // kind rides the fast-exit net above. Require BOTH the
                    // marker and this tab's (random uuid) session id, so a
                    // resumed conversation that merely mentions the phrase
                    // can't be mistaken for a real miss.
                    let screen = tab.pane.screen_contents();
                    if screen.contains(RESUME_MISS_MARKER) && screen.contains(&tab.id) {
                        resume_missed.push((ws_id.clone(), tab.id.clone()));
                    }
                }
            }
        }

        // Resume failures: a resumed tab that exited fast non-zero, OR one still
        // alive showing the resume-miss marker. Auto-heal each by replacing the
        // gone session with a fresh one of the SAME kind in the SAME tab slot
        // (the replacement gets a new id, so the retain pass below leaves it
        // alone). If the fresh spawn also fails, fall back to dropping the tab
        // with the reopen message.
        let mut failed_resume: Vec<(String, String, TabKind)> = exited
            .iter()
            .filter(|(_, _, was_resume, spawned, code, _)| resume_failed(*spawned, *was_resume, now, *code))
            .map(|(ws, tab, .., kind)| (ws.clone(), tab.clone(), *kind))
            .collect();
        failed_resume.extend(
            resume_missed
                .iter()
                .cloned()
                .map(|(ws, tab)| (ws, tab, TabKind::Claude)),
        );
        for (ws_id, tab_id, kind) in &failed_resume {
            let ws_dir = self
                .workspaces
                .iter()
                .find(|w| &w.id == ws_id)
                .map(|w| w.working_dir.clone());
            match ws_dir {
                Some(dir) if self.heal_resume(*kind, ws_id, tab_id, &dir, now) => {}
                _ => {
                    // Couldn't start a fresh session — forget the id and drop the
                    // stuck/dead tab; the message tells the user to reopen.
                    self.state.remove_embedded_session(ws_id, tab_id);
                    self.save_state();
                    self.embed_error = Some((ws_id.clone(), RESUME_FAIL_MSG.to_string()));
                }
            }
        }

        // A non-resumable tab (e.g. a shell the user `exit`ed) that exited is
        // gone for good: forget its persisted entry so reopen doesn't respawn
        // a tab the user ended. Resumable kinds keep their id (an exited
        // conversation resumes later). Non-resumable tabs never satisfy
        // resume_failed (was_resume is always false), so this can't
        // double-remove with the fallback above.
        let mut forgot = false;
        for (ws_id, tab_id, .., kind) in &exited {
            if !kind.resumable() {
                self.state.remove_embedded_session(ws_id, tab_id);
                forgot = true;
            }
        }
        if forgot {
            self.save_state();
        }

        let dead: HashSet<(String, String)> = exited
            .into_iter()
            .map(|(ws, tab, ..)| (ws, tab))
            .chain(resume_missed)
            .collect();
        let live_ws: HashSet<&String> = self.workspaces.iter().map(|w| &w.id).collect();
        let mut remove_ws: Vec<String> = Vec::new();
        for (ws_id, sessions) in self.embedded.iter_mut() {
            if !live_ws.contains(ws_id) {
                remove_ws.push(ws_id.clone()); // workspace was deleted
                continue;
            }
            // Drop dead tabs, then rebase `active` to the same tab by identity (a
            // saturating clamp would silently focus a different session).
            let active_id = sessions.tabs.get(sessions.active).map(|t| t.id.clone());
            sessions
                .tabs
                .retain(|t| !dead.contains(&(ws_id.clone(), t.id.clone())));
            if sessions.tabs.is_empty() {
                remove_ws.push(ws_id.clone());
            } else {
                sessions.active = active_id
                    .and_then(|id| sessions.tabs.iter().position(|t| t.id == id))
                    .unwrap_or_else(|| sessions.active.min(sessions.tabs.len() - 1));
            }
        }
        for ws_id in &remove_ws {
            self.embedded.remove(ws_id);
        }
        // Dropped tabs/workspaces shift what renders where — a stale highlight
        // must not land on the neighbor tab.
        if !dead.is_empty() || !remove_ws.is_empty() {
            self.pane_selection = None;
        }
        if self.focus == Focus::Embedded {
            let gone = self
                .selected_workspace()
                .map(|w| !self.embedded.contains_key(&w.id))
                .unwrap_or(true);
            if gone {
                self.focus = Focus::Tree;
            }
        }
    }

    /// Refresh `waiting_response` (the activity-spinner set): output deltas
    /// across all tabs via [`Self::apply_pane_activity`], then
    /// [`Self::apply_shell_busy`] overrides shell tabs by their PTY foreground
    /// process group. Called every tick.
    fn update_pane_activity(&mut self, now: Instant) {
        let seqs: Vec<(String, u64, Option<Instant>)> = self
            .embedded
            .values()
            .flat_map(|s| {
                s.tabs
                    .iter()
                    .map(|t| (t.id.clone(), t.pane.output_seq(), t.pane.last_input_at()))
            })
            .collect();
        self.apply_pane_activity(now, &seqs);
        // Shell tabs report activity by their PTY foreground process group (a
        // command is running) gated on recent output, so a streaming build spins
        // but a quiet foreground process (an open editor/pager) decays to idle.
        // Claude tabs keep the output-based signal above (claude streams as it works).
        let shell_busy: Vec<(String, Option<bool>)> = self
            .embedded
            .values()
            .flat_map(|s| {
                s.tabs
                    .iter()
                    .filter(|t| t.kind == TabKind::Shell)
                    .map(|t| (t.id.clone(), t.pane.foreground_busy()))
            })
            .collect();
        self.apply_shell_busy(now, &shell_busy);
        // Keep the on-screen session marked seen, then latch any others that went
        // quiet with unseen output. Order matters: clear before latching so the
        // session you're watching is never flagged.
        self.mark_active_viewed();
        let seq_pairs: Vec<(String, u64)> = seqs
            .iter()
            .map(|(id, seq, _)| (id.clone(), *seq))
            .collect();
        let newly_waiting = self.recompute_attention(now, &seq_pairs);
        if !newly_waiting.is_empty() {
            self.notify_newly_waiting(&newly_waiting);
        }
    }

    /// Fire bell/desktop notifications for sessions that just entered "needs
    /// you" (the rising edge from [`Self::recompute_attention`]), per the
    /// configured `notify_mode`. A no-op when notifications are off, so tests and
    /// the default install stay silent.
    fn notify_newly_waiting(&self, session_ids: &[String]) {
        // The bell needs no name — ring it as soon as any session newly needs
        // you (robust even if the workspace-name lookup below comes up empty).
        if self.notify_mode.wants_bell() {
            notify::ring_bell(&mut std::io::stdout());
        }
        if !self.notify_mode.wants_desktop() {
            return;
        }
        // Desktop notifications name the workspace(s) that went quiet, deduped
        // (a workspace with two tabs going quiet at once still alerts once).
        let mut names: Vec<String> = Vec::new();
        for (ws_id, sessions) in &self.embedded {
            if !sessions.tabs.iter().any(|t| session_ids.contains(&t.id)) {
                continue;
            }
            let Some(ws) = self.workspaces.iter().find(|w| &w.id == ws_id) else {
                continue;
            };
            if !names.contains(&ws.name) {
                names.push(ws.name.clone());
            }
        }
        for name in &names {
            let body = format!("{name} is waiting");
            if let Some((prog, args)) = notify::desktop_command("kommand0", &body) {
                // Fire-and-forget on a detached thread that waits on (reaps) the
                // short-lived notifier so it can't block the loop or zombie; a
                // missing notifier (e.g. no `notify-send`) is silently ignored.
                std::thread::spawn(move || {
                    let _ = std::process::Command::new(prog)
                        .args(args)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                });
            }
        }
    }

    /// Mark the currently-viewed session (the focused workspace's active tab) as
    /// seen up to its latest output, and clear any pending attention on it. Runs
    /// every frame (before draw) and every tick, so a session you're looking at
    /// never raises the "needs you" flag.
    pub(crate) fn mark_active_viewed(&mut self) {
        if self.focus != Focus::Embedded {
            return;
        }
        let Some(ws_id) = self.selected_workspace().map(|w| w.id.clone()) else {
            return;
        };
        let Some((id, seq)) = self
            .embedded
            .get(&ws_id)
            .and_then(|s| s.active_tab())
            .map(|t| (t.id.clone(), t.pane.output_seq()))
        else {
            return;
        };
        self.viewed_seq.insert(id.clone(), seq);
        self.attention.remove(&id);
    }

    /// Latch sessions that produced unseen output and have since gone quiet. A
    /// latched session stays flagged until viewed (or its pane disappears), so a
    /// mid-turn tool pause that later resumes can't strobe the indicator.
    /// Returns the session ids that *newly* entered the "needs you" set on this
    /// pass (the rising edge) — used to fire one-shot attention notifications.
    fn recompute_attention(&mut self, now: Instant, seqs: &[(String, u64)]) -> Vec<String> {
        // Only flag "needs you" once a session has been genuinely idle a while,
        // not on a brief mid-turn pause. Kept above ACTIVE_WINDOW so the spinner
        // has faded first (no overlap between "working" and "needs you").
        const ATTENTION_SETTLE: Duration = Duration::from_millis(3000);
        let mut newly = Vec::new();
        for (id, seq) in seqs {
            let seen = self.viewed_seq.get(id).copied().unwrap_or(0);
            let unseen = *seq > seen;
            let settled = self
                .last_output_at
                .get(id)
                .is_some_and(|t| now.duration_since(*t) >= ATTENTION_SETTLE);
            // `insert` returns true only on the rising edge; the latch keeps a
            // session flagged after that, so a notification fires at most once.
            if unseen && settled && self.attention.insert(id.clone()) {
                newly.push(id.clone());
            }
        }
        // Forget sessions whose pane is gone (closed/healed-to-a-new-id).
        let live: HashSet<&str> = seqs.iter().map(|(id, _)| id.as_str()).collect();
        self.viewed_seq.retain(|id, _| live.contains(id.as_str()));
        self.attention.retain(|id| live.contains(id.as_str()));
        newly
    }

    /// Keep every live embedded pane sized to the visible content area — not just
    /// the one on screen — so switching tabs or workspaces is instant and a
    /// terminal resize reaches the background panes too. `Pane::resize` is a
    /// no-op (no PTY resize, no SIGWINCH) when the size is unchanged, so calling
    /// this every frame only does work on an actual resize.
    pub(crate) fn resize_embedded_panes(&mut self, content: ratatui::layout::Rect) {
        for sessions in self.embedded.values_mut() {
            for tab in sessions.tabs.iter_mut() {
                let _ = tab.pane.resize(content.height, content.width);
            }
        }
    }

    /// Tear down every embedded pane in ONE shared grace period at quit. Dropping
    /// the panes one by one runs a full SIGHUP→250ms→SIGKILL per pane, so quitting
    /// with N sessions that ignore SIGHUP (a Node `claude` does) froze the UI
    /// ~N×250ms. Instead: broadcast SIGHUP to every child, wait once, then
    /// SIGKILL+reap any straggler — the per-pane `Drop` then sees an exited child
    /// and returns instantly.
    fn shutdown_panes(&mut self) {
        let mut any = false;
        for sessions in self.embedded.values_mut() {
            for tab in sessions.tabs.iter_mut() {
                tab.pane.signal_hangup();
                any = true;
            }
        }
        if !any {
            return;
        }
        // One shared grace window: poll up to ~250ms, breaking early once every
        // child has exited (so SIGHUP-respecting children don't cost the full
        // wait). Then SIGKILL+reap any straggler that ignored the hangup.
        for _ in 0..5 {
            let all_done = self
                .embedded
                .values_mut()
                .all(|s| s.tabs.iter_mut().all(|t| t.pane.has_exited()));
            if all_done {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for sessions in self.embedded.values_mut() {
            for tab in sessions.tabs.iter_mut() {
                tab.pane.force_kill_and_reap();
            }
        }
    }

    /// Kick off an off-loop git-status refresh for every workspace (no-op if one
    /// is already running or the channel isn't wired). The worker computes each
    /// workspace's [`kommand0_core::branch_status`] and sends the whole map back;
    /// the event loop replaces the cache wholesale, so deleted workspaces drop
    /// out on the next refresh (no manual pruning).
    fn request_branch_status_refresh(&mut self) {
        if self.status_inflight {
            return;
        }
        let Some(tx) = self.status_tx.clone() else {
            return; // not wired (e.g. unit tests drive the cache directly)
        };
        // Only workspaces with their own worktree/branch — a fallback workspace's
        // working_dir is the shared repo root, whose status the UI never shows.
        let targets: Vec<(String, String)> = self
            .workspaces
            .iter()
            .filter(|w| w.worktree_path.is_some())
            .map(|w| (w.id.clone(), w.working_dir.clone()))
            .collect();
        if targets.is_empty() {
            return;
        }
        self.status_inflight = true;
        std::thread::spawn(move || {
            // The guard sends `result` on drop — including on panic — so the
            // event loop always clears `status_inflight`.
            let mut guard = StatusRefreshGuard {
                tx,
                result: Some(HashMap::new()),
            };
            let map = guard.result.as_mut().expect("result present until drop");
            for (id, dir) in targets {
                if let Some(status) = kommand0_core::branch_status(&dir) {
                    map.insert(id, status);
                }
            }
        });
    }

    /// Kick off an off-loop PR/CI-status refresh (no-op if one is running or the
    /// channel isn't wired). Runs one [`kommand0_core::pr_statuses`] per repo, then
    /// matches each worktree-backed workspace's `branch_name` against that repo's
    /// `headRefName` → PrStatus map. Sends the whole `ws_id` → PrStatus map back;
    /// the event loop replaces the cache wholesale (deleted workspaces / closed
    /// PRs drop out on the next refresh).
    fn request_pr_status_refresh(&mut self) {
        if self.pr_status_inflight {
            return;
        }
        let Some(tx) = self.pr_status_tx.clone() else {
            return; // not wired (e.g. unit tests drive the cache directly)
        };
        // Group workspaces by their repo's path (one `gh pr list` per repo).
        // Only own-branch workspaces have a PR to look up.
        let mut by_repo: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for w in &self.workspaces {
            let Some(branch) = &w.branch_name else {
                continue;
            };
            if w.worktree_path.is_none() {
                continue;
            }
            let Some(repo) = self.repos.iter().find(|r| r.id == w.repo_id) else {
                continue;
            };
            by_repo
                .entry(repo.path.clone())
                .or_default()
                .push((w.id.clone(), branch.clone()));
        }
        if by_repo.is_empty() {
            // No own-branch workspaces to query — drop any stale entries (e.g.
            // after the last workspace was deleted) instead of caching forever.
            self.pr_status.clear();
            return;
        }
        self.pr_status_inflight = true;
        std::thread::spawn(move || {
            // The guard sends `result` on drop — including on panic — so the
            // event loop always clears `pr_status_inflight`.
            let mut guard = PrStatusRefreshGuard {
                tx,
                result: Some(HashMap::new()),
            };
            let map = guard.result.as_mut().expect("result present until drop");
            for (repo_path, workspaces) in by_repo {
                let prs = kommand0_core::pr_statuses(&repo_path);
                for (ws_id, branch) in workspaces {
                    if let Some(status) = prs.get(&branch) {
                        map.insert(ws_id, status.clone());
                    }
                }
            }
        });
    }

    /// Post-process a workspace-create attempt from the add-workspace flow. On
    /// success: sync workspaces, expand the repo, rebuild, refresh branch status.
    /// On failure: reopen the Add-Workspace modal with the typed `name`, the given
    /// `branch` (the explicit-branch path passes the user's typed branch so it
    /// isn't wiped on error; the fork/blank paths pass an empty string), and the
    /// error.
    fn finish_add_workspace(
        &mut self,
        result: anyhow::Result<Workspace>,
        repo_id: String,
        repo_name: String,
        name: String,
        branch: String,
    ) {
        match result {
            Ok(_) => {
                self.workspaces = self.state.workspaces.clone();
                self.expanded.insert(repo_id);
                self.rebuild_tree();
                self.request_branch_status_refresh();
            }
            Err(e) => {
                self.modal = modal::ModalState::AddWorkspace {
                    repo_id,
                    repo_name,
                    input: name,
                    cursor: 0,
                    branch,
                    branch_cursor: 0,
                    field: modal::AddWorkspaceField::Name,
                    error: Some(e.to_string()),
                };
            }
        }
    }

    /// Populate and open the review-diff dialog for a workspace: the PR-style
    /// `git diff <default>...HEAD` of its worktree (committed changes only), as a
    /// collapsible file tree + per-file diff. Computed synchronously — a local git
    /// diff is fast; move off-loop if a huge worktree ever hitches the render loop.
    fn open_diff(&mut self, ws_id: &str) {
        let Some(ws) = self.workspaces.iter().find(|w| w.id == ws_id) else {
            return;
        };
        self.show_diff = true;
        self.diff_focus = DiffFocus::Files;
        self.diff_files.clear();
        self.diff_expanded.clear();
        self.diff_selected = 0;
        self.diff_list_scroll = 0;
        self.diff_scroll = 0;
        self.diff_note.clear();
        // The overlay and the tree share `pending_g`; a half-typed `gg` in the
        // tree must not complete as the overlay's jump-to-first (and vice-versa).
        self.pending_g = false;
        // Own-branch workspaces only: a fallback workspace's `working_dir` is the
        // shared repo root, not a branch to review (mirrors branch_status, which
        // gates on `worktree_path`).
        let Some(worktree) = ws.worktree_path.clone() else {
            self.diff_title = ws.name.clone();
            self.diff_rows.clear();
            self.diff_note = "This workspace has no branch to review.".to_string();
            return;
        };
        self.diff_title = match &ws.branch_name {
            Some(b) => format!("{} ({b})", ws.name),
            None => ws.name.clone(),
        };
        // Distinguish "couldn't compute" (not a repo / base unresolved → None)
        // from a genuinely-empty diff (Some(empty)); both leave no rows, but the
        // note the left pane shows differs.
        self.diff_files = match kommand0_core::diff_files_vs_default_branch(&worktree) {
            Some(files) => files,
            None => {
                self.diff_rows.clear();
                self.diff_note = "Couldn't compute a diff — not a git repo.".to_string();
                return;
            }
        };
        if self.diff_files.is_empty() {
            self.diff_note =
                "No committed changes on this branch vs the default branch.".to_string();
        }
        // Default to every folder expanded, then flatten to the visible rows and
        // land the selection on the first File row (so the right pane shows a diff).
        for path in self.diff_files.iter().map(|f| f.path.clone()).collect::<Vec<_>>() {
            for parent in folder_prefixes(&path) {
                self.diff_expanded.insert(parent);
            }
        }
        self.rebuild_diff_rows();
        self.diff_selected = self
            .diff_rows
            .iter()
            .position(|r| matches!(r, DiffRow::File { .. }))
            .unwrap_or(0);
    }

    /// Flatten `diff_files` paths into the visible `diff_rows`: nested folders (in
    /// sorted order, one row each, hidden under a collapsed ancestor) and their
    /// files. Mirrors `rebuild_tree`. Clamps `diff_selected` into the new range.
    pub(crate) fn rebuild_diff_rows(&mut self) {
        self.diff_rows.clear();
        // Sort file indices by path so folders/files come out in a stable order.
        let mut order: Vec<usize> = (0..self.diff_files.len()).collect();
        order.sort_by(|&a, &b| self.diff_files[a].path.cmp(&self.diff_files[b].path));

        // Track which folder paths we've already emitted a row for.
        let mut seen_folders: HashSet<String> = HashSet::new();
        for idx in order {
            let path = &self.diff_files[idx].path;
            let comps: Vec<&str> = path.split('/').collect();
            // Emit any not-yet-seen ancestor folders, deepest last, skipping the
            // subtree of a collapsed folder.
            let mut prefix = String::new();
            let mut hidden = false;
            for comp in &comps[..comps.len().saturating_sub(1)] {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(comp);
                let depth = prefix.matches('/').count() as u16;
                if !seen_folders.contains(&prefix) {
                    seen_folders.insert(prefix.clone());
                    if !hidden {
                        self.diff_rows.push(DiffRow::Folder {
                            path: prefix.clone(),
                            name: (*comp).to_string(),
                            depth,
                        });
                    }
                }
                // A collapsed folder hides everything below it (still records the
                // ancestors as "seen" so the loop doesn't re-emit them elsewhere).
                if !self.diff_expanded.contains(&prefix) {
                    hidden = true;
                }
            }
            if !hidden {
                let depth = (comps.len() - 1) as u16;
                self.diff_rows.push(DiffRow::File {
                    file_idx: idx,
                    name: (*comps.last().unwrap()).to_string(),
                    depth,
                });
            }
        }
        if self.diff_selected >= self.diff_rows.len() {
            self.diff_selected = self.diff_rows.len().saturating_sub(1);
        }
    }

    /// Reset the right-pane scroll to the top when the selected row is a File (it
    /// now shows that file from the start). A no-op on a Folder selection.
    fn diff_reset_body_scroll_if_file(&mut self) {
        if matches!(self.diff_rows.get(self.diff_selected), Some(DiffRow::File { .. })) {
            self.diff_scroll = 0;
        }
    }

    fn diff_toggle_focus(&mut self) {
        // Switching panes abandons any half-typed `gg` (shared with the tree).
        self.pending_g = false;
        self.diff_focus = match self.diff_focus {
            DiffFocus::Files => DiffFocus::Diff,
            DiffFocus::Diff => DiffFocus::Files,
        };
    }

    /// Move the file-tree selection by `delta` (clamped, no wrap). Landing on a
    /// File resets the diff scroll so the right pane shows it from the top.
    fn diff_select_move(&mut self, delta: i32) {
        if self.diff_rows.is_empty() {
            return;
        }
        let last = self.diff_rows.len() as i32 - 1;
        let next = (self.diff_selected as i32 + delta).clamp(0, last);
        self.diff_selected = next as usize;
        self.diff_reset_body_scroll_if_file();
    }

    fn diff_select_first(&mut self) {
        self.diff_selected = 0;
        self.diff_reset_body_scroll_if_file();
    }

    fn diff_select_last(&mut self) {
        self.diff_selected = self.diff_rows.len().saturating_sub(1);
        self.diff_reset_body_scroll_if_file();
    }

    /// `l`/`Right`/`Enter`: on a Folder, expand it; on a File, move focus to the
    /// diff pane.
    fn diff_expand_or_enter(&mut self) {
        match self.diff_rows.get(self.diff_selected) {
            Some(DiffRow::Folder { path, .. }) => {
                let path = path.clone();
                if self.diff_expanded.insert(path) {
                    self.rebuild_diff_rows();
                }
            }
            Some(DiffRow::File { .. }) => self.diff_focus = DiffFocus::Diff,
            None => {}
        }
    }

    /// `h`/`Left`: collapse an expanded Folder; otherwise select the parent folder
    /// row (the nearest shallower Folder above the selection).
    fn diff_collapse_or_parent(&mut self) {
        match self.diff_rows.get(self.diff_selected) {
            Some(DiffRow::Folder { path, .. }) if self.diff_expanded.contains(path) => {
                let path = path.clone();
                self.diff_expanded.remove(&path);
                self.rebuild_diff_rows();
            }
            Some(row) => {
                let depth = match row {
                    DiffRow::Folder { depth, .. } | DiffRow::File { depth, .. } => *depth,
                };
                if depth > 0 {
                    // Walk up to the nearest folder with a smaller depth.
                    for i in (0..self.diff_selected).rev() {
                        if let DiffRow::Folder { depth: d, .. } = self.diff_rows[i]
                            && d < depth
                        {
                            self.diff_selected = i;
                            self.diff_reset_body_scroll_if_file();
                            break;
                        }
                    }
                }
            }
            None => {}
        }
    }

    /// Handle a click at `(col, row)` inside the diff dialog. A click in the file
    /// list selects/toggles a row; a click in the diff body focuses that pane.
    /// Returns whether the click landed on the dialog.
    fn diff_handle_click(&mut self, col: u16, row: u16) -> bool {
        if mouse::contains(self.diff_list_area, col, row) {
            let clicked =
                row.saturating_sub(self.diff_list_area.y) as usize + self.diff_list_scroll as usize;
            if clicked < self.diff_rows.len() {
                self.diff_selected = clicked;
                self.diff_focus = DiffFocus::Files;
                match &self.diff_rows[clicked] {
                    DiffRow::Folder { path, .. } => {
                        let path = path.clone();
                        if !self.diff_expanded.remove(&path) {
                            self.diff_expanded.insert(path);
                        }
                        self.rebuild_diff_rows();
                    }
                    DiffRow::File { .. } => self.diff_scroll = 0,
                }
            }
            return true;
        }
        if mouse::contains(self.diff_body_area, col, row) {
            self.diff_focus = DiffFocus::Diff;
            return true;
        }
        false
    }

    /// Handle a scroll-wheel event inside the diff dialog: over the body it scrolls
    /// the diff, over the list it moves the selection. Returns whether it landed.
    fn diff_handle_scroll(&mut self, col: u16, row: u16, up: bool) -> bool {
        if mouse::contains(self.diff_body_area, col, row) {
            self.diff_scroll = if up {
                self.diff_scroll.saturating_sub(1)
            } else {
                self.diff_scroll.saturating_add(1)
            };
            return true;
        }
        if mouse::contains(self.diff_list_area, col, row) {
            self.diff_select_move(if up { -1 } else { 1 });
            return true;
        }
        false
    }

    /// Open the cleanup confirmation modal for a workspace (own-branch only),
    /// pre-filling the branch and any cached uncommitted/unpushed warnings.
    fn cleanup_workspace_prompt(&mut self, ws_id: &str) {
        if self.cleanup_inflight.contains(ws_id) {
            return;
        }
        let Some(ws) = self.workspaces.iter().find(|w| w.id == ws_id) else {
            return;
        };
        let (Some(_), Some(branch)) = (ws.worktree_path.as_ref(), ws.branch_name.clone()) else {
            return; // fallback workspace: nothing to clean up
        };
        let ws_name = ws.name.clone();
        let st = self.branch_status.get(ws_id);
        let dirty = st.map(|s| s.dirty).unwrap_or(false);
        let unpushed = st.map(|s| s.ahead > 0).unwrap_or(false);
        self.modal = modal::ModalState::ConfirmCleanup {
            ws_id: ws_id.to_string(),
            ws_name,
            branch,
            dirty,
            unpushed,
        };
    }

    /// Run the merged-workspace cleanup off the render loop (worktree removal +
    /// branch deletion happen in core, which enforces the safety guards). Tears
    /// down any live embedded pane first so its cwd isn't yanked out from under it.
    fn start_cleanup(&mut self, ws_id: &str) {
        if self.cleanup_inflight.contains(ws_id) {
            return;
        }
        let Some(ws) = self.workspaces.iter().find(|w| w.id == ws_id) else {
            return;
        };
        let (Some(worktree), Some(branch)) = (ws.worktree_path.clone(), ws.branch_name.clone())
        else {
            return;
        };
        let Some(repo) = self
            .repos
            .iter()
            .find(|r| r.id == ws.repo_id)
            .map(|r| r.path.clone())
        else {
            return;
        };
        let Some(tx) = self.cleanup_tx.clone() else {
            return; // not wired (unit tests)
        };
        // Tear down the embedded pane synchronously (Drop terminates the child),
        // so the worktree dir isn't removed while a claude is running inside it.
        self.embedded.remove(ws_id);
        self.cleanup_inflight.insert(ws_id.to_string());
        self.cleanup_result.remove(ws_id);
        let id = ws_id.to_string();
        std::thread::spawn(move || {
            let mut guard = CleanupGuard {
                tx,
                payload: Some((id.clone(), Err("the cleanup was interrupted".to_string()))),
            };
            let result = kommand0_core::cleanup_merged_workspace(&repo, &worktree, &branch);
            guard.payload = Some((id, result));
        });
    }

    /// Whether any of a workspace's session tabs needs the user's attention.
    pub(crate) fn ws_needs_attention(&self, ws_id: &str) -> bool {
        self.embedded
            .get(ws_id)
            .map(|s| s.tabs.iter().any(|t| self.attention.contains(&t.id)))
            .unwrap_or(false)
    }

    /// Core of [`Self::update_pane_activity`], split out for testing: given the
    /// observed `(session_id, output_seq, last_input_at)` for the live panes,
    /// arm a session as active only after two consecutive ticks of new output
    /// (debounce), let it decay after `ACTIVE_WINDOW`, and prune sessions no
    /// longer present. Output arriving within `INPUT_GRACE` of user input to
    /// that pane is the child redrawing in response (a scroll wheel tick, a
    /// keystroke echo), not work: it updates `pane_seen` but neither arms the
    /// spinner nor stamps `last_output_at` (so a scrolled pager stays idle for
    /// [`Self::apply_shell_busy`] too), and it keeps a fully-seen session's
    /// `viewed_seq` current so a scroll-provoked redraw can't latch a false
    /// "needs you" (a genuinely unseen backlog still latches).
    fn apply_pane_activity(&mut self, now: Instant, seqs: &[(String, u64, Option<Instant>)]) {
        // Generous so the spinner rides through Claude's bursty output without
        // flicker; it decays only after a real ~2s pause (feel over accuracy).
        // Kept below ATTENTION_SETTLE so the spinner fades before the "needs you"
        // dot can appear — the two never show at once.
        const ACTIVE_WINDOW: Duration = Duration::from_millis(2000);
        // Interaction-driven redraws land well inside this; real work keeps
        // producing past it, so at worst the spinner starts one grace late.
        const INPUT_GRACE: Duration = Duration::from_millis(500);
        for (id, seq, input_at) in seqs {
            let had_new = self.pane_seen.get(id) != Some(seq);
            // Captured BEFORE the insert: "fully seen" below must compare
            // against what the session had produced before THIS chunk.
            let prev_seen = self.pane_seen.get(id).copied().unwrap_or(0);
            self.pane_seen.insert(id.clone(), *seq);
            let user_caused =
                input_at.is_some_and(|t| now.saturating_duration_since(t) < INPUT_GRACE);
            if had_new && !user_caused {
                // Stamp the last-output time (no debounce) for the settle check.
                self.last_output_at.insert(id.clone(), now);
                if self.pane_pending.contains(id) {
                    self.pane_active_until.insert(id.clone(), now + ACTIVE_WINDOW);
                } else {
                    self.pane_pending.insert(id.clone());
                }
            } else if !had_new {
                self.pane_pending.remove(id);
            } else {
                // had_new && user_caused: swallowed for the spinner. Keep any
                // pending arm so a stray scroll doesn't reset genuinely
                // starting work. A session that was fully seen before this
                // chunk stays seen: the redraw the user's own input provoked
                // (hover-scroll runs tree-focused, where mark_active_viewed
                // never fires) must not latch a false "needs you". A
                // genuinely-unseen backlog still latches.
                if self.viewed_seq.get(id).copied().unwrap_or(0) >= prev_seen {
                    self.viewed_seq.insert(id.clone(), *seq);
                }
            }
        }
        // Drop bookkeeping for panes no longer present this tick.
        let live: HashSet<&str> = seqs.iter().map(|(id, _, _)| id.as_str()).collect();
        self.pane_seen.retain(|id, _| live.contains(id.as_str()));
        self.pane_pending.retain(|id| live.contains(id.as_str()));
        self.pane_active_until
            .retain(|id, _| live.contains(id.as_str()));
        self.last_output_at.retain(|id, _| live.contains(id.as_str()));
        self.waiting_response = self
            .pane_active_until
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(id, _)| id.clone())
            .collect();
    }

    /// Fold the shell-tab foreground-process signal into `waiting_response`,
    /// after [`Self::apply_pane_activity`] has rebuilt it from output deltas.
    /// `Some(true)` (a foreground command is running) marks the shell busy only
    /// if it also produced output within `SHELL_BUSY_IDLE` — a quiet foreground
    /// process (an open editor, a pager, a shell parked in `less`) is you sitting
    /// there, not work, so it decays to idle like any other silent pane.
    /// `Some(false)` clears it, and `None` ("can't tell") leaves the output-based
    /// result untouched. Split out from [`Self::update_pane_activity`] so the
    /// override is unit-testable without a live PTY. Only touches
    /// `waiting_response` — never `attention`, so a busy shell spins without
    /// raising a "needs you" notification.
    fn apply_shell_busy(&mut self, now: Instant, shell_busy: &[(String, Option<bool>)]) {
        // 3s bridges bursty build/test output between lines; bump if a slow step
        // (linker, quiet compile) flickers idle mid-run.
        const SHELL_BUSY_IDLE: Duration = Duration::from_secs(3);
        for (id, busy) in shell_busy {
            match busy {
                Some(true) => {
                    let fresh = self
                        .last_output_at
                        .get(id)
                        .is_some_and(|t| now.duration_since(*t) < SHELL_BUSY_IDLE);
                    if fresh {
                        self.waiting_response.insert(id.clone());
                    } else {
                        // A running-but-quiet foreground command (an open editor
                        // or pager) — not work. Clear it; output-based activity,
                        // if any, has lapsed at this age too.
                        self.waiting_response.remove(id);
                    }
                }
                Some(false) => {
                    self.waiting_response.remove(id);
                }
                None => {}
            }
        }
    }

    /// Get the selected workspace, if any
    pub(crate) fn selected_workspace(&self) -> Option<&Workspace> {
        match self.tree_items.get(self.selected_index) {
            Some(TreeNode::Workspace { ws, .. }) => Some(ws),
            _ => None,
        }
    }
}

/// Outcome of handling a single key event.
#[derive(Debug, PartialEq)]
enum KeyOutcome {
    Continue,
    Quit,
}

/// Append pasted text to the tree filter query (sanitized for a single line via
/// [`modal::sanitize_paste`]), then re-apply the filter. Extracted from the
/// event loop so it's unit-testable and consistent with the modal/palette paste
/// sinks. A paste that sanitizes to nothing is a no-op — the query and the
/// current selection are left untouched (no needless re-rank).
fn handle_filter_paste(app: &mut App, text: &str) {
    let clean = modal::sanitize_paste(text);
    if clean.is_empty() {
        return;
    }
    app.filter_query.push_str(&clean);
    app.apply_filter();
}

/// Handle one key press. Extracted from the main event loop so tests can
/// drive the app without a real terminal.
async fn handle_key(app: &mut App, key: KeyEvent) -> anyhow::Result<KeyOutcome> {
    // Any key dismisses a pane selection highlight (it may be about to change
    // what the pane shows; the copy already happened on mouse-up).
    app.pane_selection = None;
    // Embedded pane owns the keyboard: every key forwards to the real claude
    // (incl. Ctrl+C, Tab, q, slash commands). kommand0 commands are reached via a
    // tmux-style prefix (Ctrl+A) so there's always a reliable way out:
    //   Ctrl+A then  q = quit · t/Tab/Esc = back to tree · Ctrl+A = literal Ctrl+A
    // A modal (e.g. Rename Session) opens over the embedded pane without leaving
    // Embedded focus, so it must intercept keys before this block forwards them
    // to claude. The modal block below (and the paste/mouse paths) is gated the
    // same way.
    if app.focus == Focus::Embedded && !app.modal.is_active() {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if app.embedded_prefix {
            app.embedded_prefix = false;
            // The `!ctrl` guards keep `Ctrl+]` (decoded as Char(']') or Char('5')
            // with CTRL) from being read as a tab command after the prefix.
            match key.code {
                KeyCode::Char('q') => {
                    return Ok(KeyOutcome::Quit);
                }
                KeyCode::Char('t') if !ctrl => {
                    app.focus = Focus::Tree;
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Tab | KeyCode::Esc => {
                    app.focus = Focus::Tree;
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('c') if !ctrl => {
                    if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone()) {
                        app.new_tab(TabKind::Claude, &ws_id);
                    }
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('s') if !ctrl => {
                    // New shell tab ($SHELL / configured shell).
                    if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone()) {
                        app.new_tab(TabKind::Shell, &ws_id);
                    }
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('x') if !ctrl => {
                    app.close_active_session();
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('d') if !ctrl => {
                    app.detach_selected_workspace();
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('r') if !ctrl => {
                    app.open_rename_active_session();
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('[') if !ctrl => {
                    if let Some(s) = app.selected_sessions_mut() {
                        s.prev();
                    }
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char(']') if !ctrl => {
                    if let Some(s) = app.selected_sessions_mut() {
                        s.next();
                    }
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char(c @ '1'..='9') if !ctrl => {
                    let idx = (c as u8 - b'1') as usize;
                    if let Some(s) = app.selected_sessions_mut() {
                        s.select(idx);
                    }
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('l') if !ctrl => {
                    // Last-active tab (tmux prefix-l); no-op with no history.
                    if let Some(s) = app.selected_sessions_mut() {
                        s.select_last_active();
                    }
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('a') if ctrl => {
                    app.forward_to_embedded(key); // literal Ctrl+A to claude
                    return Ok(KeyOutcome::Continue);
                }
                _ => return Ok(KeyOutcome::Continue), // unknown command: swallow
            }
        }
        if ctrl && key.code == KeyCode::Char('a') {
            app.embedded_prefix = true; // start a prefix sequence
            return Ok(KeyOutcome::Continue);
        }
        // Direct leave alias: Ctrl+] (Kitty CSI-u reports Char(']')+CTRL, a legacy
        // terminal reports Char('5')+CTRL since both are byte 0x1d).
        if ctrl && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5')) {
            app.focus = Focus::Tree;
            return Ok(KeyOutcome::Continue);
        }
        if !app.forward_to_embedded(key) {
            app.focus = Focus::Tree; // pane vanished — bail to the tree
        }
        return Ok(KeyOutcome::Continue);
    }

    // Help modal: scrollable, dismissed with ?/Esc, swallows other keys
    if app.show_help {
        let g_was_pending = std::mem::take(&mut app.pending_g);
        match key.code {
            KeyCode::Char('?') | KeyCode::Esc => app.show_help = false,
            KeyCode::Down | KeyCode::Char('j') => {
                app.help_scroll = app.help_scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.help_scroll = app.help_scroll.saturating_sub(1);
            }
            KeyCode::PageDown => {
                app.help_scroll = app.help_scroll.saturating_add(10);
            }
            KeyCode::PageUp => {
                app.help_scroll = app.help_scroll.saturating_sub(10);
            }
            KeyCode::Char('g') => {
                if g_was_pending {
                    app.help_scroll = 0;
                } else {
                    app.pending_g = true;
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                // Clamped to actual content height at render time
                app.help_scroll = u16::MAX;
            }
            KeyCode::Home => app.help_scroll = 0,
            _ => {} // swallow all other keys
        }
        return Ok(KeyOutcome::Continue);
    }

    // Settings page: full-screen config editor. Browse mode navigates rows;
    // edit mode types into a LineEdit and commits on Enter. Swallows all keys.
    if app.settings.is_some() {
        std::mem::take(&mut app.pending_g); // abandon a half-typed `gg`
        let editing = app.settings.as_ref().is_some_and(|s| s.edit.is_some());
        if editing {
            match key.code {
                KeyCode::Esc => {
                    if let Some(s) = app.settings.as_mut() {
                        s.edit = None;
                        s.error = None;
                    }
                }
                KeyCode::Enter => {
                    // Two-phase: read the pending edit, then commit (needs
                    // `&mut self`), then write the outcome back into the page.
                    let pending = app
                        .settings
                        .as_ref()
                        .and_then(|s| s.edit.as_ref().map(|e| (s.field(), e.buf.clone())));
                    if let Some((field, raw)) = pending {
                        let result = app.commit_setting(field, &raw);
                        if let Some(s) = app.settings.as_mut() {
                            match result {
                                Ok(()) => {
                                    s.edit = None;
                                    s.error = None;
                                }
                                // Keep the edit open so the input can be fixed.
                                Err(e) => s.error = Some(e),
                            }
                        }
                    }
                }
                _ => {
                    if let Some(edit) = app.settings.as_mut().and_then(|s| s.edit.as_mut()) {
                        match key.code {
                            KeyCode::Char(c)
                                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                edit.insert_char(c);
                            }
                            KeyCode::Backspace => edit.backspace(),
                            KeyCode::Left => edit.left(),
                            KeyCode::Right => edit.right(),
                            KeyCode::Home => edit.home(),
                            KeyCode::End => edit.end(),
                            _ => {} // swallow all other keys
                        }
                    }
                }
            }
        } else {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => app.settings = None,
                // The open binding also closes (`,` by default — respects rebinds).
                _ if app.keymap.resolve(&key) == Some(keymap::Action::OpenSettings) => {
                    app.settings = None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(s) = app.settings.as_mut() {
                        s.move_down();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(s) = app.settings.as_mut() {
                        s.move_up();
                    }
                }
                KeyCode::Enter => {
                    if let Some(s) = app.settings.as_mut() {
                        s.error = None;
                        s.edit = Some(modal::LineEdit::new(s.field().current(&app.config)));
                    }
                }
                _ => {} // swallow all other keys
            }
        }
        return Ok(KeyOutcome::Continue);
    }

    // Review-diff dialog: two panes (file tree + diff), dismissed with v/Esc/q,
    // swallows the rest. Tab switches focus; keys act on the focused pane.
    if app.show_diff {
        let g_was_pending = std::mem::take(&mut app.pending_g);
        match key.code {
            // Closing abandons any half-typed `gg` so it can't complete in the
            // tree (the flag is shared). (`mem::take` above already cleared it;
            // stated explicitly so the invariant survives that line changing.)
            KeyCode::Char('v') | KeyCode::Char('q') | KeyCode::Esc => {
                app.show_diff = false;
                app.pending_g = false;
            }
            KeyCode::Tab => app.diff_toggle_focus(),
            _ if app.diff_focus == DiffFocus::Files => match key.code {
                KeyCode::Down | KeyCode::Char('j') => app.diff_select_move(1),
                KeyCode::Up | KeyCode::Char('k') => app.diff_select_move(-1),
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => app.diff_expand_or_enter(),
                KeyCode::Left | KeyCode::Char('h') => app.diff_collapse_or_parent(),
                KeyCode::Char('g') => {
                    if g_was_pending {
                        app.diff_select_first();
                    } else {
                        app.pending_g = true;
                    }
                }
                KeyCode::Char('G') => app.diff_select_last(),
                _ => {} // swallow all other keys
            },
            // Focus == Diff: scroll the right pane (clamped at render time).
            KeyCode::Down | KeyCode::Char('j') => {
                app.diff_scroll = app.diff_scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.diff_scroll = app.diff_scroll.saturating_sub(1);
            }
            KeyCode::PageDown => app.diff_scroll = app.diff_scroll.saturating_add(10),
            KeyCode::PageUp => app.diff_scroll = app.diff_scroll.saturating_sub(10),
            KeyCode::Char('g') => {
                if g_was_pending {
                    app.diff_scroll = 0;
                } else {
                    app.pending_g = true;
                }
            }
            KeyCode::Char('G') | KeyCode::End => app.diff_scroll = u16::MAX,
            KeyCode::Home => app.diff_scroll = 0,
            _ => {} // swallow all other keys
        }
        return Ok(KeyOutcome::Continue);
    }

    // Modal dialog: swallow all keys
    if app.modal.is_active() {
        match modal::handle_modal_key(&mut app.modal, key) {
            modal::ModalResult::Consumed | modal::ModalResult::Cancelled => {}
            modal::ModalResult::SubmitRepo(path) => match app.state.add_repo(&path) {
                Ok(repo) => {
                    // Expand the just-added repo so its "(no workspaces — press w)"
                    // hint shows immediately, pointing the user at the next step.
                    app.expanded.insert(repo.id.clone());
                    app.repos = app.state.repos.clone();
                    app.rebuild_tree();
                }
                Err(e) => {
                    app.modal = modal::ModalState::AddRepo {
                        input: path,
                        cursor: 0,
                        error: Some(e.to_string()),
                        completions: Vec::new(),
                        completion_index: None,
                    };
                }
            },
            modal::ModalResult::ConfirmDelete(target) => {
                match target {
                    modal::DeleteTarget::Workspace { id, .. } => {
                        if let Some(sid) = app
                            .state
                            .find_session_by_workspace(&id)
                            .filter(|s| s.status == SessionStatus::Running)
                            .map(|s| s.id.clone())
                        {
                            let _ = app
                                .state
                                .update_session_status(&sid, SessionStatus::Stopped);
                        }
                        // Tear the embedded pane down here (at user-action
                        // time) rather than letting reap_embedded block the
                        // 50ms tick on the pane's Drop.
                        app.embedded.remove(&id);
                        let _ = app.state.delete_workspace_by_id(&id);
                        app.workspaces = app.state.workspaces.clone();
                        app.rebuild_tree();
                        app.update_active_session();
                    }
                    modal::DeleteTarget::Repo { id, .. } => {
                        // Stop all running sessions for this repo's workspaces
                        let ws_ids: Vec<String> = app
                            .state
                            .workspaces
                            .iter()
                            .filter(|w| w.repo_id == id)
                            .map(|w| w.id.clone())
                            .collect();
                        for ws_id in &ws_ids {
                            if let Some(s) = app.state.find_session_by_workspace(ws_id)
                                && s.status == SessionStatus::Running
                            {
                                let sid = s.id.clone();
                                let _ = app
                                    .state
                                    .update_session_status(&sid, SessionStatus::Stopped);
                            }
                            app.embedded.remove(ws_id);
                        }
                        let _ = app.state.delete_repo(&id);
                        app.repos = app.state.repos.clone();
                        app.workspaces = app.state.workspaces.clone();
                        app.expanded.remove(&id);
                        app.rebuild_tree();
                        app.update_active_session();
                    }
                }
            }
            modal::ModalResult::SubmitWorkspace(repo_id, name, branch) => {
                // Look the repo up once. `repo_id` (not name) is the create ref:
                // resolve_repo matches name before id, so a duplicate basename
                // could target the wrong repo. `repo.name` is for the modal text,
                // `repo.path` for the branch detector.
                let repo = app.repos.iter().find(|r| r.id == repo_id).cloned();
                if !branch.is_empty() {
                    // An explicitly-filled Branch field checks out that branch as
                    // today — the detect-and-offer flow is only for a blank one.
                    // A blank name means derive a path-safe one from the branch.
                    let repo_name = repo.map(|r| r.name).unwrap_or_default();
                    let result = app.state.create_workspace_from_branch(
                        (!name.is_empty()).then_some(name.as_str()),
                        &repo_id,
                        &branch,
                    );
                    app.finish_add_workspace(result, repo_id, repo_name, name, branch);
                } else if let Some(repo) = repo {
                    // A name with a '/' can only mean an existing branch (workspace
                    // names ban path separators): check it out directly, the
                    // workspace name derived by core. A missing branch surfaces
                    // core's error, with the typed name preserved for a retry.
                    if name.contains('/') {
                        let result =
                            app.state.create_workspace_from_branch(None, &repo_id, &name);
                        app.finish_add_workspace(result, repo_id, repo.name, name, String::new());
                    }
                    // Blank branch: offer the checkout only for a valid, unused
                    // name whose bare branch already exists — otherwise fall
                    // through to create, which surfaces core's canonical error
                    // (a duplicate or invalid name never opens the offer).
                    else if app.state.validate_new_workspace_name(&name, &repo_id).is_ok()
                        && kommand0_core::worktree::branch_exists_bare(&repo.path, &name)
                    {
                        app.modal = modal::ModalState::ConfirmBranchCheckout {
                            repo_id,
                            repo_name: repo.name,
                            name,
                        };
                    } else {
                        let result = app.state.create_workspace(Some(&name), &repo_id);
                        app.finish_add_workspace(result, repo_id, repo.name, name, String::new());
                    }
                } else {
                    // Repo not found: fall through to create so its resolve error
                    // surfaces (don't call the detector with an empty path).
                    let result = app.state.create_workspace(Some(&name), &repo_id);
                    app.finish_add_workspace(result, repo_id, String::new(), name, String::new());
                }
            }
            modal::ModalResult::BranchCheckoutChoice { repo_id, name, checkout } => {
                // Route directly — never back through SubmitWorkspace, or a fork
                // would re-trigger detection and loop. repo_name is only needed
                // for the Err-reopen text.
                let repo_name = app
                    .repos
                    .iter()
                    .find(|r| r.id == repo_id)
                    .map(|r| r.name.clone())
                    .unwrap_or_default();
                let result = if checkout {
                    app.state.create_workspace_from_branch(Some(&name), &repo_id, &name)
                } else {
                    app.state.create_workspace(Some(&name), &repo_id)
                };
                app.finish_add_workspace(result, repo_id, repo_name, name, String::new());
            }
            modal::ModalResult::SubmitRename(ws_id, session_id, title) => {
                // Only title a session that's still live — reap can heal/drop the
                // tab (assigning a new id) while the modal is open, and a title
                // must never outlive its session. Persist immediately (the quit
                // path does not unconditionally save). Focus stays Embedded, so
                // closing the modal drops the user straight back into the pane.
                if app
                    .state
                    .embedded_session_ids(&ws_id)
                    .iter()
                    .any(|id| id == &session_id)
                {
                    app.state.set_embedded_session_title(&ws_id, &session_id, &title);
                    app.save_state();
                }
            }
            modal::ModalResult::ConfirmCleanup(ws_id) => {
                app.start_cleanup(&ws_id);
            }
        }
        return Ok(KeyOutcome::Continue);
    }

    // Command palette ("go to workspace"): a flat fuzzy jump overlay. While open
    // it captures every key — Enter jumps to the selection, Esc cancels. The
    // borrow of `app.palette` is scoped so we can mutate `app` after it closes.
    if app.palette.is_some() {
        let mut close = false;
        let mut action: Option<palette::PaletteAction> = None;
        {
            let p = app.palette.as_mut().unwrap();
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => close = true,
                KeyCode::Enter => {
                    action = p.selected_action().cloned();
                    close = true;
                }
                KeyCode::Up => p.move_up(),
                KeyCode::Down => p.move_down(),
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => p.move_up(),
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => p.move_down(),
                KeyCode::Backspace => p.pop_char(),
                KeyCode::Char(c) => p.push_char(c),
                _ => {}
            }
        }
        if close {
            app.palette = None;
        }
        if let Some(a) = action {
            app.dispatch_palette_action(a);
        }
        return Ok(KeyOutcome::Continue);
    }

    // Tree filter input: while typing a `/` filter, swallow every key so it
    // edits the query rather than running a tree/global command (incl. q/?).
    if app.filter_input {
        match key.code {
            KeyCode::Esc => {
                app.filter_query.clear();
                app.filter_input = false;
                app.apply_filter();
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.filter_query.clear();
                app.filter_input = false;
                app.apply_filter();
            }
            KeyCode::Enter => app.filter_input = false, // keep the query applied
            // Arrows walk the live matches without leaving the filter box.
            KeyCode::Up => app.move_up(),
            KeyCode::Down => app.move_down(),
            KeyCode::Backspace => {
                app.filter_query.pop();
                app.apply_filter();
            }
            KeyCode::Char(ch) => {
                app.filter_query.push(ch);
                app.apply_filter();
            }
            _ => {}
        }
        return Ok(KeyOutcome::Continue);
    }

    // Vim `gg`: consume the pending flag; only a bare `g` below re-arms it.
    let g_was_pending = std::mem::take(&mut app.pending_g);

    // Tree-pane keys. (Embedded focus is intercepted at the top of handle_key.)
    if app.focus == Focus::Tree {
        // Fixed (non-rebindable) keys: the `gg` motion and Esc-clears-filter.
        match key.code {
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                if g_was_pending {
                    app.tree_select_first();
                } else {
                    app.pending_g = true;
                }
                return Ok(KeyOutcome::Continue);
            }
            KeyCode::Esc => {
                if !app.filter_query.is_empty() {
                    app.filter_query.clear();
                    app.apply_filter();
                }
                return Ok(KeyOutcome::Continue);
            }
            _ => {}
        }

        // Everything else dispatches through the (rebindable) keymap.
        if let Some(action) = app.keymap.resolve(&key) {
            use keymap::Action;
            match action {
                Action::Quit => {
                    let running_ids: Vec<String> = app
                        .state
                        .sessions
                        .iter()
                        .filter(|s| s.status == SessionStatus::Running)
                        .map(|s| s.id.clone())
                        .collect();
                    for sid in running_ids {
                        let _ = app
                            .state
                            .update_session_status(&sid, SessionStatus::Stopped);
                    }
                    return Ok(KeyOutcome::Quit);
                }
                Action::Help => {
                    app.show_help = !app.show_help;
                    app.help_scroll = 0;
                }
                Action::OpenSettings => {
                    app.settings = Some(settings::SettingsState::default());
                }
                Action::MoveUp => app.move_up(),
                Action::MoveDown => app.move_down(),
                Action::CollapseOrParent => app.tree_collapse_or_parent(),
                Action::StepInto => app.tree_expand_or_enter(),
                Action::SelectLast => app.tree_select_last(),
                Action::PrevRepo => app.jump_repo(false),
                Action::NextRepo => app.jump_repo(true),
                Action::WidenTree => app.widen_tree(),
                Action::ShrinkTree => app.shrink_tree(),
                Action::OpenSession => app.toggle_embedded(),
                Action::ActivateSelection => {
                    // Enter on a repo expands it; on a workspace it opens the
                    // embedded interactive claude (the default session experience).
                    match app.tree_items.get(app.selected_index) {
                        Some(TreeNode::Repo { .. }) => app.toggle_expand(),
                        Some(TreeNode::Workspace { .. }) => app.toggle_embedded(),
                        Some(TreeNode::Hint { .. }) | None => {}
                    }
                }
                Action::CloseSession => {
                    // Same teardown as the embedded `Ctrl+A d`: kill the panes,
                    // keep the persisted sessions (the focus flip is a no-op
                    // here — action dispatch only runs in tree focus).
                    app.detach_selected_workspace();
                }
                Action::ReviewDiff => {
                    if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone()) {
                        app.open_diff(&ws_id);
                    }
                }
                Action::OpenPrInBrowser => {
                    // Open the selected workspace's PR in the browser, if one is
                    // cached with a URL; otherwise no-op (no PR to open).
                    if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone())
                        && let Some(url) =
                            app.pr_status.get(&ws_id).map(|p| p.url.clone()).filter(|u| !u.is_empty())
                    {
                        open_url(&url);
                    }
                }
                Action::Cleanup => {
                    if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone()) {
                        app.cleanup_workspace_prompt(&ws_id);
                    }
                }
                Action::Filter => {
                    // Enter the tree filter; keep any existing query to edit.
                    app.filter_input = true;
                }
                Action::Palette => {
                    // Open the "go to workspace" jump palette over a snapshot of
                    // every workspace.
                    let candidates = app.palette_candidates();
                    app.palette = Some(palette::Palette::new(candidates));
                }
                Action::NextWaiting => app.jump_to_waiting(true),
                Action::PrevWaiting => app.jump_to_waiting(false),
                Action::ArchiveToggle => {
                    if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone()) {
                        app.archive_toggle(&ws_id);
                    }
                }
                Action::AddRepo => {
                    app.modal = modal::ModalState::AddRepo {
                        input: String::new(),
                        cursor: 0,
                        error: None,
                        completions: Vec::new(),
                        completion_index: None,
                    };
                }
                Action::AddWorkspace => {
                    let repo_info = match app.tree_items.get(app.selected_index) {
                        Some(TreeNode::Repo { id, name, .. }) => Some((id.clone(), name.clone())),
                        Some(TreeNode::Workspace { ws, repo_name }) => {
                            Some((ws.repo_id.clone(), repo_name.clone()))
                        }
                        _ => None,
                    };
                    if let Some((repo_id, repo_name)) = repo_info {
                        app.modal = modal::ModalState::AddWorkspace {
                            repo_id,
                            repo_name,
                            input: String::new(),
                            cursor: 0,
                            branch: String::new(),
                            branch_cursor: 0,
                            field: modal::AddWorkspaceField::Name,
                            error: None,
                        };
                    }
                }
                Action::Delete => match app.tree_items.get(app.selected_index).cloned() {
                    Some(TreeNode::Workspace { ws, repo_name }) => {
                        app.modal = modal::ModalState::ConfirmDelete {
                            target: modal::DeleteTarget::Workspace {
                                id: ws.id,
                                name: ws.name,
                                repo: repo_name,
                            },
                        };
                    }
                    Some(TreeNode::Repo { id, name, .. }) => {
                        let ws_count =
                            app.workspaces.iter().filter(|w| w.repo_id == id).count();
                        app.modal = modal::ModalState::ConfirmDelete {
                            target: modal::DeleteTarget::Repo {
                                id,
                                name,
                                workspace_count: ws_count,
                            },
                        };
                    }
                    _ => {}
                },
                Action::ForceDelete => match app.tree_items.get(app.selected_index).cloned() {
                    Some(TreeNode::Workspace { ws, .. }) => {
                        // The tree node carries the workspace: key everything
                        // on its id (a by-name refetch could hit another
                        // repo's same-named workspace and its live session).
                        if let Some(sid) = app
                            .state
                            .find_session_by_workspace(&ws.id)
                            .filter(|s| s.status == SessionStatus::Running)
                            .map(|s| s.id.clone())
                        {
                            let _ = app
                                .state
                                .update_session_status(&sid, SessionStatus::Stopped);
                        }
                        app.embedded.remove(&ws.id);
                        let _ = app.state.delete_workspace_by_id(&ws.id);
                        app.workspaces = app.state.workspaces.clone();
                        app.rebuild_tree();
                        app.update_active_session();
                    }
                    Some(TreeNode::Repo { id, .. }) => {
                        let ws_ids: Vec<String> = app
                            .state
                            .workspaces
                            .iter()
                            .filter(|w| w.repo_id == id)
                            .map(|w| w.id.clone())
                            .collect();
                        for ws_id in &ws_ids {
                            if let Some(s) = app.state.find_session_by_workspace(ws_id)
                                && s.status == SessionStatus::Running
                            {
                                let sid = s.id.clone();
                                let _ = app
                                    .state
                                    .update_session_status(&sid, SessionStatus::Stopped);
                            }
                            app.embedded.remove(ws_id);
                        }
                        let _ = app.state.delete_repo(&id);
                        app.repos = app.state.repos.clone();
                        app.workspaces = app.state.workspaces.clone();
                        app.expanded.remove(&id);
                        app.rebuild_tree();
                        app.update_active_session();
                    }
                    _ => {}
                },
            }
        }
    }
    Ok(KeyOutcome::Continue)
}

/// Route warnings/errors to `<state_dir>/kommand0.log` — they can't go to stderr
/// while the TUI owns the terminal (alt-screen). Best-effort: if the file can't
/// be opened, the app still starts (tracing calls just become no-ops).
fn init_logging() {
    let dir = AppState::state_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("kommand0.log"))
    {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_target(false)
            .try_init();
    }
}

/// Handle the few non-launching args (`--version`, `--help`) before the TUI
/// takes over the terminal. Returns the text to print, or `None` to launch.
/// (The TUI takes no other arguments — anything else just launches it.)
fn cli_short_circuit(args: &[String]) -> Option<String> {
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Some(format!("kommand0 {}", env!("CARGO_PKG_VERSION")));
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Some(format!(
            "kommand0 {} — keyboard-first orchestrator for parallel Claude Code sessions\n\n\
             Usage: kommand0                    launch the TUI\n\
             \x20      kommand0 --profile <name>   run an isolated profile (own state, config, log, sessions, worktrees)\n\
             \x20      kommand0 --version          print version\n\n\
             Manage repos/workspaces from the CLI with `kmd` (see the README).",
            env!("CARGO_PKG_VERSION")
        ));
    }
    None
}

/// First `--profile <name>` / `--profile=<name>` anywhere in the args (later
/// duplicates are ignored — first wins; `kmd`/clap errors on duplicates
/// instead, a deliberate divergence). The space form takes the next arg
/// verbatim. `Err(message)` on a missing or empty value.
fn parse_profile_arg(args: &[String]) -> Result<Option<String>, String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--profile" {
            return match it.next() {
                Some(v) => Ok(Some(v.clone())),
                None => Err("--profile requires a value".to_string()),
            };
        }
        if let Some(v) = arg.strip_prefix("--profile=") {
            if v.is_empty() {
                return Err("--profile requires a value".to_string());
            }
            return Ok(Some(v.to_string()));
        }
    }
    Ok(None)
}

/// Keyboard-enhancement flags requested when the terminal supports the Kitty
/// protocol. `REPORT_ALL_KEYS_AS_ESCAPE_CODES` routes even plain keys through
/// the CSI-u path; `REPORT_ALTERNATE_KEYS` must accompany it so the terminal
/// also reports the *shifted* codepoint. Without it `?` (Shift+/) arrives as
/// `Char('/')` + SHIFT, and `normalize()` drops SHIFT — collapsing `?`→`/`
/// (and `:`→`;`, `<`→`,`, `>`→`.`, uppercase letters, …), so help opened the
/// filter instead and the embedded pane typed `/` for `?`.
fn keyboard_enhancement_flags() -> crossterm::event::KeyboardEnhancementFlags {
    use crossterm::event::KeyboardEnhancementFlags as Flags;
    Flags::DISAMBIGUATE_ESCAPE_CODES | Flags::REPORT_ALL_KEYS_AS_ESCAPE_CODES | Flags::REPORT_ALTERNATE_KEYS
}

/// modifyOtherKeys mode 1 request / reset (`CSI >4;1m` / `CSI >4;0m`) — the
/// fallback for tmux, which never passes the kitty protocol through but,
/// with `extended-keys on` + `extended-keys-format csi-u`, delivers
/// Shift+Enter to a requesting pane app as `CSI 13;2u` (which crossterm
/// parses as Shift+Enter). Probed on tmux 3.7b: mode 1 suffices and leaves
/// ordinary keys (Ctrl+C, Tab, Ctrl+arrows, plain Enter) on their classic
/// encodings — mode 2 would rewrite far more keys for no extra gain here.
/// Both the explicit `>4;0m` and bare `>4m` reset there; the explicit form
/// is used.
const MODIFY_OTHER_KEYS_REQUEST: &str = "\x1b[>4;1m";
const MODIFY_OTHER_KEYS_RESET: &str = "\x1b[>4;0m";

/// One tmux server option (`tmux show -sv <name>`), raw stdout. Panic-free
/// and bounded (~2s, the run_gh timeout shape) so a wedged tmux server can't
/// hang startup. `None` when tmux is missing, errors (an older tmux without
/// the option), times out, or prints nothing.
fn tmux_option(name: &str) -> Option<String> {
    let child = std::process::Command::new("tmux")
        .args(["show", "-sv", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = rx.recv_timeout(Duration::from_secs(2)).ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// Whether tmux will deliver modifyOtherKeys in the CSI u form crossterm
/// parses: `extended-keys` on/always AND `extended-keys-format` csi-u.
/// Takes raw `tmux show -sv` output (a trailing newline is fine). tmux 3.7
/// defaults are off/xterm — BOTH options must be configured — and an older
/// tmux where the query fails (`None`) counts as not delivering: with the
/// xterm format tmux answers `CSI 27;2;13~`, which crossterm cannot parse.
fn tmux_delivers_csi_u(extended_keys: Option<&str>, format: Option<&str>) -> bool {
    matches!(extended_keys.map(str::trim), Some("on") | Some("always"))
        && format.map(str::trim) == Some("csi-u")
}

/// Print a startup error and exit. Only for failures before the alt-screen
/// starts — stderr still reaches the terminal there.
fn die(msg: &str) -> ! {
    eprintln!("kommand0: {msg}");
    std::process::exit(1);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Answer `--version`/`--help` before entering the alt-screen, where stdout
    // would be swallowed. (Kept first so `kommand0 --profile --help` prints
    // help and exits rather than treating `--help` as a profile name.)
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(msg) = cli_short_circuit(&args) {
        println!("{msg}");
        return Ok(());
    }
    // Profile errors (bad value, KOMMAND0_STATE_DIR conflict) exit here, while
    // stderr still reaches the terminal — the alt-screen starts below.
    let profile = match parse_profile_arg(&args) {
        Ok(p) => p,
        Err(e) => die(&e),
    };
    // --profile, else an inherited KOMMAND0_PROFILE (a profiled parent TUI
    // exports it to embedded sessions) — the effective name drives the label.
    let profile = match AppState::init_profile(profile.as_deref()) {
        Ok(name) => name,
        Err(e) => die(&e),
    };
    // Must run BEFORE init_logging(): that create_dir_all's the state dir,
    // which would create profiles/… first and trip the migration guard —
    // reordering this after init_logging silently orphans pre-profiles state.
    if let Err(e) = AppState::migrate_legacy_profiles() {
        die(&e.to_string());
    }
    init_logging();
    tracing::info!("kommand0 started");
    let mut terminal = ratatui::init();
    // DISAMBIGUATE_ESCAPE_CODES lets terminals report Shift+Enter distinctly from Enter
    let supports_enhanced_keys =
        crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    // tmux swallows the kitty protocol (the probe above returns false there),
    // but when its extended-keys options are set it delivers modified keys in
    // the CSI u form to a pane app that requests modifyOtherKeys — probe the
    // options now (cheap, read-only `tmux show`); the request itself is
    // written after the rest of the terminal setup, and reset in every
    // teardown path (normal exit + the panic hook).
    let tmux = std::env::var_os("TMUX").is_some();
    let tmux_extended_keys = tmux
        && !supports_enhanced_keys
        && tmux_delivers_csi_u(
            tmux_option("extended-keys").as_deref(),
            tmux_option("extended-keys-format").as_deref(),
        );
    // The hint only fires when tmux is NOT configured for CSI u delivery —
    // when it is, Shift+Enter just works and there is nothing to hint.
    let tmux_newline_hint = tmux && !supports_enhanced_keys && !tmux_extended_keys;
    // Panic safety net: we've entered raw mode + the alt-screen. `ratatui::init`
    // already installed a hook that restores the terminal on panic, but it does
    // NOT pop the keyboard-enhancement flags or disable mouse/bracketed-paste.
    // Wrap it: do our extra cleanup first, then chain ratatui's hook
    // (`prev_hook`), which leaves raw mode + the alt-screen and prints the
    // backtrace into the restored terminal. Without this, a panic in
    // draw/handle_key/tick would unwind past the teardown below and strand the
    // user on a scrambled screen.
    {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let mut out = std::io::stdout();
            if supports_enhanced_keys {
                let _ = crossterm::execute!(out, crossterm::event::PopKeyboardEnhancementFlags);
            }
            if tmux_extended_keys {
                let _ = crossterm::execute!(out, crossterm::style::Print(MODIFY_OTHER_KEYS_RESET));
            }
            let _ = crossterm::execute!(out, crossterm::event::DisableFocusChange);
            let _ = crossterm::execute!(out, crossterm::event::DisableBracketedPaste);
            let _ = crossterm::execute!(out, crossterm::event::DisableMouseCapture);
            prev_hook(info);
        }));
    }
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)?;
    // Focus reporting (mode 1004): FocusGained/FocusLost feed the composite
    // focus synthesized for embedded children (App::sync_focus_states).
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableFocusChange)?;
    if supports_enhanced_keys {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        );
    }
    if tmux_extended_keys {
        // Shift+Enter then arrives as `CSI 13;2u` → crossterm parses
        // Shift+Enter → the pane's existing mapping sends \n.
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::style::Print(MODIFY_OTHER_KEYS_REQUEST)
        );
    }
    let result = run(&mut terminal, profile, tmux_newline_hint).await;
    if supports_enhanced_keys {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
    if tmux_extended_keys {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::style::Print(MODIFY_OTHER_KEYS_RESET)
        );
    }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableFocusChange);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

async fn run(
    terminal: &mut DefaultTerminal,
    profile: String,
    tmux_newline_hint: bool,
) -> anyhow::Result<()> {
    // A corrupt state.json degrades to default (backed up) + a warning, rather
    // than aborting startup.
    let (state, state_warning) = AppState::load_checked()?;
    let mut app = App::new(state);
    // Surface the (effective) profile in the tree title — hidden for the
    // default profile, so `--profile default` looks exactly like no flag.
    app.profile_label = (profile != DEFAULT_PROFILE).then_some(profile);
    // Load user config now (App::new keeps a hermetic default for tests). A
    // present-but-invalid file (or a bad keybinding) surfaces a warning in the
    // tree border, with full detail in the log.
    let (config, config_warning) = Config::load_checked();
    app.config = config;
    app.tree_width_pct = seed_tree_width(app.config.tree_width_pct);
    let (keymap, key_warnings) = keymap::KeyMap::build(&app.config.keybindings);
    app.keymap = keymap;
    let (theme, theme_warnings) =
        theme::Theme::build(app.config.theme.as_deref(), &app.config.theme_colors);
    app.theme = theme;
    let (notify_mode, notify_warning) = notify::NotifyMode::parse(app.config.notify.as_deref());
    app.notify_mode = notify_mode;
    let mut warnings: Vec<String> = state_warning.into_iter().chain(config_warning).collect();
    warnings.extend(key_warnings);
    warnings.extend(theme_warnings);
    warnings.extend(notify_warning);
    for w in &warnings {
        tracing::warn!("config: {w}");
    }
    // Not a config problem, but the tree border + log is the only passive
    // startup notify surface there is.
    if tmux_newline_hint {
        let hint = "tmux: for Shift+Enter newlines add 'set -s extended-keys on' and \
                    'set -s extended-keys-format csi-u' to tmux.conf, then restart tmux — \
                    or use Alt+Enter";
        tracing::info!("{hint}");
        warnings.push(hint.to_string());
    }
    app.config_warning = match warnings.len() {
        0 => None,
        1 => Some(warnings.remove(0)),
        n => Some(format!("{n} config issues — see kommand0.log")),
    };

    // A persisted `Running` is stale (no stream session is resurrected) — heal it.
    if app.normalize_stale_running() > 0 {
        app.save_state();
    }

    // Drop persisted Claude session ids for workspaces that no longer exist.
    let before = app.state.embedded_sessions.len();
    app.state.prune_embedded_sessions();
    if app.state.embedded_sessions.len() != before {
        app.save_state();
    }

    // Terminal input is read on a dedicated blocking thread rather than via
    // crossterm's async `EventStream`: that stream wedges after a rapid SIGWINCH
    // burst (its internal wake task stops delivering events, so keystrokes are
    // never seen again — see the `survives_a_resize_drag_without_wedging_input`
    // e2e test). A plain blocking `event::read()` loop has no such machinery. The thread
    // forwards every event over a channel into the select! loop below, and exits
    // when the receiver is dropped (app shutdown).
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        // `read()` erroring ends the `while let` (thread exits); a send error
        // means the receiver was dropped (app shutting down) so we stop too.
        while let Ok(ev) = crossterm::event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });
    let mut tick_interval = tokio::time::interval(Duration::from_millis(50));

    // Embedded panes' reader threads ping this to force a coalesced repaint, so
    // keystroke echo in an embedded `claude` stays responsive (not 50ms-laggy).
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    app.embedded_wake = Some(wake_tx);

    // Off-loop git-status worker → event loop. Seed one refresh up front, then
    // refresh periodically (and on demand) so branch/diff status stays current
    // without ever blocking the render loop on a git call.
    let (status_tx, mut status_rx) =
        tokio::sync::mpsc::unbounded_channel::<HashMap<String, kommand0_core::BranchStatus>>();
    app.status_tx = Some(status_tx);
    app.request_branch_status_refresh();
    app.last_status_refresh = Some(Instant::now());

    // Off-loop PR/CI-status worker → event loop. Seeded once, then refreshed on a
    // slow interval (one `gh pr list` per repo is a network call).
    let (pr_status_tx, mut pr_status_rx) =
        tokio::sync::mpsc::unbounded_channel::<HashMap<String, kommand0_core::PrStatus>>();
    app.pr_status_tx = Some(pr_status_tx);
    app.request_pr_status_refresh();
    app.last_pr_status_refresh = Some(Instant::now());

    // Cleanup worker → event loop, carrying `(workspace_id, Ok(()) | Err(msg))`.
    let (cleanup_tx, mut cleanup_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<(), String>)>();
    app.cleanup_tx = Some(cleanup_tx);

    // Redraw coalescing for the wake arm: the visible pane's `output_seq` as of
    // the last draw, when that draw happened, and whether the wake arm proved
    // the next frame would be identical (background-only output: skip it).
    let mut last_drawn_seq: Option<u64> = None;
    let mut last_draw_at = Instant::now();
    let mut skip_draw = false;

    loop {
        if !skip_draw {
            // Seed the viewed session before drawing so a just-opened/just-switched
            // tab is never momentarily flagged "needs you" before the next tick.
            app.mark_active_viewed();
            // Edge-triggered focus synthesis for embedded children, once per frame:
            // covers every transition (terminal focus, Tree<->Embedded, tab and
            // workspace switches, a child opting in mid-run) without per-site wiring.
            app.sync_focus_states();
            // Sample BEFORE drawing: a read after could record a seq whose chunk
            // is not in the frame just drawn, silently skipping that output.
            let seq = app.visible_pane_seq();
            terminal.draw(|frame| render::ui(frame, &mut app))?;
            last_drawn_seq = seq;
            last_draw_at = Instant::now();
        }
        skip_draw = false;

        tokio::select! {
            _ = wake_rx.recv() => {
                // Drain any backlog so we coalesce into a single redraw.
                while wake_rx.try_recv().is_ok() {}
                // Invariant: this arm stays mutation-free beyond draining and
                // pacing. Any state change belongs in an always-draw arm; the
                // seq-only comparison below relies on visible-pane identity
                // changes (tab/workspace switches, opens, reaps) flowing
                // through arms that always repaint.
                if app.visible_pane_seq() == last_drawn_seq {
                    // Background-only output: the visible pane's frame would be
                    // unchanged, so skip the repaint (a resume storm in other
                    // workspaces must not peg the UI thread).
                    skip_draw = true;
                } else {
                    // Visible-pane output storm: pace repaints to ~66fps. Defer
                    // rather than drop: dropping would regress keystroke echo to
                    // the 50ms tick (the input-arm draw resets the clock and the
                    // echo lands within the cap). Read elapsed() ONCE and subtract
                    // from that value; a guard and subtraction on two separate
                    // reads can underflow, and Duration subtraction panics.
                    const WAKE_REDRAW_CAP: Duration = Duration::from_millis(15);
                    let e = last_draw_at.elapsed();
                    if e < WAKE_REDRAW_CAP {
                        tokio::time::sleep(WAKE_REDRAW_CAP - e).await;
                        // Pick up chunks that arrived during the pause so they
                        // land in this draw, not a queued extra wake iteration.
                        while wake_rx.try_recv().is_ok() {}
                    }
                }
            }
            Some(status) = status_rx.recv() => {
                // A status refresh finished: replace the cache wholesale (drops
                // entries for deleted workspaces) and allow the next refresh.
                app.branch_status = status;
                app.status_inflight = false;
            }
            Some(pr_status) = pr_status_rx.recv() => {
                // A PR-status refresh finished: replace the cache wholesale (drops
                // entries for deleted workspaces / closed PRs) and allow the next.
                app.pr_status = pr_status;
                app.pr_status_inflight = false;
            }
            Some((ws_id, result)) = cleanup_rx.recv() => {
                app.cleanup_inflight.remove(&ws_id);
                match result {
                    Ok(()) => {
                        // The worktree + branch are gone; drop the workspace too
                        // (exact id; a no-op if it was already deleted meanwhile,
                        // even when a new workspace has reused the id as a name).
                        if app.state.delete_workspace_by_id(&ws_id).is_ok() {
                            app.workspaces = app.state.workspaces.clone();
                            app.expanded_icon_rows.remove(&ws_id);
                            app.cleanup_result.remove(&ws_id);
                            app.rebuild_tree();
                            // clamp_selection re-seats off any hint row AND
                            // re-syncs the active session (a raw clamp skipped
                            // both, stranding focus/selection after cleanup).
                            app.clamp_selection();
                            // Rows shifted under selected_index — a DIFFERENT
                            // workspace's pane can now be the visible one (the
                            // one pane-switch route with no key and no Down),
                            // so a highlight (or live drag) must not carry over.
                            // (Inside the select! loop — unit-unreachable;
                            // review-only coverage.)
                            app.pane_selection = None;
                        }
                    }
                    Err(msg) => {
                        app.cleanup_result.insert(ws_id, msg);
                    }
                }
                app.request_branch_status_refresh();
            }
            maybe_event = input_rx.recv() => {
                let Some(event) = maybe_event else { break; }; // reader thread ended
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if handle_key(&mut app, key).await? == KeyOutcome::Quit {
                            break;
                        }
                    }
                    Event::Resize(_, _) => {
                        // No-op: the redraw at the top of the loop re-queries the
                        // terminal size (ratatui autoresize) and repaints, so a
                        // resize needs no explicit handling. (Input is read on a
                        // blocking thread above because crossterm's async
                        // EventStream wedges on a rapid resize burst.)
                    }
                    // Terminal focus (mode 1004, enabled at startup). Handled
                    // here at the top level on purpose: these aren't keys, so
                    // no overlay/focus guard may swallow them — the composite
                    // focus for embedded children must always track reality.
                    Event::FocusGained => app.terminal_focused = true,
                    Event::FocusLost => app.terminal_focused = false,
                    Event::Paste(text) => {
                        // Bracketed paste arrives as one event (not Char keys).
                        // Route it with the SAME precedence handle_key uses: the
                        // embedded pane wins unless a modal is up, then modal →
                        // palette → filter. The pane must be checked first because
                        // a stale `/` filter can coexist with a mouse-opened pane
                        // (toggle_embedded leaves filter_input set, and handle_key
                        // checks embedded before filter too) — routing filter first
                        // would steal the pane's paste. Help swallows keys and
                        // implies Focus::Tree, so it falls through to a no-op here,
                        // matching its key behavior.
                        if app.show_diff {
                            // The diff overlay owns input (its key handler swallows
                            // too) — don't leak a paste to the tree/filter behind it.
                        } else if let Some(s) = app.settings.as_mut() {
                            // Settings owns the screen (implies Focus::Tree); paste
                            // only lands in an open field edit, else it's dropped.
                            if let Some(edit) = s.edit.as_mut() {
                                edit.paste(&text);
                            }
                        } else if app.focus == Focus::Embedded && !app.modal.is_active() {
                            // Forward raw to the embedded app as a bracketed paste
                            // (claude enables it, so a multi-line paste stays one
                            // block). Keep newlines here — the pane wants them.
                            let sent = app.active_pane_mut().map(|pane| {
                                let mut bytes = b"\x1b[200~".to_vec();
                                bytes.extend_from_slice(text.as_bytes());
                                bytes.extend_from_slice(b"\x1b[201~");
                                let _ = pane.send(&bytes);
                            });
                            if sent.is_none() {
                                app.focus = Focus::Tree;
                            }
                        } else if app.modal.is_active() {
                            modal::handle_modal_paste(&mut app.modal, &text);
                        } else if let Some(p) = app.palette.as_mut() {
                            p.paste(&text);
                        } else if app.filter_input {
                            handle_filter_paste(&mut app, &text);
                        }
                    }
                    Event::Mouse(mouse_event) => {
                        if app.show_diff {
                            mouse::handle_diff_mouse(&mut app, mouse_event);
                        } else if app.show_help
                            || app.modal.is_active()
                            || app.palette.is_some()
                            || app.settings.is_some()
                        {
                            // An overlay owns the screen — ignore mouse (don't leak
                            // stray clicks to the tree/embedded claude behind it; a
                            // leaked click could even open a modal and orphan the
                            // palette, which captures keys after the modal block).
                            // An overlay opening mid-drag must not strand the flag.
                            // (Inside the select! loop — unit-unreachable;
                            // review-only coverage.)
                            app.dragging_divider = false;
                            app.pane_selection = None;
                        } else if mouse::handle_divider_drag(&mut app, mouse_event) {
                            // A tree/content border drag — consumed; don't route on.
                        } else if mouse::handle_selection(
                            &mut app,
                            mouse_event,
                            &mut std::io::stdout(),
                        ) {
                            // A drag-selection gesture over a mouse-less embedded
                            // pane (before the focus split on purpose: a Down from
                            // the tree both focuses and anchors). OSC 52 copy
                            // happens on release, inside handle_selection.
                        } else if app.focus == Focus::Embedded {
                            app.handle_embedded_mouse(mouse_event);
                        } else {
                            mouse::handle_mouse(&mut app, mouse_event);
                        }
                    }
                    _ => {}
                }

                // Process pending button actions (from mouse clicks)
                if let Some(action) = app.pending_button_action.take() {
                    match action {
                        // Detail-pane [Open Claude] button (uses selected workspace)
                        buttons::HitAction::StartSession => {
                            app.toggle_embedded();
                        }
                        // Tree-icon start/resume/retry buttons: open the embedded claude.
                        buttons::HitAction::StartSessionFor { workspace_id }
                        | buttons::HitAction::ResumeSessionFor { workspace_id }
                        | buttons::HitAction::RetrySessionFor { workspace_id } => {
                            app.embed_workspace_by_id(&workspace_id);
                        }
                        // Session tab strip: select a tab, or open a new one.
                        buttons::HitAction::SelectSessionTab { workspace_id, index } => {
                            app.select_session_tab(&workspace_id, index);
                        }
                        buttons::HitAction::NewSessionTab { workspace_id } => {
                            app.new_tab(TabKind::Claude, &workspace_id);
                        }
                        buttons::HitAction::CleanupWorkspaceFor { workspace_id } => {
                            app.cleanup_workspace_prompt(&workspace_id);
                        }
                        buttons::HitAction::StopSessionFor { workspace_id } => {
                            app.embedded.remove(&workspace_id);
                            if let Some(session_id) = app.state.find_session_by_workspace(&workspace_id)
                                .filter(|s| s.status == SessionStatus::Running)
                                .map(|s| s.id.clone())
                            {
                                let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                app.update_active_session();
                            }
                        }
                        buttons::HitAction::FocusComposerFor { workspace_id } => {
                            // Focus the embedded claude for the given workspace.
                            if app.embedded.contains_key(&workspace_id) {
                                app.embed_workspace_by_id(&workspace_id);
                            }
                        }
                        buttons::HitAction::ToggleIconsFor { workspace_id } => {
                            // Toggle force-expanded icons for narrow pane
                            if app.expanded_icon_rows.contains(&workspace_id) {
                                app.expanded_icon_rows.remove(&workspace_id);
                            } else {
                                app.expanded_icon_rows.insert(workspace_id);
                            }
                        }
                        buttons::HitAction::DeleteWorkspaceFor { workspace_id } => {
                            // Only act while the workspace still exists
                            if let Some(ws) = app.state.workspaces.iter().find(|w| w.id == workspace_id).cloned() {
                                // Stop running session first
                                if let Some(session_id) = app.state.find_session_by_workspace(&workspace_id)
                                    .filter(|s| s.status == SessionStatus::Running)
                                    .map(|s| s.id.clone())
                                {
                                    let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                }
                                if app.state.delete_workspace_by_id(&ws.id).is_ok() {
                                    app.embedded.remove(&workspace_id);
                                    app.expanded_icon_rows.remove(&workspace_id);
                                    app.repos = app.state.repos.clone();
                                    app.workspaces = app.state.workspaces.clone();
                                    app.rebuild_tree();
                                    // Re-seat off any hint row and re-sync the
                                    // active session (clamp_selection does both).
                                    app.clamp_selection();
                                }
                            }
                        }
                        buttons::HitAction::DeleteRepoFor { repo_name } => {
                            // Stop all running sessions for this repo's workspaces
                            if let Ok(repo) = app.state.resolve_repo(&repo_name).cloned() {
                                let ws_ids: Vec<String> = app.state.workspaces.iter()
                                    .filter(|w| w.repo_id == repo.id)
                                    .map(|w| w.id.clone())
                                    .collect();
                                for ws_id in &ws_ids {
                                    if let Some(session_id) = app.state.find_session_by_workspace(ws_id)
                                        .filter(|s| s.status == SessionStatus::Running)
                                        .map(|s| s.id.clone())
                                    {
                                        let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                    }
                                    app.embedded.remove(ws_id);
                                    app.expanded_icon_rows.remove(ws_id);
                                }
                                if app.state.delete_repo(&repo_name).is_ok() {
                                    app.expanded.remove(&repo.id);
                                    app.repos = app.state.repos.clone();
                                    app.workspaces = app.state.workspaces.clone();
                                    app.rebuild_tree();
                                    // Re-seat off any hint row and re-sync the
                                    // active session (clamp_selection does both).
                                    app.clamp_selection();
                                }
                            }
                        }
                        buttons::HitAction::AddWorkspaceFor { repo_id } => {
                            // Open modal to add workspace to this repo
                            if let Some(repo) = app.state.repos.iter().find(|r| r.id == repo_id).cloned() {
                                app.modal = modal::ModalState::AddWorkspace {
                                    repo_id,
                                    repo_name: repo.name,
                                    input: String::new(),
                                    cursor: 0,
                                    branch: String::new(),
                                    branch_cursor: 0,
                                    field: modal::AddWorkspaceField::Name,
                                    error: None,
                                };
                            }
                        }
                        buttons::HitAction::AddRepo => {
                            app.modal = modal::ModalState::AddRepo {
                                input: String::new(),
                                cursor: 0,
                                error: None,
                                completions: Vec::new(),
                                completion_index: None,
                            };
                        }
                    }
                }
            }
            _ = tick_interval.tick() => {
                let now = Instant::now();
                // Reap embedded panes whose claude exited (leaves Embedded focus).
                app.reap_embedded(now);
                // Refresh per-pane activity so the tree spinner reflects which
                // sessions are currently producing output.
                app.update_pane_activity(now);
                // Advance spinner animation (every 5th tick = ~250ms at 50ms interval)
                app.tick_counter = app.tick_counter.wrapping_add(1);
                if app.tick_counter.is_multiple_of(5) {
                    app.spinner_tick = (app.spinner_tick + 1) % 10;
                }
                // Periodic git-status refresh (off-loop). Time-based so it doesn't
                // depend on the wrapping tick counter; interval is configurable.
                let status_interval = app
                    .config
                    .status_refresh_secs
                    // Floor at 1s so a mis-set 0 can't thrash a git subprocess
                    // every tick.
                    .map(|s| Duration::from_secs(s.max(1)))
                    .unwrap_or(STATUS_REFRESH_INTERVAL);
                if app
                    .last_status_refresh
                    .is_none_or(|t| now.duration_since(t) >= status_interval)
                {
                    app.last_status_refresh = Some(now);
                    app.request_branch_status_refresh();
                }
                // Periodic PR/CI-status refresh (off-loop, slow interval — it's a
                // network call per repo).
                if app
                    .last_pr_status_refresh
                    .is_none_or(|t| now.duration_since(t) >= PR_STATUS_REFRESH_INTERVAL)
                {
                    app.last_pr_status_refresh = Some(now);
                    app.request_pr_status_refresh();
                }
            }
        }
    }

    // Tear down all embedded panes in one shared grace period (not N×250ms).
    app.shutdown_panes();
    Ok(())
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // These two Kitty-protocol flags must travel together: REPORT_ALL_KEYS_AS_ESCAPE_CODES
    // routes shifted keys through CSI-u, and REPORT_ALTERNATE_KEYS is what makes the
    // terminal report the shifted codepoint. The behavioral contract they enable
    // (`?` resolves to Help, not Filter) is pinned in keymap.rs::shifted_symbols_resolve.
    #[test]
    fn enhancement_flags_pair_alternate_with_report_all() {
        use crossterm::event::KeyboardEnhancementFlags as Flags;
        let flags = keyboard_enhancement_flags();
        assert!(flags.contains(Flags::REPORT_ALL_KEYS_AS_ESCAPE_CODES));
        assert!(flags.contains(Flags::REPORT_ALTERNATE_KEYS));
    }

    #[test]
    fn translate_mouse_maps_content_and_rejects_strip_and_border() {
        // Right pane x=30,w=70 -> inner x=31,y=1; the active pane content starts
        // below the 1-row tab strip, at (31, 2).
        let area = ratatui::layout::Rect::new(30, 0, 70, 30);
        assert_eq!(translate_mouse(area, 31, 2), Some((0, 0))); // top-left content cell
        assert_eq!(translate_mouse(area, 59, 11), Some((28, 9)));
        // Rejected: borders, the tab strip row, and the tree pane.
        assert_eq!(translate_mouse(area, 30, 5), None); // left border
        assert_eq!(translate_mouse(area, 50, 0), None); // top border
        assert_eq!(translate_mouse(area, 50, 1), None); // tab strip row
        assert_eq!(translate_mouse(area, 10, 5), None); // tree pane
        assert_eq!(translate_mouse(area, 99, 5), None); // right border
        assert_eq!(translate_mouse(area, 50, 29), None); // bottom border
    }

    fn test_app() -> App {
        // Redirect persistence to a hermetic per-process temp dir so tests that
        // save (archive, rename, close-session) never touch the dev's
        // `.kommand0-dev`. Set once, early, to a single value.
        use std::sync::Once;
        static STATE_DIR: Once = Once::new();
        STATE_DIR.call_once(|| {
            let dir = std::env::temp_dir().join(format!("kommand0-tests-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            unsafe { std::env::set_var("KOMMAND0_STATE_DIR", &dir) };
        });

        let mut state = AppState::default();
        state.repos.push(RepoEntry {
            id: "r1".into(),
            name: "alpha".into(),
            path: "/tmp/alpha".into(),
        });
        state.repos.push(RepoEntry {
            id: "r2".into(),
            name: "beta".into(),
            path: "/tmp/beta".into(),
        });
        state.workspaces.push(Workspace {
            id: "w1".into(),
            name: "ws-one".into(),
            repo_id: "r1".into(),
            working_dir: "/tmp/alpha".into(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        });
        let mut app = App::new(state);
        // Every test that commits a setting writes to `config_path` — give each
        // App its own file (the KOMMAND0_STATE_DIR above is process-wide, so
        // the effective_path default would race under parallel tests).
        static NEXT_CONFIG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT_CONFIG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        app.config_path = std::env::temp_dir().join(format!(
            "kommand0-tests-{}-config-{n}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&app.config_path); // stale file from a reused pid
        app
    }

    async fn press(app: &mut App, code: KeyCode) -> KeyOutcome {
        handle_key(app, key(code)).await.unwrap()
    }

    #[test]
    fn seed_tree_width_defaults_and_clamps() {
        assert_eq!(seed_tree_width(None), TREE_WIDTH_DEFAULT);
        assert_eq!(seed_tree_width(Some(5)), TREE_WIDTH_MIN); // below min
        assert_eq!(seed_tree_width(Some(999)), TREE_WIDTH_MAX); // above max
        assert_eq!(seed_tree_width(Some(40)), 40); // in range, unchanged
    }

    #[test]
    fn shrink_and_widen_tree_clamp_at_bounds() {
        let mut app = test_app();

        app.tree_width_pct = TREE_WIDTH_MIN;
        app.shrink_tree();
        assert_eq!(app.tree_width_pct, TREE_WIDTH_MIN, "shrink @min stays at min");

        app.tree_width_pct = TREE_WIDTH_MAX;
        app.widen_tree();
        assert_eq!(app.tree_width_pct, TREE_WIDTH_MAX, "widen @max stays at max");

        app.tree_width_pct = 30;
        app.shrink_tree();
        assert_eq!(app.tree_width_pct, 25);

        app.tree_width_pct = 30;
        app.widen_tree();
        assert_eq!(app.tree_width_pct, 35);
    }

    #[test]
    fn set_tree_width_pct_clamps() {
        let mut app = test_app();
        app.set_tree_width_pct(5);
        assert_eq!(app.tree_width_pct, TREE_WIDTH_MIN); // below min
        app.set_tree_width_pct(95);
        assert_eq!(app.tree_width_pct, TREE_WIDTH_MAX); // above max
        app.set_tree_width_pct(40);
        assert_eq!(app.tree_width_pct, 40); // in range, unchanged
    }

    #[test]
    fn divider_drag_lifecycle() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let ev = |kind, column| MouseEvent {
            kind,
            column,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        let mut app = test_app();
        // tree width 30 in a body of 100 → divider col 29.
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);
        app.pane_areas.body = ratatui::layout::Rect::new(0, 0, 100, 8);
        app.tree_width_pct = 30;

        // Down on the divider grabs it; width unchanged until the drag.
        assert!(mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Down(MouseButton::Left), 29)
        ));
        assert!(app.dragging_divider);
        assert_eq!(app.tree_width_pct, 30, "grab alone must not resize");

        // Drag far from the divider still resizes (no per-Drag hit-test): col 49
        // in a width-100 body → 50%, and the event stays consumed.
        assert!(mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Drag(MouseButton::Left), 49)
        ));
        assert_eq!(app.tree_width_pct, 50);

        // A drag past the max clamps to TREE_WIDTH_MAX.
        assert!(mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Drag(MouseButton::Left), 90)
        ));
        assert_eq!(app.tree_width_pct, TREE_WIDTH_MAX);

        // Up ends the drag.
        assert!(mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Up(MouseButton::Left), 90)
        ));
        assert!(!app.dragging_divider);

        // A Down off the divider is not consumed (normal routing still runs).
        assert!(!mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Down(MouseButton::Left), 5)
        ));

        // With the flag clear, Drag/Up are not consumed and don't resize —
        // the gate that keeps drag-select / scroll routing alive.
        let before = app.tree_width_pct;
        assert!(!mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Drag(MouseButton::Left), 10)
        ));
        assert!(!mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Up(MouseButton::Left), 10)
        ));
        assert_eq!(app.tree_width_pct, before, "no resize while not dragging");

        // Release-safety: a stray non-Drag/Up event mid-drag (the `_` arm) ends
        // the grab and routes on — the flag can never get stranded true.
        assert!(mouse::handle_divider_drag(
            &mut app,
            ev(MouseEventKind::Down(MouseButton::Left), 29)
        ));
        assert!(app.dragging_divider, "re-grabbed the divider");
        let held = app.tree_width_pct;
        assert!(
            !mouse::handle_divider_drag(&mut app, ev(MouseEventKind::ScrollDown, 5)),
            "a stray event mid-drag is not consumed"
        );
        assert!(!app.dragging_divider, "stray event cleared the drag flag");
        assert_eq!(app.tree_width_pct, held, "stray event didn't resize");
    }

    fn mk_ws(id: &str, name: &str, repo: &str, branch: Option<&str>) -> Workspace {
        Workspace {
            id: id.into(),
            name: name.into(),
            repo_id: repo.into(),
            working_dir: "/tmp".into(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: branch.map(Into::into),
        }
    }

    fn ws_names(app: &App) -> Vec<String> {
        app.tree_items
            .iter()
            .filter_map(|n| match n {
                TreeNode::Workspace { ws, .. } => Some(ws.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rebuild_tree_filters_by_name_and_branch() {
        let mut app = test_app();
        app.workspaces = vec![
            mk_ws("w1", "auth-refactor", "r1", None),
            mk_ws("w2", "docs", "r1", None),
            mk_ws("w3", "misc", "r1", Some("billing")),
            mk_ws("w4", "ui", "r2", None),
        ];
        app.expanded.insert("r1".to_string());
        app.expanded.insert("r2".to_string());

        // Name match.
        app.filter_query = "auth".into();
        app.rebuild_tree();
        assert_eq!(ws_names(&app), vec!["auth-refactor"]);
        assert!(
            !app.tree_items.iter().any(|n| matches!(n, TreeNode::Hint { .. })),
            "no hint rows while filtering"
        );

        // Branch match (workspace name doesn't contain the query).
        app.filter_query = "billing".into();
        app.rebuild_tree();
        assert_eq!(ws_names(&app), vec!["misc"]);

        // A match in repo r2 force-expands r2 even though r1 is also expanded.
        app.filter_query = "ui".into();
        app.rebuild_tree();
        assert_eq!(ws_names(&app), vec!["ui"]);

        // Empty query => no filter (all four shown).
        app.filter_query.clear();
        app.rebuild_tree();
        assert_eq!(ws_names(&app).len(), 4);
    }

    #[test]
    fn paste_appends_to_filter_query_and_reapplies() {
        let mut app = test_app();
        app.workspaces = vec![
            mk_ws("w1", "auth", "r1", None),
            mk_ws("w2", "docs", "r1", None),
        ];
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.filter_input = true;

        handle_filter_paste(&mut app, "au\n"); // newline stripped
        assert_eq!(app.filter_query, "au", "control chars stripped, text appended");
        assert_eq!(ws_names(&app), vec!["auth"], "the filter is re-applied after paste");
    }

    #[test]
    fn paste_of_only_control_chars_leaves_the_filter_untouched() {
        let mut app = test_app();
        app.filter_query = "keep".into();
        app.filter_input = true;
        handle_filter_paste(&mut app, "\n\t");
        assert_eq!(app.filter_query, "keep", "a paste that sanitizes to nothing is a no-op");
    }

    #[tokio::test]
    async fn slash_filters_and_q_does_not_quit_while_typing() {
        let mut app = test_app();
        app.workspaces = vec![
            mk_ws("w1", "auth", "r1", None),
            mk_ws("w2", "docs", "r1", None),
        ];
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();

        press(&mut app, KeyCode::Char('/')).await;
        assert!(app.filter_input, "slash enters filter input");

        // A literal 'q' must edit the query, not quit the app.
        let outcome = press(&mut app, KeyCode::Char('q')).await;
        assert_eq!(outcome, KeyOutcome::Continue, "q while typing must not quit");
        assert_eq!(app.filter_query, "q");

        press(&mut app, KeyCode::Backspace).await;
        for c in "au".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        assert_eq!(app.filter_query, "au");
        assert_eq!(ws_names(&app), vec!["auth"], "tree narrows live");

        press(&mut app, KeyCode::Esc).await;
        assert!(!app.filter_input && app.filter_query.is_empty(), "Esc clears the filter");
        assert_eq!(ws_names(&app).len(), 2, "tree restored");
    }

    #[tokio::test]
    async fn filter_to_zero_matches_is_safe_and_enter_keeps_the_filter() {
        let mut app = test_app();
        app.workspaces = vec![mk_ws("w1", "auth", "r1", None)];
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();

        press(&mut app, KeyCode::Char('/')).await;
        for c in "zzz".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        // No match: the tree is empty, selection stays in-range, nothing panics.
        assert!(app.tree_items.is_empty());
        assert!(app.selected_workspace().is_none());

        // Narrow back to a match, then Enter keeps the filter applied for nav.
        for _ in 0..3 {
            press(&mut app, KeyCode::Backspace).await;
        }
        for c in "au".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        press(&mut app, KeyCode::Enter).await;
        assert!(!app.filter_input, "Enter exits input mode");
        assert_eq!(app.filter_query, "au", "Enter keeps the query applied");
        assert_eq!(ws_names(&app), vec!["auth"]);
        // Navigation works on the filtered tree (j/k reach the handlers now).
        assert!(app.selected_workspace().is_some());
    }

    #[test]
    fn expanded_empty_repo_shows_press_w_hint() {
        let mut app = test_app();
        // r2 (from test_app) has no workspaces; expanding it surfaces the hint
        // that points at the in-TUI `w` key.
        app.expanded.insert("r2".to_string());
        app.rebuild_tree();
        assert!(
            app.tree_items.iter().any(|n| matches!(
                n,
                TreeNode::Hint { text } if text.as_str() == "(no workspaces — press w to add)"
            )),
            "an expanded empty repo guides the user to press w"
        );
    }

    #[tokio::test]
    async fn adding_a_repo_auto_expands_it() {
        let mut app = test_app();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        // Submit the Add-Repo modal pre-filled with a real directory.
        app.modal = modal::ModalState::AddRepo {
            input: path.clone(),
            cursor: path.len(),
            error: None,
            completions: Vec::new(),
            completion_index: None,
        };
        press(&mut app, KeyCode::Enter).await;

        // The freshly added repo is tracked, auto-expanded, and immediately shows
        // its "press w" hint (no extra expand keypress needed).
        let new = app
            .state
            .repos
            .iter()
            .find(|r| r.id != "r1" && r.id != "r2")
            .expect("repo was added");
        assert!(app.expanded.contains(&new.id), "freshly added repo is auto-expanded");
        assert!(
            app.tree_items.iter().any(|n| matches!(
                n,
                TreeNode::Hint { text } if text.as_str() == "(no workspaces — press w to add)"
            )),
            "the new empty repo guides the user to press w"
        );
    }

    /// A real git repo at `dir` with an initial commit and an extra branch named
    /// `branch`. Mirrors the `init_git_repo` harness in `worktree.rs`/`lib.rs`.
    fn git_repo_with_branch(dir: &std::path::Path, branch: &str) {
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(dir).output().unwrap()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "--allow-empty", "-m", "init"]);
        git(&["branch", branch]);
    }

    /// Add a real git repo (id `real`) to the app, returning its TempDir guard.
    fn add_real_repo(app: &mut App, branch: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        git_repo_with_branch(dir.path(), branch);
        let repo = RepoEntry {
            id: "real".into(),
            name: "real".into(),
            path: dir.path().to_string_lossy().into_owned(),
        };
        app.state.repos.push(repo.clone());
        app.repos.push(repo);
        dir
    }

    fn add_workspace_modal_for(repo_id: &str, name: &str) -> modal::ModalState {
        modal::ModalState::AddWorkspace {
            repo_id: repo_id.to_string(),
            repo_name: "real".to_string(),
            input: name.to_string(),
            cursor: name.len(),
            branch: String::new(),
            branch_cursor: 0,
            field: modal::AddWorkspaceField::Name,
            error: None,
        }
    }

    #[tokio::test]
    async fn blank_branch_matching_existing_branch_offers_checkout() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "feat");
        // Name matches the existing branch, Branch blank, Enter.
        app.modal = add_workspace_modal_for("real", "feat");
        press(&mut app, KeyCode::Enter).await;

        match &app.modal {
            modal::ModalState::ConfirmBranchCheckout { repo_id, name, .. } => {
                assert_eq!(repo_id, "real");
                assert_eq!(name, "feat");
            }
            _ => panic!("expected the branch-exists confirm modal to open"),
        }
        // Creation is deferred — no workspace yet.
        assert!(!app.workspaces.iter().any(|w| w.name == "feat"), "creation deferred until the choice");
    }

    #[tokio::test]
    async fn blank_branch_no_matching_branch_creates_workspace() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "feat");
        // A name with no matching branch forks a fresh one immediately.
        app.modal = add_workspace_modal_for("real", "fresh");
        press(&mut app, KeyCode::Enter).await;

        assert!(matches!(app.modal, modal::ModalState::None), "no prompt, modal closes");
        let ws = app.workspaces.iter().find(|w| w.name == "fresh").expect("workspace created");
        assert_eq!(
            ws.branch_name.as_deref(),
            Some("fresh"),
            "forked a fresh branch named after the workspace (not a repo-root fallback)"
        );
    }

    #[tokio::test]
    async fn slash_name_checks_out_the_existing_branch_directly() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "user/slashed");
        // A '/' name can only be a branch: no offer prompt, straight to checkout
        // with the path-safe derived workspace name.
        app.modal = add_workspace_modal_for("real", "user/slashed");
        press(&mut app, KeyCode::Enter).await;

        assert!(matches!(app.modal, modal::ModalState::None), "no prompt, modal closes");
        let ws = app.workspaces.iter().find(|w| w.name == "user-slashed").expect("workspace created");
        assert_eq!(
            ws.branch_name.as_deref(),
            Some("user/slashed"),
            "checked out the existing slash branch, not a fresh fork"
        );
    }

    #[tokio::test]
    async fn slash_name_missing_branch_reopens_with_error() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "feat");
        // A '/' name whose branch doesn't exist must surface the checkout error,
        // not the invalid-name one, and keep the typed name for a retry.
        app.modal = add_workspace_modal_for("real", "ghost/nope");
        press(&mut app, KeyCode::Enter).await;

        match &app.modal {
            modal::ModalState::AddWorkspace { error: Some(e), input, .. } => {
                assert!(e.contains("branch not found"), "surfaces the checkout error: {e}");
                assert_eq!(input, "ghost/nope", "the typed name is preserved for a retry");
            }
            _ => panic!("expected AddWorkspace reopened with an error"),
        }
    }

    #[tokio::test]
    async fn explicit_branch_blank_name_derives_the_workspace_name() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "team/derive-me");
        // Branch filled, name blank: the workspace name is derived path-safe
        // from the branch.
        app.modal = modal::ModalState::AddWorkspace {
            repo_id: "real".into(),
            repo_name: "real".into(),
            input: String::new(),
            cursor: 0,
            branch: "team/derive-me".into(),
            branch_cursor: 14,
            field: modal::AddWorkspaceField::Branch,
            error: None,
        };
        press(&mut app, KeyCode::Enter).await;

        assert!(matches!(app.modal, modal::ModalState::None), "modal closes");
        let ws = app.workspaces.iter().find(|w| w.name == "team-derive-me").expect("workspace created");
        assert_eq!(ws.branch_name.as_deref(), Some("team/derive-me"));
    }

    #[tokio::test]
    async fn checkout_choice_fork_on_taken_name_reopens_with_blank_branch() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "feat");
        // A workspace named "feat" already exists, so the create fails; the fork
        // route must reopen AddWorkspace with the error and a BLANK branch.
        app.state.workspaces.push(Workspace {
            id: "taken".into(),
            name: "feat".into(),
            repo_id: "real".into(),
            working_dir: "/tmp/x".into(),
            active: true,
            created_at: 0,
            worktree_path: None,
            branch_name: None,
        });
        app.workspaces = app.state.workspaces.clone();
        app.modal = modal::ModalState::ConfirmBranchCheckout {
            repo_id: "real".into(),
            repo_name: "real".into(),
            name: "feat".into(),
        };
        // `f` forks; the create bails "workspace already exists".
        press(&mut app, KeyCode::Char('f')).await;

        match &app.modal {
            modal::ModalState::AddWorkspace { error: Some(e), branch, input, .. } => {
                assert!(e.contains("already exists"), "surfaces the create error: {e}");
                assert!(branch.is_empty(), "branch reopens blank, not the checkout name");
                assert_eq!(input, "feat", "the typed name is preserved for a retry");
            }
            _ => panic!("expected AddWorkspace reopened with an error"),
        }
    }

    #[tokio::test]
    async fn checkout_choice_checks_out_the_existing_bare_branch() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "feat");
        // The branch-exists prompt is open for the real local `feat`; Enter checks
        // it out (the existing branch, NOT a fresh suffixed fork).
        app.modal = modal::ModalState::ConfirmBranchCheckout {
            repo_id: "real".into(),
            repo_name: "real".into(),
            name: "feat".into(),
        };
        press(&mut app, KeyCode::Enter).await;

        assert!(matches!(app.modal, modal::ModalState::None), "the choice closes the modal");
        let ws = app.workspaces.iter().find(|w| w.name == "feat").expect("workspace created");
        assert_eq!(
            ws.branch_name.as_deref(),
            Some("feat"),
            "checked out the existing branch, not a suffixed fork"
        );
    }

    #[tokio::test]
    async fn checkout_choice_fork_on_free_name_creates_the_suffixed_branch() {
        let mut app = test_app();
        // Unique workspace name: worktrees land in the shared per-process state
        // dir and these tests share the literal repo id "real"
        // (`worktrees/real/<name>`), so reusing "feat" would race the checkout
        // test's worktree under parallel runs.
        let _repo = add_real_repo(&mut app, "forkme");
        // The prompt is open because local `forkme` exists; `f` forks — the
        // fork must land on the suffixed branch, never shadow the existing one.
        app.modal = modal::ModalState::ConfirmBranchCheckout {
            repo_id: "real".into(),
            repo_name: "real".into(),
            name: "forkme".into(),
        };
        press(&mut app, KeyCode::Char('f')).await;

        assert!(matches!(app.modal, modal::ModalState::None), "the choice closes the modal");
        let ws = app.workspaces.iter().find(|w| w.name == "forkme").expect("workspace created");
        assert_eq!(ws.branch_name.as_deref(), Some("forkme-2"), "forked the suffixed branch");
    }

    #[tokio::test]
    async fn confirm_branch_checkout_copy_does_not_name_the_fork() {
        // The fork's final name (suffixing) isn't known at render time, so the
        // modal must offer "fork a new branch" generically — naming one would lie.
        let mut app = test_app();
        app.modal = modal::ModalState::ConfirmBranchCheckout {
            repo_id: "r1".into(),
            repo_name: "alpha".into(),
            name: "feat".into(),
        };
        let text = render_to_string(&mut app, 100, 30);
        assert!(text.contains("fork a new branch"), "generic fork wording: {text}");
        assert!(!text.contains("fork kommand0/"), "never names a branch it can't know");
    }

    #[tokio::test]
    async fn explicit_branch_preserves_typed_branch_on_error() {
        let mut app = test_app();
        let _repo = add_real_repo(&mut app, "feat");
        // Submit a name with an EXPLICIT branch that doesn't exist: the checkout
        // fails, and the reopened modal must keep the typed branch (not blank it).
        app.modal = modal::ModalState::AddWorkspace {
            repo_id: "real".into(),
            repo_name: "real".into(),
            input: "ws".into(),
            cursor: 2,
            branch: "ghost".into(),
            branch_cursor: 5,
            field: modal::AddWorkspaceField::Name,
            error: None,
        };
        press(&mut app, KeyCode::Enter).await;

        match &app.modal {
            modal::ModalState::AddWorkspace { error: Some(e), branch, input, .. } => {
                assert!(e.contains("couldn't check out branch"), "surfaces the create error: {e}");
                assert_eq!(branch, "ghost", "the typed branch is preserved on error, not wiped");
                assert_eq!(input, "ws", "the typed name is preserved too");
            }
            _ => panic!("expected AddWorkspace reopened with an error"),
        }
    }

    #[tokio::test]
    async fn palette_opens_types_and_esc_closes() {
        let mut app = test_app();
        app.rebuild_tree();
        press(&mut app, KeyCode::Char(':')).await;
        assert!(app.palette.is_some(), "`:` opens the palette");
        press(&mut app, KeyCode::Char('w')).await;
        press(&mut app, KeyCode::Char('s')).await;
        assert_eq!(app.palette.as_ref().unwrap().query, "ws", "keys edit the query");
        press(&mut app, KeyCode::Esc).await;
        assert!(app.palette.is_none(), "Esc closes the palette");
    }

    #[tokio::test]
    async fn palette_jumps_to_a_workspace_under_a_collapsed_repo() {
        let mut app = test_app();
        app.rebuild_tree();
        // r1 is collapsed by default, so its workspace w1 ("ws-one") has no row.
        assert!(app.expanded.is_empty());
        assert!(
            !app.tree_items.iter().any(|n| matches!(n, TreeNode::Workspace { ws, .. } if ws.id == "w1")),
            "precondition: the target row is hidden under a collapsed repo"
        );

        press(&mut app, KeyCode::Char(':')).await;
        for c in "ws-one".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        assert!(
            matches!(
                app.palette.as_ref().unwrap().selected_action(),
                Some(palette::PaletteAction::OpenWorkspace { ws_id }) if ws_id == "w1"
            ),
            "the workspace jump for w1 is the top match for its name"
        );
        press(&mut app, KeyCode::Enter).await;

        // The palette closed and the jump expanded r1 + selected w1 — even though
        // its row didn't exist when the palette opened. (Opening the embedded pane
        // itself needs a claude stub; that's covered e2e.)
        assert!(app.palette.is_none(), "Enter closes the palette");
        assert!(app.expanded.contains("r1"), "jump expanded the target's repo");
        assert_eq!(app.selected_workspace().map(|w| w.id.as_str()), Some("w1"));
    }

    #[tokio::test]
    async fn palette_runs_an_action_not_just_a_jump() {
        let mut app = test_app();
        app.rebuild_tree();
        assert!(app.workspaces.iter().find(|w| w.id == "w1").unwrap().active, "w1 starts active");

        press(&mut app, KeyCode::Char(':')).await;
        for c in "archive".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        // "archive" isn't a subsequence of the plain jump text, so only the
        // Archive action matches — the palette runs actions, not just jumps.
        assert!(
            matches!(
                app.palette.as_ref().unwrap().selected_action(),
                Some(palette::PaletteAction::ArchiveToggle { ws_id }) if ws_id == "w1"
            ),
            "the Archive action is the match for 'archive'"
        );
        press(&mut app, KeyCode::Enter).await;
        assert!(app.palette.is_none(), "Enter closes the palette");
        assert!(
            !app.workspaces.iter().find(|w| w.id == "w1").unwrap().active,
            "running the action archived the workspace"
        );
    }

    #[tokio::test]
    async fn palette_jumps_to_a_specific_session_tab() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("s1", &["-c", "sleep 30"]), tab("s2", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );

        press(&mut app, KeyCode::Char(':')).await;
        for c in "tab 2".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        assert!(
            matches!(
                app.palette.as_ref().unwrap().selected_action(),
                Some(palette::PaletteAction::JumpTab { ws_id, index }) if ws_id == "w1" && *index == 1
            ),
            "'tab 2' selects the second session tab"
        );
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.embedded.get("w1").unwrap().active, 1, "jumped to tab 2 (index 1)");
        assert_eq!(app.focus, Focus::Embedded, "and focused the embedded pane");
    }

    #[tokio::test]
    async fn jump_to_next_waiting_cycles_through_needing_workspaces() {
        let mut app = test_app();
        for nth in 2..=4 {
            app.state.workspaces.push(Workspace {
                id: format!("w{nth}"),
                name: format!("ws-{nth}"),
                repo_id: "r1".into(),
                working_dir: "/tmp".into(),
                active: true,
                created_at: 0,
                worktree_path: None,
                branch_name: None,
            });
        }
        app.workspaces = app.state.workspaces.clone();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        // Mark w2 and w4 as "needs you": an embedded tab whose id is in attention.
        for ws in ["w2", "w4"] {
            let tab_id = format!("{ws}-s");
            app.embedded.insert(
                ws.to_string(),
                WorkspaceSessions { tabs: vec![tab(&tab_id, &["-c", "sleep 30"])], active: 0, last_active: None },
            );
            app.attention.insert(tab_id);
        }
        app.select_workspace_row("w1"); // start on a non-waiting row

        app.jump_to_waiting(true);
        assert_eq!(app.selected_workspace().map(|w| w.id.as_str()), Some("w2"));
        app.jump_to_waiting(true);
        assert_eq!(app.selected_workspace().map(|w| w.id.as_str()), Some("w4"), "skips non-waiting w3");
        app.jump_to_waiting(true);
        assert_eq!(app.selected_workspace().map(|w| w.id.as_str()), Some("w2"), "wraps");
        app.jump_to_waiting(false);
        assert_eq!(app.selected_workspace().map(|w| w.id.as_str()), Some("w4"), "previous wraps to w4");
    }

    #[tokio::test]
    async fn new_shell_session_persists_a_shell_sentinel() {
        let mut app = test_app();
        app.config.shell = Some("sh".to_string()); // deterministic, not the real $SHELL
        // Make sure the spawn cwd exists.
        if let Some(w) = app.workspaces.iter_mut().find(|w| w.id == "w1") {
            w.working_dir = "/tmp".into();
        }
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();

        app.new_tab(TabKind::Shell, "w1");
        let s = app.embedded.get("w1").expect("a tab was opened");
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.tabs[0].kind, TabKind::Shell, "it's a shell tab");
        assert!(
            s.tabs[0].id.starts_with("shell:"),
            "the tab id IS the persisted sentinel: {}",
            s.tabs[0].id
        );
        assert_eq!(app.focus, Focus::Embedded);
        // Persisted at creation, so reopening the workspace restores the tab.
        assert_eq!(
            app.state.embedded_session_ids("w1"),
            std::slice::from_ref(&s.tabs[0].id),
            "the shell sentinel is persisted for reopen"
        );
    }

    #[tokio::test]
    async fn toggle_embedded_restores_mixed_tabs_in_order() {
        let mut app = test_app();
        // `sh --resume <id>` (claude) and bare `sh` (shell) both spawn; the
        // children may exit at once, but tabs live until a reap runs, so the
        // assertions right after the call are deterministic.
        app.config.claude_bin = Some("sh".to_string());
        app.config.shell = Some("sh".to_string());
        if let Some(w) = app.workspaces.iter_mut().find(|w| w.id == "w1") {
            w.working_dir = "/tmp".into();
        }
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        // Ten interleaved entries: one past MAX_SESSION_TABS, so the cap shows too.
        let seeded: Vec<String> = (0..10)
            .map(|n| if n % 2 == 0 { format!("c{n}") } else { format!("shell:s{n}") })
            .collect();
        for id in &seeded {
            app.state.add_embedded_session("w1", id);
        }

        app.toggle_embedded();

        let s = app.embedded.get("w1").expect("tabs restored");
        assert_eq!(s.tabs.len(), MAX_SESSION_TABS, "capped at the tab limit");
        for (tab, want) in s.tabs.iter().zip(&seeded) {
            assert_eq!(&tab.id, want, "ids restored in persisted order");
            let want_kind =
                if want.starts_with("shell:") { TabKind::Shell } else { TabKind::Claude };
            assert_eq!(tab.kind, want_kind, "kind follows the sentinel for {want}");
            if want_kind == TabKind::Shell {
                assert!(!tab.was_resume, "reopened shells stay out of the resume nets");
            }
        }
        assert_eq!(s.active, 0, "reopen focuses the first tab");
        assert_eq!(
            app.state.embedded_session_ids("w1"),
            seeded.as_slice(),
            "persisted order and content unchanged"
        );
    }

    #[tokio::test]
    async fn reap_drops_an_exited_shell_tab_but_keeps_claude() {
        let mut app = test_app();
        // All three persisted, so the reap's forgetting is observable per kind.
        for id in ["claude-1", "shell:sh-1", "claude-2"] {
            app.state.add_embedded_session("w1", id);
        }
        let claude = tab("claude-1", &["-c", "sleep 30"]); // stays alive
        let mut shell = tab("shell:sh-1", &["-c", "exit 0"]); // exits immediately
        shell.kind = TabKind::Shell;
        let claude2 = tab("claude-2", &["-c", "exit 0"]); // a clean claude exit (code 0)
        let mut s =
            WorkspaceSessions { tabs: vec![claude, shell, claude2], active: 1, last_active: None };
        wait_exit(&mut s.tabs[1].pane);
        wait_exit(&mut s.tabs[2].pane);
        app.embedded.insert("w1".to_string(), s);

        app.reap_embedded(Instant::now());

        let ids: Vec<String> = app
            .embedded
            .get("w1")
            .map(|s| s.tabs.iter().map(|t| t.id.clone()).collect())
            .unwrap_or_default();
        assert_eq!(ids, vec!["claude-1".to_string()], "exited tabs dropped, live Claude kept");
        // The asymmetry, pinned both ways: an exited shell forgets its sentinel
        // (that shell is gone for good), while an exited claude keeps its id
        // (an /exit'd conversation resumes later).
        assert_eq!(
            app.state.embedded_session_ids("w1"),
            &["claude-1".to_string(), "claude-2".to_string()],
            "shell forgotten; exited claude still persisted"
        );
    }

    #[tokio::test]
    async fn closing_a_shell_tab_forgets_entry_and_title() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.state.add_embedded_session("w1", "shell:s1");
        app.state.set_embedded_session_title("w1", "shell:s1", "build");
        let mut sh = tab("shell:s1", &["-c", "sleep 30"]);
        sh.kind = TabKind::Shell;
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions { tabs: vec![sh], active: 0, last_active: None },
        );
        app.focus = Focus::Embedded;

        app.close_active_session();

        assert!(app.state.embedded_session_ids("w1").is_empty(), "the sentinel is forgotten");
        assert_eq!(
            app.state.embedded_session_title("w1", "shell:s1"),
            None,
            "the title goes in lockstep"
        );
        assert_eq!(app.focus, Focus::Tree, "last tab closed: back to the tree");
    }

    #[tokio::test]
    async fn closed_tab_does_not_resurrect_from_a_stale_baseline() {
        // The merge baseline must advance on every successful save. Left at the
        // startup snapshot, a workspace key first persisted mid-run reads as a
        // concurrent add once its last tab is closed (mine=absent,
        // baseline=absent, disk=present) and the closed tab resurrects on the
        // next reopen. Disk is modeled explicitly via merged_over (the exact
        // merge merge_save applies) because the test suite shares one
        // process-wide KOMMAND0_STATE_DIR, so reading state.json back races
        // other saving tests.
        let mut app = test_app();
        app.config.shell = Some("sh".to_string());
        app.config.claude_bin = Some("sh".to_string());
        if let Some(w) = app.workspaces.iter_mut().find(|w| w.id == "w1") {
            w.working_dir = "/tmp".into();
        }
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        assert!(app.state_baseline.embedded_session_ids("w1").is_empty(), "fresh at startup");

        // Shell flavor: the sentinel is born on disk mid-run.
        app.new_tab(TabKind::Shell, "w1");
        let shell_id = app.embedded["w1"].tabs[0].id.clone();
        assert_eq!(
            app.state_baseline.embedded_session_ids("w1"),
            std::slice::from_ref(&shell_id),
            "a successful save advances the baseline"
        );
        // The merge that close's own save performs: against the baseline as it
        // stands at close time and a disk that holds the entry (what the
        // spawn's save wrote). Pre-fix the baseline is still the startup
        // snapshot, so the entry reads as a concurrent add and survives.
        let baseline_at_close = app.state_baseline.clone();
        app.close_active_session();
        let mut disk = AppState::default();
        disk.add_embedded_session("w1", &shell_id);
        let merged = app.state.merged_over(&baseline_at_close, disk);
        assert!(
            merged.embedded_session_ids("w1").is_empty(),
            "the closed shell must read as our deletion, not a concurrent add"
        );
        assert!(
            app.state_baseline.embedded_session_ids("w1").is_empty(),
            "close's save advances the baseline past the deletion"
        );

        // Claude flavor: same bug class, fresh --session-id persisted mid-run.
        app.new_tab(TabKind::Claude, "w1");
        let claude_id = app.embedded["w1"].tabs[0].id.clone();
        assert_eq!(
            app.state_baseline.embedded_session_ids("w1"),
            std::slice::from_ref(&claude_id),
            "the claude save advances the baseline too"
        );
        let baseline_at_close = app.state_baseline.clone();
        app.close_active_session();
        let mut disk = AppState::default();
        disk.add_embedded_session("w1", &claude_id);
        let merged = app.state.merged_over(&baseline_at_close, disk);
        assert!(
            merged.embedded_session_ids("w1").is_empty(),
            "the closed claude tab must not resurrect either"
        );
    }

    #[tokio::test]
    async fn ctrl_a_d_detaches_panes_but_keeps_persisted_sessions() {
        let mut app = test_app();
        app.config.shell = Some("sh".to_string());
        app.config.claude_bin = Some("sh".to_string());
        if let Some(w) = app.workspaces.iter_mut().find(|w| w.id == "w1") {
            w.working_dir = "/tmp".into();
        }
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.new_tab(TabKind::Claude, "w1");
        app.new_tab(TabKind::Shell, "w1");
        let ids = app.state.embedded_session_ids("w1").to_vec();
        assert_eq!(ids.len(), 2, "one claude + one shell tab persisted");
        assert_eq!(app.focus, Focus::Embedded);
        // An unrelated workspace's live pane must survive the detach.
        app.embedded.insert(
            "w2".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("sess-w2", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );

        // Through the real dispatch, not a direct method call: Ctrl+A arms the
        // prefix, d detaches.
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        press(&mut app, KeyCode::Char('d')).await;

        assert!(!app.embedded.contains_key("w1"), "the live panes are gone");
        assert!(
            app.embedded.contains_key("w2"),
            "detach is scoped to the selected workspace"
        );
        assert!(!app.embedded_prefix, "the prefix is consumed");
        assert_eq!(
            app.state.embedded_session_ids("w1"),
            ids,
            "detach keeps every persisted entry (unlike close_active_session)"
        );
        assert_eq!(app.focus, Focus::Tree, "detach lands back on the tree");
    }

    #[tokio::test]
    async fn reopen_shell_spawn_failure_forgets_the_entry() {
        // KOMMAND0_SHELL would override config.shell and mask the failure path.
        assert!(
            std::env::var("KOMMAND0_SHELL").is_err(),
            "test needs the config-shell path; unset KOMMAND0_SHELL"
        );
        let mut app = test_app();
        // A missing binary makes Pane::spawn return Err (the claude twin of this
        // trick is pinned by the failed-resume e2e).
        app.config.shell = Some("/nonexistent/kommand0-shell".to_string());
        if let Some(w) = app.workspaces.iter_mut().find(|w| w.id == "w1") {
            w.working_dir = "/tmp".into();
        }
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.state.add_embedded_session("w1", "shell:s1");

        app.toggle_embedded();

        assert!(!app.embedded.contains_key("w1"), "no tabs opened");
        assert!(
            app.state.embedded_session_ids("w1").is_empty(),
            "the unspawnable sentinel is forgotten so the Vec stays aligned"
        );
        assert!(
            app.embed_error
                .as_ref()
                .is_some_and(|(_, m)| m.contains("Failed to start shell")),
            "the failure is surfaced: {:?}",
            app.embed_error
        );
        assert_eq!(app.focus, Focus::Tree, "nothing spawned: stay on the tree");
    }

    #[tokio::test]
    async fn jump_to_waiting_is_a_noop_when_nothing_waits() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        let before = app.selected_workspace().map(|w| w.id.clone());
        app.jump_to_waiting(true);
        assert_eq!(app.selected_workspace().map(|w| w.id.clone()), before, "nothing waiting: no move");
    }

    #[test]
    fn mouse_tree_click_accounts_for_scroll_offset() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let click = |app: &mut App, row: u16| {
            mouse::handle_mouse(
                app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 2,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
            );
        };
        let mut app = test_app();
        // Enough workspace rows that the list could scroll.
        for n in 2..=5 {
            app.state.workspaces.push(Workspace {
                id: format!("w{n}"),
                name: format!("ws-{n}"),
                repo_id: "r1".into(),
                working_dir: "/tmp".into(),
                active: true,
                created_at: 0,
                worktree_path: None,
                branch_name: None,
            });
        }
        app.workspaces = app.state.workspaces.clone();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);

        // Screen row 2 = the 2nd in-pane row (viewport row 1), a workspace row
        // (row 1 is the repo header). Offset 0 selects it directly.
        app.tree_scroll_offset = 0;
        click(&mut app, 2);
        let base = app.selected_index;
        // Same screen row, but the list has scrolled 2 rows: select 2 lower.
        app.tree_scroll_offset = 2;
        click(&mut app, 2);
        assert_eq!(
            app.selected_index,
            base + 2,
            "a tree click maps the viewport row through the scroll offset"
        );
    }

    #[test]
    fn embedded_tree_click_returns_focus_to_tree() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = test_app();
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 20);
        app.right_pane_area = ratatui::layout::Rect::new(30, 0, 70, 20);
        app.focus = Focus::Embedded;
        // A left-click in empty tree space (well below any row) must focus the
        // tree — with one repo/one workspace the tree is mostly empty, so this
        // is the only way to click back out of the embedded pane.
        app.handle_embedded_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 15,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.focus, Focus::Tree);
    }

    #[test]
    fn content_click_without_a_session_keeps_tree_focus() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = test_app();
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 20);
        app.right_pane_area = ratatui::layout::Rect::new(30, 0, 70, 20);
        app.focus = Focus::Tree;
        // No embedded session is live, so a click in the content pane must not
        // steal focus into a dead pane.
        mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 50,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.focus, Focus::Tree);
    }

    #[test]
    fn diff_overlay_renders_the_diff_note_for_an_empty_diff() {
        let mut app = test_app();
        app.show_diff = true;
        app.diff_title = "ws-one".into();
        // No files → empty rows → the left pane shows `diff_note` (set here as
        // open_diff would for a genuinely-empty diff).
        app.diff_files.clear();
        app.rebuild_diff_rows();
        app.diff_note = "No committed changes on this branch vs the default branch.".into();
        let text = render_to_string(&mut app, 100, 30);
        assert!(
            text.contains("No committed changes"),
            "an empty diff renders diff_note, not a blank dialog:\n{text}"
        );
    }

    #[test]
    fn open_diff_on_a_fallback_workspace_notes_no_branch_and_renders_it() {
        // A fallback workspace (no worktree_path) has no branch to review: the
        // note distinguishes it from an empty diff / a non-repo, and it renders.
        let mut app = test_app();
        app.workspaces = app.state.workspaces.clone();
        // test_app's w1 already has worktree_path = None (a fallback workspace).
        app.open_diff("w1");
        assert!(app.diff_rows.is_empty());
        assert_eq!(app.diff_note, "This workspace has no branch to review.");
        let text = render_to_string(&mut app, 100, 30);
        assert!(
            text.contains("branch to review"),
            "the no-branch note renders in the dialog:\n{text}"
        );
    }

    #[test]
    fn open_diff_titles_by_branch_and_gates_on_worktree() {
        let mut app = test_app();
        app.workspaces = app.state.workspaces.clone();
        // With a worktree + branch, the title carries the branch.
        if let Some(w) = app.workspaces.iter_mut().find(|w| w.id == "w1") {
            w.worktree_path = Some("/nonexistent/worktree".into());
            w.branch_name = Some("feat".into());
        }
        app.open_diff("w1");
        assert!(app.show_diff);
        assert_eq!(app.diff_title, "ws-one (feat)");
        // Without a worktree (fallback workspace) it shows the no-branch note.
        if let Some(w) = app.workspaces.iter_mut().find(|w| w.id == "w1") {
            w.worktree_path = None;
        }
        app.open_diff("w1");
        assert_eq!(app.diff_title, "ws-one");
        assert!(app.diff_rows.is_empty());
        assert!(app.diff_files.is_empty());
    }

    #[tokio::test]
    async fn open_pr_in_browser_is_a_safe_noop_without_a_pr() {
        // Pressing `p` on a workspace with no cached pr_status must not panic (and
        // must not open anything). The actual browser spawn isn't unit-testable —
        // only the no-PR guard is exercised here.
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        assert!(app.selected_workspace().is_some());
        assert!(app.pr_status.is_empty());
        assert_eq!(press(&mut app, KeyCode::Char('p')).await, KeyOutcome::Continue);
    }

    // --- two-pane diff dialog ---

    fn file_diff(path: &str, body: &str) -> kommand0_core::FileDiff {
        kommand0_core::FileDiff {
            path: path.to_string(),
            text: format!("diff --git a/{path} b/{path}\n{body}"),
        }
    }

    /// A diff dialog seeded with `paths` (each an empty-ish section), all folders
    /// expanded, rows rebuilt, selection on the first File.
    fn diff_app(paths: &[&str]) -> App {
        let mut app = test_app();
        app.show_diff = true;
        app.diff_files = paths.iter().map(|p| file_diff(p, "@@ -0,0 +1 @@\n+x\n")).collect();
        for p in paths {
            for parent in folder_prefixes(p) {
                app.diff_expanded.insert(parent);
            }
        }
        app.rebuild_diff_rows();
        app.diff_selected = app
            .diff_rows
            .iter()
            .position(|r| matches!(r, DiffRow::File { .. }))
            .unwrap_or(0);
        app
    }

    fn row_labels(app: &App) -> Vec<String> {
        app.diff_rows
            .iter()
            .map(|r| match r {
                DiffRow::Folder { path, .. } => format!("D:{path}"),
                DiffRow::File { file_idx, .. } => format!("F:{}", app.diff_files[*file_idx].path),
            })
            .collect()
    }

    #[test]
    fn rebuild_diff_rows_nests_folders_and_files_sorted() {
        // Paths in two dirs (out of order) → folder rows before their files, in a
        // stable sorted order, with the right depths.
        let app = diff_app(&["src/main.rs", "README.md", "src/lib.rs", "docs/guide.md"]);
        assert_eq!(
            row_labels(&app),
            vec![
                "F:README.md",   // top-level file
                "D:docs",        // folder
                "F:docs/guide.md",
                "D:src",         // folder (its two files, sorted)
                "F:src/lib.rs",
                "F:src/main.rs",
            ]
        );
        // Depth: top-level file 0, folder 0, nested file 1.
        assert!(matches!(app.diff_rows[0], DiffRow::File { depth: 0, .. }));
        assert!(matches!(app.diff_rows[1], DiffRow::Folder { depth: 0, .. }));
        assert!(matches!(app.diff_rows[2], DiffRow::File { depth: 1, .. }));
    }

    #[test]
    fn rebuild_diff_rows_hides_a_collapsed_folders_descendants() {
        let mut app = diff_app(&["src/main.rs", "src/lib.rs", "README.md"]);
        // Collapse `src`: its two files disappear; the folder row stays.
        app.diff_expanded.remove("src");
        app.rebuild_diff_rows();
        assert_eq!(row_labels(&app), vec!["F:README.md", "D:src"]);
        // Re-expand: files reappear.
        app.diff_expanded.insert("src".into());
        app.rebuild_diff_rows();
        assert_eq!(row_labels(&app), vec!["F:README.md", "D:src", "F:src/lib.rs", "F:src/main.rs"]);
    }

    #[test]
    fn rebuild_diff_rows_hides_a_nested_collapsed_subtree() {
        // Path-sorted: "a/b/deep.rs" < "a/top.rs" ('/' < 't', and 'b' < 't'), so
        // the `a/b` subtree comes before the top-level file.
        let mut app = diff_app(&["a/b/deep.rs", "a/top.rs"]);
        assert_eq!(
            row_labels(&app),
            vec!["D:a", "D:a/b", "F:a/b/deep.rs", "F:a/top.rs"]
        );
        // Collapsing the outer `a` hides everything under it (including `a/b`).
        app.diff_expanded.remove("a");
        app.rebuild_diff_rows();
        assert_eq!(row_labels(&app), vec!["D:a"]);
    }

    #[tokio::test]
    async fn diff_tab_toggles_focus_between_panes() {
        let mut app = diff_app(&["a.txt"]);
        assert_eq!(app.diff_focus, DiffFocus::Files);
        press(&mut app, KeyCode::Tab).await;
        assert_eq!(app.diff_focus, DiffFocus::Diff);
        press(&mut app, KeyCode::Tab).await;
        assert_eq!(app.diff_focus, DiffFocus::Files);
    }

    #[tokio::test]
    async fn diff_pending_g_is_reset_on_tab_and_on_close() {
        // The overlay and the tree share `pending_g`; toggling focus or closing
        // must clear it so a half-typed `gg` can't complete across the boundary.
        let mut app = diff_app(&["a.txt"]);
        app.pending_g = true;
        press(&mut app, KeyCode::Tab).await;
        assert!(!app.pending_g, "Tab clears a pending g");

        app.pending_g = true;
        press(&mut app, KeyCode::Esc).await; // close
        assert!(!app.show_diff);
        assert!(!app.pending_g, "closing clears a pending g");
    }

    #[tokio::test]
    async fn diff_jk_move_the_file_selection_in_files_focus() {
        let mut app = diff_app(&["a.txt", "b.txt", "c.txt"]);
        assert_eq!(app.diff_selected, 0);
        press(&mut app, KeyCode::Char('j')).await;
        assert_eq!(app.diff_selected, 1);
        press(&mut app, KeyCode::Char('j')).await;
        assert_eq!(app.diff_selected, 2);
        press(&mut app, KeyCode::Char('j')).await; // clamps at the last row
        assert_eq!(app.diff_selected, 2);
        press(&mut app, KeyCode::Char('k')).await;
        assert_eq!(app.diff_selected, 1);
    }

    #[tokio::test]
    async fn diff_enter_expands_and_collapses_a_folder() {
        let mut app = diff_app(&["src/main.rs", "README.md"]);
        // Select the `src` folder row (index 1: README.md, src, src/main.rs).
        app.diff_selected = 1;
        assert!(matches!(app.diff_rows[1], DiffRow::Folder { .. }));
        // h collapses an expanded folder — its file vanishes.
        press(&mut app, KeyCode::Char('h')).await;
        assert!(!app.diff_expanded.contains("src"));
        assert_eq!(row_labels(&app), vec!["F:README.md", "D:src"]);
        // Enter (l/Right) re-expands it.
        press(&mut app, KeyCode::Enter).await;
        assert!(app.diff_expanded.contains("src"));
        assert_eq!(row_labels(&app), vec!["F:README.md", "D:src", "F:src/main.rs"]);
    }

    #[tokio::test]
    async fn diff_enter_on_a_file_moves_focus_to_the_diff_pane() {
        let mut app = diff_app(&["a.txt"]);
        assert!(matches!(app.diff_rows[app.diff_selected], DiffRow::File { .. }));
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.diff_focus, DiffFocus::Diff);
    }

    #[tokio::test]
    async fn diff_selecting_a_new_file_resets_the_diff_scroll() {
        let mut app = diff_app(&["a.txt", "b.txt"]);
        app.diff_scroll = 5; // as if scrolled into the first file
        press(&mut app, KeyCode::Char('j')).await; // move onto b.txt (a File)
        assert_eq!(app.diff_selected, 1);
        assert_eq!(app.diff_scroll, 0, "landing on a new file resets the body scroll");
    }

    #[test]
    fn diff_overlay_renders_file_tree_and_selected_diff() {
        let mut app = diff_app(&["src/main.rs", "README.md"]);
        // Select the nested file so the right pane shows its added line.
        app.diff_selected = row_labels(&app).iter().position(|l| l == "F:src/main.rs").unwrap();
        let text = render_to_string(&mut app, 100, 30);
        // Left pane: a folder caret and the file names.
        assert!(text.contains('\u{25BE}'), "an expanded folder shows the ▾ caret:\n{text}");
        assert!(text.contains("README.md"), "the left pane lists a filename:\n{text}");
        assert!(text.contains("main.rs"), "the left pane lists the nested filename:\n{text}");
        // Right pane: the selected file's added line.
        assert!(text.contains("+x"), "the right pane shows an added diff line:\n{text}");
    }

    #[test]
    fn diff_overlay_shows_collapsed_caret_for_a_collapsed_folder() {
        let mut app = diff_app(&["src/main.rs"]);
        app.diff_expanded.remove("src");
        app.rebuild_diff_rows();
        let text = render_to_string(&mut app, 100, 30);
        assert!(text.contains('\u{25B8}'), "a collapsed folder shows the ▸ caret:\n{text}");
    }

    #[test]
    fn diff_click_in_list_selects_and_toggles_a_folder() {
        let mut app = diff_app(&["src/main.rs", "README.md"]);
        // Render to populate the pane rects.
        let _ = render_to_string(&mut app, 100, 30);
        let la = app.diff_list_area;
        // Row index 1 is the `src` folder (README.md, src, src/main.rs).
        let clicked = app.diff_handle_click(la.x + 1, la.y + 1);
        assert!(clicked, "a click in the list area is consumed");
        assert_eq!(app.diff_selected, 1);
        assert!(!app.diff_expanded.contains("src"), "clicking a folder toggles it collapsed");
        assert_eq!(row_labels(&app), vec!["F:README.md", "D:src"]);
    }

    #[test]
    fn diff_click_on_a_file_row_selects_it_without_toggling_the_folder() {
        let mut app = diff_app(&["src/main.rs", "README.md"]);
        let _ = render_to_string(&mut app, 100, 30);
        // Rows: [F:README.md (0), D:src (1), F:src/main.rs (2)].
        assert_eq!(row_labels(&app), vec!["F:README.md", "D:src", "F:src/main.rs"]);
        let la = app.diff_list_area;
        app.diff_scroll = 7; // as if scrolled into another file's diff
        // Click viewport row 2 (scroll 0) → the FILE src/main.rs.
        let clicked = app.diff_handle_click(la.x + 1, la.y + 2);
        assert!(clicked, "a click in the list area is consumed");
        assert_eq!(app.diff_selected, 2, "lands on the file row");
        assert_eq!(app.diff_focus, DiffFocus::Files);
        assert!(app.diff_expanded.contains("src"), "a file click must NOT toggle the folder");
        assert_eq!(row_labels(&app), vec!["F:README.md", "D:src", "F:src/main.rs"], "rows unchanged");
        assert_eq!(app.diff_scroll, 0, "selecting a file resets the diff scroll");
    }

    #[test]
    fn diff_click_maps_through_a_non_zero_list_scroll() {
        // Every other click test has scroll 0; this pins the `(row - y) + scroll`
        // hit-test math with the list scrolled. 40 files, a short pane, so the
        // list must scroll to keep the selection in view.
        let paths: Vec<String> = (0..40).map(|i| format!("f{i:02}.txt")).collect();
        let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        let mut app = diff_app(&refs);
        // Jump to the last file so the render scrolls the list down, then render
        // to commit `diff_list_scroll`.
        app.diff_selected = app.diff_rows.len() - 1;
        let _ = render_to_string(&mut app, 100, 12);
        let scroll = app.diff_list_scroll;
        assert!(scroll > 0, "the list scrolled to keep the selection in view");
        let la = app.diff_list_area;
        // Click the top visible viewport row → index == scroll (not 0).
        let clicked = app.diff_handle_click(la.x + 1, la.y);
        assert!(clicked, "a click in the list area is consumed");
        assert_eq!(
            app.diff_selected, scroll as usize,
            "the top visible row maps to `scroll`, not row 0"
        );
        // Sanity: the selected row is the file the scroll offset points at.
        let want = format!("F:f{:02}.txt", scroll);
        assert_eq!(row_labels(&app)[app.diff_selected], want);
    }

    #[test]
    fn diff_click_in_body_focuses_the_diff_pane() {
        let mut app = diff_app(&["a.txt"]);
        let _ = render_to_string(&mut app, 100, 30);
        let ba = app.diff_body_area;
        assert!(app.diff_handle_click(ba.x + 1, ba.y + 1));
        assert_eq!(app.diff_focus, DiffFocus::Diff);
    }

    #[test]
    fn cli_short_circuit_handles_version_and_help() {
        assert!(
            cli_short_circuit(&["--version".to_string()]).unwrap().starts_with("kommand0 "),
            "--version prints the name + version"
        );
        assert!(cli_short_circuit(&["-V".to_string()]).is_some());
        assert!(cli_short_circuit(&["--help".to_string()]).unwrap().contains("Usage"));
        assert!(
            cli_short_circuit(&["--help".to_string()]).unwrap().contains("--profile"),
            "help documents --profile"
        );
        assert!(cli_short_circuit(&[]).is_none(), "no args launches the TUI");
        assert!(
            cli_short_circuit(&["anything-else".to_string()]).is_none(),
            "unknown args just launch the TUI"
        );
    }

    #[test]
    fn tmux_delivers_csi_u_option_table() {
        // Raw `tmux show -sv` captures carry a trailing newline.
        assert!(tmux_delivers_csi_u(Some("on\n"), Some("csi-u\n")));
        assert!(tmux_delivers_csi_u(Some("always\n"), Some("csi-u\n")));
        // tmux 3.7 defaults (off/xterm): both must be configured.
        assert!(!tmux_delivers_csi_u(Some("off\n"), Some("csi-u\n")));
        assert!(!tmux_delivers_csi_u(Some("on\n"), Some("xterm\n")));
        // An older tmux without the options (query fails) → not delivering.
        assert!(!tmux_delivers_csi_u(None, Some("csi-u\n")));
        assert!(!tmux_delivers_csi_u(Some("on\n"), None));
        assert!(!tmux_delivers_csi_u(None, None));
    }

    #[test]
    fn parse_profile_arg_table() {
        let args = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_profile_arg(&args(&[])), Ok(None), "absent");
        assert_eq!(parse_profile_arg(&args(&["--profile", "work"])), Ok(Some("work".into())));
        assert_eq!(parse_profile_arg(&args(&["--profile=work"])), Ok(Some("work".into())));
        assert!(parse_profile_arg(&args(&["--profile"])).is_err(), "missing value");
        assert!(parse_profile_arg(&args(&["--profile="])).is_err(), "empty equals value");
        // …but the space form takes even an empty next arg verbatim —
        // validation rejects it downstream (mirror of the `--profile=` row).
        assert_eq!(parse_profile_arg(&args(&["--profile", ""])), Ok(Some(String::new())));
        // Any position: the whole vector is scanned.
        assert_eq!(
            parse_profile_arg(&args(&["--other", "x", "--profile", "late"])),
            Ok(Some("late".into()))
        );
        // The space form takes the next arg verbatim — unreachable for --help
        // in main() (cli_short_circuit runs first and wins), and validation
        // rejects any leading-'-' name downstream anyway.
        assert_eq!(
            parse_profile_arg(&args(&["--profile", "--help"])),
            Ok(Some("--help".into()))
        );
        // Duplicate flag: first wins (kmd/clap errors instead — deliberate
        // divergence).
        assert_eq!(
            parse_profile_arg(&args(&["--profile", "a", "--profile", "b"])),
            Ok(Some("a".into()))
        );
    }

    #[tokio::test]
    async fn colon_while_filtering_types_a_colon_not_palette() {
        let mut app = test_app();
        app.rebuild_tree();
        press(&mut app, KeyCode::Char('/')).await; // enter the filter box
        assert!(app.filter_input);
        press(&mut app, KeyCode::Char(':')).await;
        assert!(app.palette.is_none(), "`:` while filtering must not open the palette");
        assert_eq!(app.filter_query, ":", "`:` edits the filter query instead");
    }

    async fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c)).await;
        }
    }

    /// Read the JSON the settings page wrote for this test's App.
    fn written_config(app: &App) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&app.config_path).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn comma_opens_settings_and_esc_closes() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char(',')).await;
        assert!(app.settings.is_some(), "`,` opens the settings page");
        press(&mut app, KeyCode::Esc).await;
        assert!(app.settings.is_none(), "Esc closes it");
        // The open binding closes too (round-trips through the keymap).
        press(&mut app, KeyCode::Char(',')).await;
        press(&mut app, KeyCode::Char(',')).await;
        assert!(app.settings.is_none());
    }

    #[tokio::test]
    async fn comma_while_filtering_types_a_comma_not_settings() {
        let mut app = test_app();
        app.rebuild_tree();
        press(&mut app, KeyCode::Char('/')).await; // enter the filter box
        assert!(app.filter_input);
        press(&mut app, KeyCode::Char(',')).await;
        assert!(app.settings.is_none(), "`,` while filtering must not open settings");
        assert_eq!(app.filter_query, ",", "`,` edits the filter query instead");
    }

    #[tokio::test]
    async fn settings_edit_commits_to_file_and_config() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char(',')).await;
        press(&mut app, KeyCode::Char('j')).await; // claude_args -> claude_bin
        press(&mut app, KeyCode::Enter).await;
        type_str(&mut app, "claude-dev").await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.config.claude_bin.as_deref(), Some("claude-dev"));
        assert_eq!(written_config(&app)["claude_bin"], "claude-dev");
        let s = app.settings.as_ref().unwrap();
        assert!(s.edit.is_none() && s.error.is_none(), "commit closes the edit");
    }

    #[tokio::test]
    async fn settings_esc_cancels_edit_without_writing() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char(',')).await;
        press(&mut app, KeyCode::Char('j')).await;
        press(&mut app, KeyCode::Enter).await;
        type_str(&mut app, "nope").await;
        press(&mut app, KeyCode::Esc).await;
        assert!(app.settings.is_some(), "Esc in edit mode only cancels the edit");
        assert_eq!(app.config.claude_bin, None);
        assert!(!app.config_path.exists(), "nothing written on cancel");
    }

    #[tokio::test]
    async fn settings_blank_commit_removes_the_key() {
        let mut app = test_app();
        std::fs::write(&app.config_path, r#"{ "claude_bin": "x", "future_knob": 1 }"#).unwrap();
        app.config.claude_bin = Some("x".into());
        press(&mut app, KeyCode::Char(',')).await;
        press(&mut app, KeyCode::Char('j')).await;
        press(&mut app, KeyCode::Enter).await; // edit seeded with "x"
        press(&mut app, KeyCode::Backspace).await; // now blank
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.config.claude_bin, None);
        let raw = written_config(&app);
        assert!(raw.get("claude_bin").is_none(), "blank removes the key");
        assert_eq!(raw["future_knob"], 1, "unknown keys survive");
    }

    #[tokio::test]
    async fn settings_commit_write_failure_keeps_config_and_shows_error() {
        let mut app = test_app();
        // A directory as the config path: reading it errors (not NotFound), so
        // the commit must fail cleanly — error shown, memory untouched.
        app.config_path = std::env::temp_dir();
        press(&mut app, KeyCode::Char(',')).await;
        press(&mut app, KeyCode::Char('j')).await;
        press(&mut app, KeyCode::Enter).await;
        type_str(&mut app, "claude-dev").await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.config.claude_bin, None, "failed write must not touch memory");
        let s = app.settings.as_ref().unwrap();
        assert!(s.error.is_some(), "error surfaces on the page");
        assert!(s.edit.is_some(), "edit stays open to fix/retry");
    }

    #[tokio::test]
    async fn settings_invalid_number_keeps_edit_open_with_error() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char(',')).await;
        for _ in 0..5 {
            press(&mut app, KeyCode::Char('j')).await; // -> status_refresh_secs
        }
        press(&mut app, KeyCode::Enter).await;
        type_str(&mut app, "fast").await;
        press(&mut app, KeyCode::Enter).await;
        let s = app.settings.as_ref().unwrap();
        assert!(s.error.is_some());
        assert!(s.edit.is_some());
        assert!(!app.config_path.exists(), "invalid input writes nothing");
        // Fixing the value clears the error and commits.
        for _ in 0..4 {
            press(&mut app, KeyCode::Backspace).await; // clear "fast"
        }
        type_str(&mut app, "5").await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.config.status_refresh_secs, Some(5));
        assert_eq!(written_config(&app)["status_refresh_secs"], 5);
        assert!(app.settings.as_ref().unwrap().error.is_none());
    }

    #[tokio::test]
    async fn settings_tree_width_commit_applies_and_clamps() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char(',')).await;
        for _ in 0..6 {
            press(&mut app, KeyCode::Char('j')).await; // -> tree_width_pct (last row)
        }
        press(&mut app, KeyCode::Enter).await;
        type_str(&mut app, "45").await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.tree_width_pct, 45, "applies live");
        // Out-of-range input is clamped before it's written.
        press(&mut app, KeyCode::Enter).await; // re-edit, seeded "45"
        press(&mut app, KeyCode::Backspace).await;
        press(&mut app, KeyCode::Backspace).await;
        type_str(&mut app, "999").await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(written_config(&app)["tree_width_pct"], 60, "file holds the clamped value");
        assert_eq!(app.tree_width_pct, TREE_WIDTH_MAX);
    }

    #[tokio::test]
    async fn settings_claude_args_respect_quotes_and_roundtrip() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char(',')).await; // claude_args is the first row
        press(&mut app, KeyCode::Enter).await;
        type_str(&mut app, r#"--append-system-prompt "be terse""#).await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(
            app.config.claude_args,
            vec!["--append-system-prompt".to_string(), "be terse".to_string()],
            "quoted arg stays one argv element"
        );
        // Reopen the seeded edit and commit unchanged: still exactly 2 args
        // (current() re-quotes, so open->Enter must not corrupt the value).
        press(&mut app, KeyCode::Enter).await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(
            written_config(&app)["claude_args"],
            serde_json::json!(["--append-system-prompt", "be terse"])
        );
    }

    #[tokio::test]
    async fn settings_theme_commit_applies_live_and_keeps_overrides() {
        let mut app = test_app();
        // A hand-edited role override must survive a theme change from the page.
        app.config.theme_colors.insert("accent".into(), "#ff8800".into());
        press(&mut app, KeyCode::Char(',')).await;
        for _ in 0..4 {
            press(&mut app, KeyCode::Char('j')).await; // -> theme
        }
        press(&mut app, KeyCode::Enter).await;
        type_str(&mut app, "high-contrast").await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.config.theme.as_deref(), Some("high-contrast"));
        assert_eq!(written_config(&app)["theme"], "high-contrast");
        assert_eq!(
            app.theme.error,
            ratatui::style::Color::LightRed,
            "high-contrast base actually applied live (default is Red)"
        );
        assert_eq!(
            app.theme.accent,
            theme::parse_color("#ff8800").unwrap(),
            "theme_colors override survives the live re-apply"
        );
    }

    #[tokio::test]
    async fn palette_jumps_to_an_archived_workspace() {
        let mut app = test_app();
        // An archived (inactive) workspace is still listed; r1 stays collapsed.
        app.workspaces = vec![mk_ws("w1", "ws-one", "r1", None)];
        app.workspaces[0].active = false;
        app.rebuild_tree();

        press(&mut app, KeyCode::Char(':')).await;
        for c in "ws-one".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        press(&mut app, KeyCode::Enter).await;

        // The jump lands on the archived row (rebuild_tree lists archived
        // workspaces), not a stale previously-selected one.
        assert!(app.expanded.contains("r1"));
        assert_eq!(
            app.selected_workspace().map(|w| w.id.as_str()),
            Some("w1"),
            "jump reaches an archived workspace"
        );
    }

    #[test]
    fn normalize_stale_running_heals_running_sessions() {
        let mut app = test_app();
        let mk = |id: &str, status| kommand0_core::Session {
            id: id.into(),
            workspace_id: "w1".into(),
            claude_session_id: None,
            pid: None,
            status,
            created_at: 0,
            ended_at: None,
            log_file: format!("{id}.log"),
        };
        app.state.sessions.push(mk("s1", SessionStatus::Running));
        app.state.sessions.push(mk("s2", SessionStatus::Stopped));

        // A persisted Running (crash/SIGKILL leftover) is flipped to Stopped.
        assert_eq!(app.normalize_stale_running(), 1, "one stale Running normalized");
        assert!(
            app.state.sessions.iter().all(|s| s.status != SessionStatus::Running),
            "no Running remains"
        );
        // Idempotent — a second pass finds nothing.
        assert_eq!(app.normalize_stale_running(), 0);
    }

    #[tokio::test]
    async fn capital_a_toggles_archive() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        assert!(app.selected_workspace().unwrap().active);

        press(&mut app, KeyCode::Char('A')).await;
        assert!(
            !app.workspaces.iter().find(|w| w.id == "w1").unwrap().active,
            "A archives an active workspace"
        );

        press(&mut app, KeyCode::Char('A')).await;
        assert!(
            app.workspaces.iter().find(|w| w.id == "w1").unwrap().active,
            "A re-activates an archived workspace"
        );
    }

    #[tokio::test]
    async fn j_k_and_arrows_navigate_tree() {
        let mut app = test_app();
        assert_eq!(app.selected_index, 0);
        press(&mut app, KeyCode::Char('j')).await;
        assert_eq!(app.selected_index, 1);
        press(&mut app, KeyCode::Char('k')).await;
        assert_eq!(app.selected_index, 0);
        press(&mut app, KeyCode::Down).await;
        assert_eq!(app.selected_index, 1);
        press(&mut app, KeyCode::Up).await;
        assert_eq!(app.selected_index, 0);
    }

    #[tokio::test]
    async fn l_expands_repo_and_h_collapses() {
        let mut app = test_app();
        assert_eq!(app.tree_items.len(), 2);
        press(&mut app, KeyCode::Char('l')).await;
        assert!(app.expanded.contains("r1"));
        assert_eq!(app.tree_items.len(), 3);
        // l on an expanded repo steps into its first child
        press(&mut app, KeyCode::Char('l')).await;
        assert_eq!(app.selected_index, 1);
        // h on a workspace jumps back to the parent repo
        press(&mut app, KeyCode::Char('h')).await;
        assert_eq!(app.selected_index, 0);
        // h on an expanded repo collapses it
        press(&mut app, KeyCode::Char('h')).await;
        assert!(!app.expanded.contains("r1"));
        assert_eq!(app.tree_items.len(), 2);
    }

    #[tokio::test]
    async fn gg_and_shift_g_jump_to_first_and_last() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('G')).await;
        assert_eq!(app.selected_index, app.tree_items.len() - 1);
        press(&mut app, KeyCode::Char('g')).await;
        assert!(app.pending_g);
        press(&mut app, KeyCode::Char('g')).await;
        assert_eq!(app.selected_index, 0);
        assert!(!app.pending_g);
    }

    #[tokio::test]
    async fn pending_g_is_cancelled_by_other_keys() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('G')).await;
        press(&mut app, KeyCode::Char('g')).await;
        press(&mut app, KeyCode::Esc).await; // any other key cancels the pending g
        press(&mut app, KeyCode::Char('g')).await;
        // gg never completed: still at the bottom, g is re-armed
        assert_eq!(app.selected_index, app.tree_items.len() - 1);
        assert!(app.pending_g);
    }

    #[tokio::test]
    async fn help_overlay_toggles_and_scrolls() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?')).await;
        assert!(app.show_help);
        press(&mut app, KeyCode::Char('j')).await;
        press(&mut app, KeyCode::Char('j')).await;
        assert_eq!(app.help_scroll, 2);
        press(&mut app, KeyCode::Char('k')).await;
        assert_eq!(app.help_scroll, 1);
        press(&mut app, KeyCode::Esc).await;
        assert!(!app.show_help);
        // Reopening resets scroll
        press(&mut app, KeyCode::Char('?')).await;
        assert_eq!(app.help_scroll, 0);
    }

    #[tokio::test]
    async fn q_quits() {
        let mut app = test_app();
        assert!(press(&mut app, KeyCode::Char('q')).await == KeyOutcome::Quit);
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            // Trim trailing blanks so snapshots aren't sensitive to right-pad.
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
        out
    }

    /// Render the app at a fixed size to the full-buffer string (for snapshots).
    fn render_to_string(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|frame| render::ui(frame, app)).unwrap();
        buffer_text(&terminal)
    }

    #[tokio::test]
    async fn renders_repo_tree() {
        let mut app = test_app();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("alpha"),
            "tree should list repo alpha:\n{text}"
        );
        assert!(text.contains("beta"), "tree should list repo beta:\n{text}");
    }

    #[tokio::test]
    async fn tree_title_shows_a_non_default_profile() {
        let mut app = test_app();
        app.profile_label = Some("work".into());
        let text = render_to_string(&mut app, 100, 30);
        assert!(text.contains("Repos · work"), "tree title carries the profile:\n{text}");
    }

    #[test]
    fn jump_repo_moves_between_repo_headers_and_clamps() {
        let mut app = test_app(); // repos alpha (r1, ws-one under it) and beta (r2)
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        // tree: [Repo r1, Workspace w1, Repo r2]
        app.selected_index = 1; // the workspace row
        app.jump_repo(true);
        assert_eq!(app.selected_index, 2, "}} skips the workspace row to the next repo");
        app.jump_repo(true);
        assert_eq!(app.selected_index, 2, "clamps at the last repo (no wrap)");
        app.jump_repo(false);
        assert_eq!(app.selected_index, 0, "{{ jumps back over the workspace row");
        app.jump_repo(false);
        assert_eq!(app.selected_index, 0, "clamps at the first repo");

        // Under an active filter the jumps operate on the filtered rows.
        app.filter_query = "beta".to_string();
        app.rebuild_tree();
        app.selected_index = 0; // only [Repo r2] remains
        app.jump_repo(true);
        assert_eq!(app.selected_index, 0, "single filtered repo: nowhere to jump");
        app.jump_repo(false);
        assert_eq!(app.selected_index, 0);
    }

    #[tokio::test]
    async fn status_line_shows_the_armed_ctrl_a_prefix() {
        let mut app = test_app();
        app.focus = Focus::Embedded;
        app.embedded_prefix = true;
        let text = render_to_string(&mut app, 100, 30);
        assert!(text.contains("Ctrl+A …"), "armed indicator shown:\n{text}");
        assert!(text.contains("d detach"), "hint lists detach:\n{text}");

        // Disarmed (the very next key clears the prefix): back to the
        // resting hint, no armed marker.
        app.embedded_prefix = false;
        let text = render_to_string(&mut app, 100, 30);
        assert!(!text.contains("Ctrl+A …"), "indicator gone when disarmed:\n{text}");
        assert!(text.contains("Ctrl+] tree"), "resting hint back:\n{text}");
    }

    // Golden full-screen snapshots of key layouts (geometry/position/borders that
    // the substring `contains` checks above can't catch). A repo is kept selected
    // so the detail pane never renders a workspace's local-timezone timestamp,
    // which would make the snapshot machine-dependent.
    #[tokio::test]
    async fn snapshot_tree_collapsed() {
        let mut app = test_app();
        insta::assert_snapshot!(render_to_string(&mut app, 100, 30));
    }

    #[tokio::test]
    async fn snapshot_tree_expanded() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.selected_index = 0; // repo selected (no timestamp in the detail pane)
        insta::assert_snapshot!(render_to_string(&mut app, 100, 30));
    }

    #[tokio::test]
    async fn snapshot_palette_overlay() {
        let mut app = test_app();
        app.workspaces = vec![mk_ws("w1", "ws-one", "r1", None)];
        app.rebuild_tree();
        let candidates = app.palette_candidates();
        app.palette = Some(palette::Palette::new(candidates));
        insta::assert_snapshot!(render_to_string(&mut app, 100, 30));
    }

    #[tokio::test]
    async fn snapshot_settings_overlay() {
        let mut app = test_app();
        // The page renders config_path in the footer — pin it so the snapshot
        // doesn't churn on the per-test temp path (render-only, never written).
        app.config_path = "/home/user/.config/kommand0/config.json".into();
        app.config.claude_args = vec!["--model".into(), "opus".into()];
        app.settings = Some(settings::SettingsState::default());
        insta::assert_snapshot!(render_to_string(&mut app, 100, 30));
    }

    #[tokio::test]
    async fn snapshot_add_repo_modal() {
        let mut app = test_app();
        app.modal = modal::ModalState::AddRepo {
            input: "/some/path".to_string(),
            cursor: "/some/path".len(),
            error: None,
            completions: Vec::new(),
            completion_index: None,
        };
        insta::assert_snapshot!(render_to_string(&mut app, 100, 30));
    }

    #[tokio::test]
    async fn snapshot_add_workspace_modal() {
        let mut app = test_app();
        app.modal = modal::ModalState::AddWorkspace {
            repo_id: "r1".to_string(),
            repo_name: "demo".to_string(),
            input: "review".to_string(),
            cursor: "review".len(),
            branch: "origin/feat/x".to_string(),
            branch_cursor: "origin/feat/x".len(),
            field: modal::AddWorkspaceField::Branch,
            error: None,
        };
        insta::assert_snapshot!(render_to_string(&mut app, 100, 30));
    }

    #[tokio::test]
    async fn snapshot_narrow_terminal() {
        let mut app = test_app();
        insta::assert_snapshot!(render_to_string(&mut app, 50, 12));
    }

    #[tokio::test]
    async fn embed_error_only_shows_for_its_workspace() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        // tree: [Repo alpha, Workspace ws-one, Repo beta]
        app.embed_error = Some((
            "w1".to_string(),
            "Failed to start claude in ws-one: boom".to_string(),
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        // On the failing workspace's own row, the error is shown.
        app.selected_index = 1;
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let on_ws = buffer_text(&terminal);
        assert!(
            on_ws.contains("Failed to start claude"),
            "error should show on its own workspace:\n{on_ws}"
        );

        // On an unrelated node (the beta repo) it must NOT bleed through.
        app.selected_index = 2;
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let on_other = buffer_text(&terminal);
        assert!(
            !on_other.contains("Failed to start claude"),
            "error must not show under an unrelated entity:\n{on_other}"
        );
    }

    #[tokio::test]
    async fn status_line_shows_mode_context_and_hints() {
        let mut app = test_app();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("TREE"), "status line should show TREE mode:\n{text}");
        assert!(text.contains("q quit"), "status line should show hints:\n{text}");
        assert!(
            text.contains("no live sessions"),
            "status line should show the live-session count:\n{text}"
        );
    }

    #[tokio::test]
    async fn status_line_keeps_mode_badge_on_narrow_terminal() {
        // The hints half is sized by display width (not byte length), so the mode
        // badge isn't starved off a narrow terminal.
        let mut app = test_app();
        let mut terminal = Terminal::new(TestBackend::new(50, 10)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("TREE"),
            "mode badge must survive on a 50-col terminal:\n{text}"
        );
    }

    #[test]
    fn pane_activity_debounces_arms_and_decays() {
        let mut app = test_app();
        let t = Instant::now();

        // One tick of new output: held pending by the debounce, not yet active.
        app.apply_pane_activity(t, &[("w1".to_string(), 1, None)]);
        assert!(
            !app.waiting_response.contains("w1"),
            "a single output tick must not arm (debounce)"
        );

        // A second consecutive tick of new output arms it.
        app.apply_pane_activity(t, &[("w1".to_string(), 2, None)]);
        assert!(
            app.waiting_response.contains("w1"),
            "two consecutive output ticks should mark the pane active"
        );

        // A gap shorter than the window keeps it active (bridges bursty output
        // so the spinner reads as continuous, not flickering).
        app.apply_pane_activity(t + Duration::from_millis(800), &[("w1".to_string(), 2, None)]);
        assert!(
            app.waiting_response.contains("w1"),
            "a sub-window output gap must not drop the spinner"
        );

        // No new output past the active window: decays to idle.
        app.apply_pane_activity(t + Duration::from_millis(2100), &[("w1".to_string(), 2, None)]);
        assert!(
            !app.waiting_response.contains("w1"),
            "a stale pane should decay to idle"
        );

        // A pane that disappears is pruned from all bookkeeping.
        app.apply_pane_activity(t, &[]);
        assert!(app.waiting_response.is_empty());
        assert!(app.pane_seen.is_empty());
        assert!(app.pane_pending.is_empty());
        assert!(app.pane_active_until.is_empty());
        assert!(app.last_output_at.is_empty());
    }

    #[test]
    fn input_driven_output_does_not_arm_or_stamp() {
        let mut app = test_app();
        let t = Instant::now();

        // Output ticks arriving right after user input (scroll-driven redraws)
        // are swallowed: no spinner, no last-output stamp, however long the
        // scrolling goes on.
        for seq in 1..=4 {
            app.apply_pane_activity(t, &[("w1".to_string(), seq, Some(t))]);
        }
        assert!(
            !app.waiting_response.contains("w1"),
            "scroll-driven redraws must not arm the spinner"
        );
        assert!(
            !app.last_output_at.contains_key("w1"),
            "input-driven output must not count for the settle/shell-busy checks"
        );

        // The same cadence with the input grace lapsed arms normally.
        let old_input = Some(t - Duration::from_secs(1));
        app.apply_pane_activity(t, &[("w1".to_string(), 5, old_input)]);
        app.apply_pane_activity(t, &[("w1".to_string(), 6, old_input)]);
        assert!(
            app.waiting_response.contains("w1"),
            "output past the input grace must arm the spinner"
        );
    }

    #[test]
    fn apply_shell_busy_overrides_waiting_response_per_tristate() {
        let mut app = test_app();
        let now = Instant::now();

        // `Some(true)` (foreground command running) marks a shell active when it
        // also produced output recently — even though the output-based pass, with
        // its shorter window, may already have dropped it.
        app.last_output_at.insert("sh1".to_string(), now);
        app.apply_shell_busy(now, &[("sh1".to_string(), Some(true))]);
        assert!(
            app.waiting_response.contains("sh1"),
            "a foreground-busy shell with recent output must spin"
        );

        // Same foreground-busy signal, but the command has gone quiet past the
        // idle window (an open editor / pager just sitting there) — must NOT spin.
        app.last_output_at
            .insert("sh1".to_string(), now - Duration::from_secs(10));
        app.apply_shell_busy(now, &[("sh1".to_string(), Some(true))]);
        assert!(
            !app.waiting_response.contains("sh1"),
            "a quiet foreground process (nvim/less) must not spin"
        );

        // `Some(false)` clears it (the command returned to the prompt).
        app.waiting_response.insert("sh1".to_string());
        app.apply_shell_busy(now, &[("sh1".to_string(), Some(false))]);
        assert!(
            !app.waiting_response.contains("sh1"),
            "an idle shell must stop spinning"
        );

        // `None` ("can't tell") must leave the output-based result untouched —
        // neither clearing an active tab nor adding an idle one.
        app.waiting_response.insert("sh2".to_string());
        app.apply_shell_busy(now, &[("sh2".to_string(), None), ("sh3".to_string(), None)]);
        assert!(
            app.waiting_response.contains("sh2"),
            "None must not clear an output-active tab"
        );
        assert!(!app.waiting_response.contains("sh3"), "None must not add a tab");
    }

    #[test]
    fn apply_shell_busy_ignores_a_foreground_process_that_never_emitted() {
        // The commit's headline case: a shell with a live foreground command
        // (an open nvim/less/pager) that has produced *no* output — so there is
        // no `last_output_at` entry at all — must not spin. `is_some_and` on the
        // missing entry is false, so `Some(true)` takes the clear branch.
        let mut app = test_app();
        let now = Instant::now();
        assert!(
            !app.last_output_at.contains_key("sh1"),
            "precondition: the shell has never emitted output"
        );
        app.apply_shell_busy(now, &[("sh1".to_string(), Some(true))]);
        assert!(
            !app.waiting_response.contains("sh1"),
            "a foreground-busy shell that never produced output must not spin"
        );
    }

    #[test]
    fn shell_busy_override_does_not_leak_into_attention() {
        // A foreground-busy shell spins (waiting_response) but must never raise the
        // "needs you" flag: attention is output-based, the override is not. This
        // guards the decoupling against a future refactor that reads
        // `waiting_response` from the attention path.
        let mut app = test_app();
        let t = Instant::now();

        // Mark the shell foreground-busy with recent output so the override spins it.
        app.last_output_at.insert("sh1".to_string(), t);
        app.apply_shell_busy(t, &[("sh1".to_string(), Some(true))]);
        assert!(app.waiting_response.contains("sh1"), "shell shows active");

        // The attention path (seq-based) never flags it: with no *unseen* output
        // (seq 0), the override's waiting_response entry stays decoupled.
        let later = t + Duration::from_millis(2000);
        let newly = app.recompute_attention(later, &[("sh1".to_string(), 0)]);
        assert!(
            newly.is_empty(),
            "a foreground-busy shell fires no attention notification"
        );
        assert!(
            !app.attention.contains("sh1"),
            "no 'needs you' dot for a busy shell"
        );
        assert!(app.waiting_response.contains("sh1"), "and it keeps spinning");
    }

    #[test]
    fn attention_latches_on_unseen_quiet_and_sticks() {
        let mut app = test_app(); // focus Tree -> nothing is "viewed"
        let t = Instant::now();

        // Fresh output isn't attention until it has gone quiet (settled).
        app.apply_pane_activity(t, &[("s1".to_string(), 1, None)]);
        let newly = app.recompute_attention(t, &[("s1".to_string(), 1)]);
        assert!(!app.attention.contains("s1"), "fresh output isn't attention yet");
        assert!(newly.is_empty(), "no rising edge before settle");

        // Unseen + quiet past the settle window -> latched (rising edge reported).
        let later = t + Duration::from_millis(3000);
        let newly = app.recompute_attention(later, &[("s1".to_string(), 1)]);
        assert!(app.attention.contains("s1"), "unseen + settled => needs you");
        assert_eq!(newly, vec!["s1".to_string()], "rising edge reported once");

        // Resumed output must NOT clear it (no mid-turn strobe) — only viewing does,
        // and a still-latched session is NOT a rising edge (no repeat notification).
        app.apply_pane_activity(later, &[("s1".to_string(), 2, None)]);
        let newly = app.recompute_attention(later, &[("s1".to_string(), 2)]);
        assert!(
            app.attention.contains("s1"),
            "latched attention survives resumed output"
        );
        assert!(newly.is_empty(), "no repeat rising edge while latched");

        // A gone pane is forgotten.
        app.recompute_attention(later, &[]);
        assert!(app.attention.is_empty());
        assert!(app.viewed_seq.is_empty());
    }

    #[test]
    fn viewing_the_active_tab_clears_its_attention() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("s1", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );
        app.attention.insert("s1".to_string());
        app.focus = Focus::Embedded;

        app.mark_active_viewed();
        assert!(
            !app.attention.contains("s1"),
            "viewing the active tab clears its attention"
        );
        assert!(app.viewed_seq.contains_key("s1"), "viewed_seq is seeded on view");
        assert!(!app.ws_needs_attention("w1"));
    }

    #[test]
    fn attention_clears_per_tab_not_per_workspace() {
        // A workspace stays flagged while a *sibling* tab has unseen output, even
        // while you're viewing another of its tabs — it clears only when you open
        // that specific session.
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("a", &["-c", "sleep 30"]), tab("b", &["-c", "sleep 30"])],
                active: 0, // viewing tab "a"
                last_active: None,
            },
        );
        app.attention.insert("b".to_string()); // sibling tab "b" came back
        app.focus = Focus::Embedded;

        app.mark_active_viewed(); // marks "a" seen, leaves "b" latched
        assert!(app.ws_needs_attention("w1"), "sibling tab keeps the workspace flagged");
        assert!(app.attention.contains("b"));

        // Switch to "b" and view it -> now it clears.
        app.embedded.get_mut("w1").unwrap().active = 1;
        app.mark_active_viewed();
        assert!(!app.ws_needs_attention("w1"), "viewing the sibling clears it");
    }

    #[test]
    fn attention_relatches_after_view_then_new_output() {
        let mut app = test_app(); // focus Tree
        let t = Instant::now();
        app.apply_pane_activity(t, &[("s1".to_string(), 1, None)]);
        app.recompute_attention(t + Duration::from_millis(3000), &[("s1".to_string(), 1)]);
        assert!(app.attention.contains("s1"), "first latch");

        // Simulate viewing it (seen up to seq 1, cleared).
        app.viewed_seq.insert("s1".to_string(), 1);
        app.attention.remove("s1");

        // New output after viewing, still fresh -> not yet.
        let t2 = t + Duration::from_millis(2000);
        app.apply_pane_activity(t2, &[("s1".to_string(), 2, None)]);
        app.recompute_attention(t2, &[("s1".to_string(), 2)]);
        assert!(!app.attention.contains("s1"), "fresh post-view output isn't attention yet");

        // ...once it settles, it re-latches.
        app.recompute_attention(t2 + Duration::from_millis(3000), &[("s1".to_string(), 2)]);
        assert!(app.attention.contains("s1"), "unseen output after a view re-latches");
    }

    #[test]
    fn visible_pane_seq_follows_selection_tab_and_workspace() {
        let mut app = test_app();
        app.workspaces.push(mk_ws("w2", "docs", "r1", None));
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        assert_eq!(app.visible_pane_seq(), None, "no embedded sessions: no visible pane");

        // w1: tab "a" prints (its seq advances), tab "b" stays silent (seq 0).
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![
                    tab("a", &["-c", "printf X; sleep 30"]),
                    tab("b", &["-c", "sleep 30"]),
                ],
                active: 0,
                last_active: None,
            },
        );
        app.embedded.insert(
            "w2".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("c", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.embedded["w1"].tabs[0].pane.output_seq() == 0 {
            assert!(Instant::now() < deadline, "pane never produced output");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Stable now: "X" was the only output, sleep produces nothing more.
        let a_seq = app.embedded["w1"].tabs[0].pane.output_seq();
        assert_eq!(app.visible_pane_seq(), Some(a_seq), "selected ws: the active tab's seq");

        // Selecting another workspace retargets to ITS active tab while w1's
        // is still the noisy one: every wrong candidate differs from the
        // expected value (an always-w1 mutant would return a_seq here).
        app.select_workspace_row("w2");
        assert_eq!(app.visible_pane_seq(), Some(0), "tracks the workspace selection");
        app.select_workspace_row("w1");
        assert_eq!(app.visible_pane_seq(), Some(a_seq), "and back");

        // A tab switch retargets to the newly active (silent) tab.
        app.embedded.get_mut("w1").unwrap().active = 1;
        assert_eq!(app.visible_pane_seq(), Some(0), "tracks the tab switch");
    }

    #[test]
    fn user_caused_output_keeps_seen_session_seen_but_not_unseen() {
        // Hover-scroll provokes child redraws while the tree keeps focus, so
        // mark_active_viewed (Embedded-only) never runs; without the advance
        // in apply_pane_activity the scrolled session would latch a false
        // "needs you" 3s later.
        let mut app = test_app(); // focus Tree
        let t = Instant::now();
        // "seen" is fully viewed at seq 1; "unseen" has a backlog (viewed 0 < 5).
        app.apply_pane_activity(
            t,
            &[("seen".to_string(), 1, None), ("unseen".to_string(), 5, None)],
        );
        app.viewed_seq.insert("seen".to_string(), 1);
        // Both bump their seq within INPUT_GRACE of user input (a wheel tick).
        let t2 = t + Duration::from_millis(100);
        app.apply_pane_activity(
            t2,
            &[("seen".to_string(), 2, Some(t2)), ("unseen".to_string(), 6, Some(t2))],
        );
        assert_eq!(app.viewed_seq.get("seen"), Some(&2), "fully-seen session stays seen");
        assert!(
            !app.viewed_seq.contains_key("unseen"),
            "a backlog is never marked seen by a scroll"
        );
        // After the settle window, only the backlog latches.
        let later = t2 + Duration::from_millis(3000);
        app.recompute_attention(later, &[("seen".to_string(), 2), ("unseen".to_string(), 6)]);
        assert!(!app.attention.contains("seen"), "no false latch for scroll-provoked output");
        assert!(app.attention.contains("unseen"), "genuinely unseen output still latches");
    }

    #[test]
    fn attention_count_shows_in_status_bar() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("s1", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );
        app.attention.insert("s1".to_string());

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("1 waiting"),
            "status bar shows the waiting count:\n{text}"
        );
        assert!(app.ws_needs_attention("w1"), "workspace flagged for attention");
    }

    #[test]
    fn resize_embedded_panes_covers_all_workspaces_and_tabs() {
        let mut app = test_app();
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("a", &["-c", "sleep 30"]), tab("b", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );
        app.embedded.insert(
            "w2".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("c", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );
        // Every pane starts at the tab() helper's 24x80.
        app.resize_embedded_panes(ratatui::layout::Rect::new(31, 2, 50, 20));
        for sessions in app.embedded.values() {
            for t in &sessions.tabs {
                assert_eq!(t.pane.size(), (20, 50), "every tab of every workspace is resized");
            }
        }
    }

    #[tokio::test]
    async fn render_sizes_background_tabs_too() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("a", &["-c", "sleep 30"]), tab("b", &["-c", "sleep 30"])],
                active: 0, // "a" is visible; "b" is a background tab
                last_active: None,
            },
        );
        app.focus = Focus::Embedded;

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();

        let content = pane_content_rect(app.right_pane_area);
        assert!(content.width > 0 && content.height > 0);
        for t in &app.embedded["w1"].tabs {
            assert_eq!(
                t.pane.size(),
                (content.height, content.width),
                "a render sizes background tabs to the pane, not just the active one"
            );
        }
    }

    #[tokio::test]
    async fn branch_status_shows_in_detail_and_tree() {
        let mut app = test_app();
        // Give w1 an own worktree/branch so the status surfaces (test_app's
        // default workspace has no worktree_path). The branch value must be
        // DISTINCT from the workspace name ("ws-one" renders in the tree row,
        // so asserting on it would prove nothing about the detail pane).
        app.workspaces[0].worktree_path = Some("/tmp/alpha".into());
        app.workspaces[0].branch_name = Some("billing-work".into());
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.branch_status.insert(
            "w1".to_string(),
            kommand0_core::BranchStatus {
                branch: Some("billing-work".into()),
                ahead: 2,
                behind: 1,
                dirty: true,
                has_upstream: true,
            },
        );

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);

        // Detail pane.
        assert!(text.contains("Branch:"), "detail shows a Branch line:\n{text}");
        assert!(text.contains("billing-work"), "detail shows the branch name");
        assert!(text.contains("↑2 ↓1"), "detail shows ahead/behind");
        assert!(text.contains("uncommitted changes"), "detail shows dirty state");
        // Tree row segment (compact, no spaces): " ↑2↓1*".
        assert!(text.contains("↑2↓1*"), "tree row shows the compact status segment:\n{text}");
    }

    #[tokio::test]
    async fn pr_status_shows_in_detail_and_tree() {
        let mut app = test_app();
        app.workspaces[0].worktree_path = Some("/tmp/alpha".into());
        app.workspaces[0].branch_name = Some("ws-one".into());
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.pr_status.insert(
            "w1".to_string(),
            kommand0_core::PrStatus {
                number: 42,
                state: kommand0_core::PrState::Open,
                checks: kommand0_core::PrChecks::Passing,
                review: kommand0_core::PrReview::Approved,
                url: "https://github.com/x/y/pull/42".into(),
            },
        );

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);

        // Detail pane: the label line + the URL.
        assert!(
            text.contains("PR #42 · open · CI passing · approved"),
            "detail shows the PR status line:\n{text}"
        );
        assert!(text.contains("pull/42"), "detail shows the PR url:\n{text}");
        // Tree row: assert the passing glyph `✓`, which ONLY the tree row emits
        // (the detail label spells "passing") — so this proves the `#N <glyph>`
        // segment actually reached the row, not just the detail pane.
        assert!(text.contains('\u{2713}'), "tree row shows the passing glyph:\n{text}");
    }

    #[tokio::test]
    async fn c_opens_cleanup_modal_for_own_branch_and_noops_for_fallback() {
        // Own-branch workspace: `c` opens the confirmation modal.
        let mut app = test_app();
        app.workspaces[0].worktree_path = Some("/tmp/alpha".into());
        app.workspaces[0].branch_name = Some("ws-one".into());
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        press(&mut app, KeyCode::Char('c')).await;
        assert!(
            matches!(app.modal, modal::ModalState::ConfirmCleanup { .. }),
            "c opens the cleanup modal"
        );

        // Fallback workspace (no own branch): `c` is a no-op.
        let mut app = test_app(); // w1 has worktree_path: None
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        press(&mut app, KeyCode::Char('c')).await;
        assert!(!app.modal.is_active(), "no cleanup modal for a branchless workspace");
    }

    #[tokio::test]
    async fn cleanup_affordance_and_states_render() {
        let mut app = test_app();
        app.workspaces[0].worktree_path = Some("/tmp/alpha".into());
        app.workspaces[0].branch_name = Some("ws-one".into());
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");

        let draw = |app: &mut App| {
            let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
            t.draw(|frame| render::ui(frame, app)).unwrap();
            buffer_text(&t)
        };

        assert!(draw(&mut app).contains("[Clean up]"), "idle offers the button");

        app.cleanup_inflight.insert("w1".to_string());
        let text = draw(&mut app);
        assert!(text.contains("Cleaning up"), "in-flight shows progress");
        assert!(!text.contains("[Clean up]"), "button hidden while in flight");
        app.cleanup_inflight.remove("w1");

        app.cleanup_result
            .insert("w1".to_string(), "uncommitted changes".to_string());
        let text = draw(&mut app);
        assert!(
            text.contains("Cleanup blocked:") && text.contains("uncommitted"),
            "shows the refusal reason:\n{text}"
        );
    }

    #[tokio::test]
    async fn fallback_workspace_shows_shared_checkout() {
        let mut app = test_app(); // w1 has worktree_path: None
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("shared checkout"),
            "a workspace with no own worktree is labelled shared:\n{text}"
        );
    }

    #[tokio::test]
    async fn render_sizes_panes_even_when_a_repo_row_is_selected() {
        // The resize runs before the right-pane branch, so a live pane is sized
        // even while the selection is a repo row (detail view, no embedded pane
        // shown) — the whole point of "reaches background panes too".
        let mut app = test_app(); // default selection is the first repo row
        assert!(app.selected_workspace().is_none(), "precondition: a repo row is selected");
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("a", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();

        let content = pane_content_rect(app.right_pane_area);
        assert!(content.width > 0 && content.height > 0);
        assert_eq!(
            app.embedded["w1"].tabs[0].pane.size(),
            (content.height, content.width),
            "panes are sized regardless of what's selected"
        );
    }

    #[test]
    fn session_args_assigns_or_resumes_per_kind() {
        // Claude, no prior id: a fresh --session-id is assigned and returned
        // to persist (bare uuid — claude's prefix is empty).
        let (args, minted) = session_args(TabKind::Claude, None);
        assert_eq!(args[0], "--session-id");
        assert_eq!(minted.as_deref(), Some(args[1].as_str()));

        // Claude, prior uuid: resume that exact id (no new id to store).
        let prior = "12345678-1234-4123-8123-123456789abc";
        let (args, minted) = session_args(TabKind::Claude, Some(prior));
        assert_eq!(args, vec!["--resume".to_string(), prior.to_string()]);
        assert!(minted.is_none());

        // Shell: no session args; a fresh tab mints its persisted sentinel,
        // a reopen keeps the prior one.
        let (args, minted) = session_args(TabKind::Shell, None);
        assert!(args.is_empty());
        assert!(minted.unwrap().starts_with("shell:"));
        let (args, minted) = session_args(TabKind::Shell, Some("shell:s1"));
        assert!(args.is_empty() && minted.is_none());
    }

    #[test]
    fn session_args_refuses_a_non_uuid_resume_target() {
        // A hand-edited state entry must not smuggle flags into the argv:
        // anything but a kommand0-minted uuid falls through to a clean fresh
        // spawn instead of `--resume <junk>`.
        for junk in ["--dangerously-skip-permissions", "sess-1"] {
            let (args, minted) = session_args(TabKind::Claude, Some(junk));
            assert_eq!(args[0], "--session-id", "junk id spawns fresh: {args:?}");
            assert!(minted.is_some(), "the fresh mint is returned to persist");
        }
    }

    #[tokio::test]
    async fn rebound_quit_works_and_the_default_key_is_dead() {
        let mut app = test_app();
        let mut cfg = std::collections::HashMap::new();
        cfg.insert("quit".to_string(), vec!["ctrl+q".to_string()]);
        let (km, warns) = keymap::KeyMap::build(&cfg);
        assert!(warns.is_empty(), "{warns:?}");
        app.keymap = km;

        // The default `q` no longer quits.
        let out = handle_key(&mut app, key(KeyCode::Char('q'))).await.unwrap();
        assert_eq!(out, KeyOutcome::Continue);
        // The rebound chord does.
        let out = handle_key(&mut app, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(out, KeyOutcome::Quit);
    }

    #[test]
    fn pick_bin_precedence() {
        // Env wins (tests/e2e rely on this); empty env is ignored.
        assert_eq!(pick_bin(TabKind::Claude, Some("envbin".into()), Some("cfgbin")), "envbin");
        assert_eq!(pick_bin(TabKind::Claude, Some(String::new()), Some("cfgbin")), "cfgbin");
        // No env -> config; nothing -> the kind's tool name.
        assert_eq!(pick_bin(TabKind::Claude, None, Some("cfgbin")), "cfgbin");
        assert_eq!(pick_bin(TabKind::Claude, None, None), "claude");
        // An empty config bin is ignored too (else the spawn would fail on "").
        assert_eq!(pick_bin(TabKind::Claude, None, Some("")), "claude");
        // Shell keeps the same env > config head; its ambient $SHELL then
        // /bin/sh tail stays unasserted (env mutation is unsafe in edition
        // 2024 and racy under the parallel runner).
        assert_eq!(pick_bin(TabKind::Shell, Some("envsh".into()), Some("cfgsh")), "envsh");
        assert_eq!(pick_bin(TabKind::Shell, None, Some("cfgsh")), "cfgsh");
    }

    #[test]
    fn resume_failed_only_for_quick_nonzero_resume() {
        let t = Instant::now();
        let soon = t + Duration::from_millis(500);
        // Resumed pane that died fast with a non-zero code → resume failure.
        assert!(resume_failed(t, true, soon, Some(1)));
        // A clean /exit (code 0) must not be treated as a failure.
        assert!(!resume_failed(t, true, soon, Some(0)));
        // A freshly-created (non-resume) session is never a resume failure.
        assert!(!resume_failed(t, false, soon, Some(1)));
        // Exit past the window (the user used it a while) → not a failure.
        assert!(!resume_failed(t, true, t + Duration::from_millis(3000), Some(1)));
        // Unknown/signal exit (None) → not a failure.
        assert!(!resume_failed(t, true, soon, None));
    }

    fn tab(id: &str, args: &[&str]) -> SessionTab {
        SessionTab {
            id: id.to_string(),
            pane: pane::Pane::spawn("sh", args, std::path::Path::new("/tmp"), 24, 80).unwrap(),
            was_resume: false,
            spawned: Instant::now(),
            kind: TabKind::Claude,
        }
    }

    fn ids(s: &WorkspaceSessions) -> Vec<&str> {
        s.tabs.iter().map(|t| t.id.as_str()).collect()
    }

    fn wait_exit(pane: &mut pane::Pane) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while pane.try_wait().is_none() {
            assert!(Instant::now() < deadline, "pane did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn workspace_sessions_navigation_and_close() {
        let mut s = WorkspaceSessions {
            tabs: vec![
                tab("a", &["-c", "sleep 30"]),
                tab("b", &["-c", "sleep 30"]),
                tab("c", &["-c", "sleep 30"]),
            ],
            active: 0,
            last_active: None,
        };
        s.next();
        assert_eq!(s.active, 1);
        s.next();
        s.next();
        assert_eq!(s.active, 0, "next wraps");
        s.prev();
        assert_eq!(s.active, 2, "prev wraps");
        s.select(1);
        assert_eq!(s.active, 1);
        s.select(9);
        assert_eq!(s.active, 1, "out-of-range select is ignored");

        // Close a tab BELOW the active one: the active tab stays the same one.
        // [a,b,c] active=1(b); remove 0(a) -> [b,c] active=0(b).
        assert!(!s.remove_tab(0));
        assert_eq!(ids(&s), vec!["b", "c"]);
        assert_eq!(s.active, 0);
        // Close the active (last) tab: clamp back onto the remaining one.
        s.active = 1;
        assert!(!s.remove_tab(1));
        assert_eq!(ids(&s), vec!["b"]);
        assert_eq!(s.active, 0);
        // Close the final tab -> empty.
        assert!(s.remove_tab(0));
    }

    #[test]
    fn sync_focus_states_composite_targets_the_active_tab_only() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        let s = WorkspaceSessions {
            tabs: vec![tab("a", &["-c", "sleep 30"]), tab("b", &["-c", "sleep 30"])],
            active: 0,
            last_active: None,
        };
        s.tabs[0].pane.set_focus_reporting(true);
        s.tabs[1].pane.set_focus_reporting(true);
        app.embedded.insert("w1".to_string(), s);

        // Embedded + terminal focused: only the active tab is "focused".
        app.focus = Focus::Embedded;
        app.terminal_focused = true;
        app.sync_focus_states();
        let s = &app.embedded["w1"];
        assert_eq!(s.tabs[0].pane.focus_sent(), Some(true), "active tab gets focus-in");
        assert_eq!(s.tabs[1].pane.focus_sent(), Some(false), "background tab gets focus-out");

        // Back to the tree: the active tab loses focus too.
        app.focus = Focus::Tree;
        app.sync_focus_states();
        assert_eq!(app.embedded["w1"].tabs[0].pane.focus_sent(), Some(false));

        // Terminal blur while embedded stays unfocused; regaining the
        // terminal restores the active tab.
        app.focus = Focus::Embedded;
        app.terminal_focused = false;
        app.sync_focus_states();
        assert_eq!(app.embedded["w1"].tabs[0].pane.focus_sent(), Some(false));
        app.terminal_focused = true;
        app.sync_focus_states();
        assert_eq!(app.embedded["w1"].tabs[0].pane.focus_sent(), Some(true));
    }

    #[test]
    fn last_active_tab_chord_toggles_and_degrades() {
        let mut s = WorkspaceSessions {
            tabs: vec![
                tab("a", &["-c", "sleep 30"]),
                tab("b", &["-c", "sleep 30"]),
                tab("c", &["-c", "sleep 30"]),
            ],
            active: 0,
            last_active: None,
        };
        // Fresh pane: no history — no-op.
        s.select_last_active();
        assert_eq!(s.active, 0);
        // a -> b, then last-active returns to a, and again toggles back to b.
        s.select(1);
        s.select_last_active();
        assert_eq!(s.tabs[s.active].id, "a", "returns to the tab we left");
        s.select_last_active();
        assert_eq!(s.tabs[s.active].id, "b", "toggles a <-> b");
        // Close the remembered tab: the dangling id degrades to a no-op.
        s.select(2); // now on c; last_active = b
        let b_idx = s.tabs.iter().position(|t| t.id == "b").unwrap();
        assert!(!s.remove_tab(b_idx));
        s.select_last_active();
        assert_eq!(s.tabs[s.active].id, "c", "closed last-active tab: no-op");
    }

    use ratatui::crossterm::event::{MouseButton, MouseEventKind};

    fn m(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    /// `m` with SHIFT held: the shift+wheel tab-switch gesture.
    fn ms(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::SHIFT }
    }

    /// App with workspace w1 selected and one embedded tab running `cmd`;
    /// right pane at (30,0) 70x30 → content cells start at screen (31, 2).
    fn app_with_pane(cmd: &str) -> App {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.right_pane_area = ratatui::layout::Rect::new(30, 0, 70, 30);
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("a", &["-c", cmd])],
                active: 0,
                last_active: None,
            },
        );
        app
    }

    #[test]
    fn pane_drag_selects_and_copies_for_non_mouse_child() {
        let mut app = app_with_pane("printf 'HELLO WORLD'; sleep 30");
        let deadline = Instant::now() + Duration::from_secs(3);
        // Poll for the END of the line: the later assertions need the whole
        // of "HELLO WORLD" on screen, and a split PTY chunk could deliver
        // "HELLO" first.
        while !app.embedded["w1"].tabs[0].pane.screen_contents().contains("WORLD") {
            assert!(Instant::now() < deadline, "stub never printed");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut out: Vec<u8> = Vec::new();
        // Down at screen (31, 2) = pane cell (row 0, col 0) — the (col, row)
        // → (row, col) swap; expectations written in (row, col).
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Down(MouseButton::Left), 31, 2),
            &mut out
        ));
        assert_eq!(app.focus, Focus::Embedded, "down focuses the pane");
        let sel = app.pane_selection.expect("anchored");
        assert_eq!(sel.anchor, (0, 0), "(row, col) convention");
        assert!(sel.dragging);
        // Drag to screen (35, 2) = cell (0, 4): inclusive head over "HELLO".
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Drag(MouseButton::Left), 35, 2),
            &mut out
        ));
        assert_eq!(app.pane_selection.unwrap().head, (0, 4));
        assert!(out.is_empty(), "no copy before release");
        // Up: OSC 52 copy of exactly the dragged cells.
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Up(MouseButton::Left), 35, 2),
            &mut out
        ));
        assert_eq!(out, pane::encode_osc52_copy("HELLO"), "copied the dragged text");
        let sel = app.pane_selection.expect("highlight persists after Up");
        assert!(!sel.dragging);

        // A drag ending on empty rows below the text: the extraction carries
        // trailing \n per empty row, but the clipboard payload is trimmed
        // (pasting into a shell would otherwise execute the line).
        let mut out: Vec<u8> = Vec::new();
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Down(MouseButton::Left), 31, 2),
            &mut out
        ));
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Drag(MouseButton::Left), 35, 4),
            &mut out
        ));
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Up(MouseButton::Left), 35, 4),
            &mut out
        ));
        assert_eq!(
            out,
            pane::encode_osc52_copy("HELLO WORLD"),
            "trailing empty rows trimmed from the payload"
        );
    }

    #[test]
    fn pane_drag_forwards_untouched_for_mouse_mode_child() {
        let mut app = app_with_pane("printf '\\033[?1002hM'; sleep 30");
        wait_wants_mouse(&app);
        let mut out: Vec<u8> = Vec::new();
        assert!(
            !mouse::handle_selection(
                &mut app,
                m(MouseEventKind::Down(MouseButton::Left), 31, 2),
                &mut out
            ),
            "mouse-mode child: not consumed — routes/forwards exactly as today"
        );
        assert!(app.pane_selection.is_none());
        assert!(out.is_empty());
    }

    #[test]
    fn click_without_drag_copies_nothing() {
        let mut app = app_with_pane("printf X; sleep 30");
        let mut out: Vec<u8> = Vec::new();
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Down(MouseButton::Left), 31, 2),
            &mut out
        ));
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Up(MouseButton::Left), 31, 2),
            &mut out
        ));
        assert!(out.is_empty(), "plain click never touches the clipboard");
        assert!(app.pane_selection.is_none(), "no lingering highlight");
        assert_eq!(app.focus, Focus::Embedded, "click-to-focus preserved");
    }

    #[test]
    fn drag_over_empty_rows_copies_nothing() {
        // The trim guard is all that prevents emitting `\x1b]52;c;\x07`,
        // which would CLEAR the user's clipboard.
        let mut app = app_with_pane("printf 'HELLO WORLD'; sleep 30");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !app.embedded["w1"].tabs[0].pane.screen_contents().contains("WORLD") {
            assert!(Instant::now() < deadline, "stub never printed");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut out: Vec<u8> = Vec::new();
        // Down at screen (31, 12) = pane cell (row 10, col 0) — an ASYMMETRIC
        // anchor, so a (col, row) transposition at the Down site cannot pass.
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Down(MouseButton::Left), 31, 12),
            &mut out
        ));
        assert_eq!(app.pane_selection.unwrap().anchor, (10, 0), "asymmetric (row, col) anchor");
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Drag(MouseButton::Left), 35, 14),
            &mut out
        ));
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Up(MouseButton::Left), 35, 14),
            &mut out
        ));
        assert!(out.is_empty(), "empty-rows selection trims to nothing: no OSC 52 at all");
        let sel = app.pane_selection.expect("trimmed-to-empty keeps the highlight");
        assert!(!sel.dragging);
    }

    #[test]
    fn down_outside_pane_clears_lingering_highlight() {
        let mut app = app_with_pane("printf X; sleep 30");
        // A Left Down on the tree clears a lingering highlight and routes on.
        app.pane_selection =
            Some(mouse::PaneSelection { anchor: (0, 0), head: (0, 3), dragging: false });
        let mut out: Vec<u8> = Vec::new();
        assert!(
            !mouse::handle_selection(
                &mut app,
                m(MouseEventKind::Down(MouseButton::Left), 5, 5),
                &mut out
            ),
            "tree click is not consumed"
        );
        assert!(app.pane_selection.is_none(), "clear-on-mouse-down is unconditional");
        // A non-left Down clears too (and never anchors).
        app.pane_selection =
            Some(mouse::PaneSelection { anchor: (0, 0), head: (0, 3), dragging: false });
        assert!(!mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Down(MouseButton::Middle), 31, 2),
            &mut out
        ));
        assert!(app.pane_selection.is_none(), "middle-button down clears");
        assert!(out.is_empty());
    }

    #[test]
    fn scroll_mid_drag_ends_grab_and_routes_on() {
        let mut app = app_with_pane("printf X; sleep 30");
        let mut out: Vec<u8> = Vec::new();
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Down(MouseButton::Left), 31, 2),
            &mut out
        ));
        assert!(mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Drag(MouseButton::Left), 35, 2),
            &mut out
        ));
        // A stray wheel event ends the grab and is NOT consumed.
        assert!(!mouse::handle_selection(&mut app, m(MouseEventKind::ScrollUp, 35, 2), &mut out));
        let sel = app.pane_selection.expect("highlight survives the stray event");
        assert!(!sel.dragging, "the grab ended");
        // With the grab gone, a further Drag routes normally (not consumed).
        assert!(!mouse::handle_selection(
            &mut app,
            m(MouseEventKind::Drag(MouseButton::Left), 40, 2),
            &mut out
        ));
        assert!(out.is_empty(), "nothing was ever copied");
    }

    /// Poll until the child has enabled mouse reporting (the reader thread
    /// applies the `?100xh` opt-in asynchronously).
    fn wait_wants_mouse(app: &App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !app.embedded["w1"].tabs[0].pane.wants_mouse() {
            assert!(Instant::now() < deadline, "child never enabled mouse mode");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn scroll_over_content_without_a_session_is_a_noop() {
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);
        app.right_pane_area = ratatui::layout::Rect::new(30, 0, 70, 30);
        let before = app.selected_index;
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollDown, 50, 10));
        assert_eq!(app.focus, Focus::Tree, "no pane: focus stays on the tree");
        assert_eq!(app.selected_index, before, "tree selection unchanged");
    }

    #[test]
    fn scroll_over_content_with_mouseless_child_sends_nothing() {
        // The forward branch is taken, but send_mouse gates on the child's
        // mouse mode: a plain sleep never opted in, so nothing is written.
        let mut app = app_with_pane("sleep 30");
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollDown, 50, 10));
        assert!(app.embedded["w1"].tabs[0].pane.last_input_at().is_none());
        assert_eq!(app.focus, Focus::Tree);
    }

    #[test]
    fn hover_scroll_forwards_to_pane_without_focusing() {
        let mut app = app_with_pane("printf '\\033[?1000h'; sleep 30");
        wait_wants_mouse(&app);
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);
        let before = app.selected_index;
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollDown, 50, 10));
        assert!(
            app.embedded["w1"].tabs[0].pane.last_input_at().is_some(),
            "the wheel tick reached the child"
        );
        assert_eq!(app.focus, Focus::Tree, "hover-scroll must not steal focus");
        assert_eq!(app.selected_index, before, "tree selection unchanged");
    }

    #[test]
    fn scroll_over_tree_navigates_even_with_mousey_pane() {
        // Tree priority: the wheel over the tree keeps navigating rows even
        // while a mouse-mode pane is live; nothing leaks to the child.
        let mut app = app_with_pane("printf '\\033[?1000h'; sleep 30");
        wait_wants_mouse(&app);
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);
        let before = app.selected_index;
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollUp, 5, 3));
        assert_ne!(app.selected_index, before, "tree scroll still navigates");
        assert!(app.embedded["w1"].tabs[0].pane.last_input_at().is_none());
    }

    #[test]
    fn scroll_on_pane_border_forwards_nothing() {
        // (30, 5) is the right pane's left border: translate_mouse rejects it,
        // so even a mouse-mode child receives nothing.
        let mut app = app_with_pane("printf '\\033[?1000h'; sleep 30");
        wait_wants_mouse(&app);
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollUp, 30, 5));
        assert!(app.embedded["w1"].tabs[0].pane.last_input_at().is_none());
    }

    #[test]
    fn hover_tilt_switches_tabs_with_wrap() {
        let mut app = app_with_pane("sleep 30");
        let s = app.embedded.get_mut("w1").unwrap();
        s.tabs.push(tab("b", &["-c", "sleep 30"]));
        s.tabs.push(tab("c", &["-c", "sleep 30"]));
        app.pane_selection =
            Some(mouse::PaneSelection { anchor: (0, 0), head: (0, 3), dragging: false });
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollRight, 50, 10));
        assert_eq!(app.embedded["w1"].active, 1, "tilt right = next tab");
        assert_eq!(app.focus, Focus::Tree, "the hover gesture must not steal focus");
        assert!(app.pane_selection.is_none(), "a tab switch drops the highlight");
        // The wheel switch went through switch_to, so Ctrl+A l returns to the
        // tab we left, exactly like the key route.
        app.embedded.get_mut("w1").unwrap().select_last_active();
        assert_eq!(app.embedded["w1"].active, 0, "last-active bookkeeping holds");
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollLeft, 50, 10));
        assert_eq!(app.embedded["w1"].active, 2, "tilt left = prev, wrapping at the start");
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollRight, 50, 10));
        assert_eq!(app.embedded["w1"].active, 0, "next wraps at the end");
    }

    #[test]
    fn hover_shift_wheel_switches_instead_of_forwarding() {
        // Tab a is a mouse-mode child: without the gesture a shifted wheel at
        // a content cell would hover-forward to it. Pins the one deliberately
        // removed behavior (a shifted wheel no longer hover-forwards to a
        // mouse-mode child) AND re-pins that the unmodified wheel still
        // forwards. Three tabs, not two: with two, next and prev are congruent
        // mod 2 and a shift-arm direction swap would pass unnoticed.
        let mut app = app_with_pane("printf '\\033[?1000h'; sleep 30");
        wait_wants_mouse(&app);
        app.embedded.get_mut("w1").unwrap().tabs.push(tab("b", &["-c", "sleep 30"]));
        app.embedded.get_mut("w1").unwrap().tabs.push(tab("c", &["-c", "sleep 30"]));
        mouse::handle_mouse(&mut app, ms(MouseEventKind::ScrollDown, 50, 10));
        assert_eq!(app.embedded["w1"].active, 1, "shift+down = next tab");
        assert!(
            app.embedded["w1"].tabs[0].pane.last_input_at().is_none(),
            "the shifted wheel was intercepted, not forwarded"
        );
        mouse::handle_mouse(&mut app, ms(MouseEventKind::ScrollUp, 50, 10));
        assert_eq!(app.embedded["w1"].active, 0, "shift+up = prev tab");
        assert!(app.embedded["w1"].tabs[0].pane.last_input_at().is_none());
        // Control half: the unmodified wheel still hover-forwards to the child.
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollDown, 50, 10));
        assert!(
            app.embedded["w1"].tabs[0].pane.last_input_at().is_some(),
            "the unmodified vertical wheel still forwards to the content"
        );
    }

    #[test]
    fn gesture_outside_right_pane_is_inert() {
        let mut app = app_with_pane("sleep 30");
        app.embedded.get_mut("w1").unwrap().tabs.push(tab("b", &["-c", "sleep 30"]));
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollRight, 5, 3)); // tree pane
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollRight, 50, 35)); // below both panes
        assert_eq!(app.embedded["w1"].active, 0, "a tilt outside the right pane never flips tabs");
    }

    #[test]
    fn shift_scroll_over_tree_still_navigates() {
        let mut app = app_with_pane("sleep 30");
        app.embedded.get_mut("w1").unwrap().tabs.push(tab("b", &["-c", "sleep 30"]));
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);
        let before = app.selected_index;
        mouse::handle_mouse(&mut app, ms(MouseEventKind::ScrollUp, 5, 3));
        assert_ne!(app.selected_index, before, "shift+wheel over the tree still navigates");
        mouse::handle_mouse(&mut app, ms(MouseEventKind::ScrollDown, 5, 3));
        assert_eq!(app.selected_index, before, "both shifted directions navigate");
        assert_eq!(app.embedded["w1"].active, 0, "tree-area shift never flips tabs");
    }

    #[test]
    fn focused_tilt_intercepts_before_child() {
        let mut app = app_with_pane("printf '\\033[?1000h'; sleep 30");
        wait_wants_mouse(&app);
        app.embedded.get_mut("w1").unwrap().tabs.push(tab("b", &["-c", "sleep 30"]));
        app.focus = Focus::Embedded;
        app.embedded_prefix = true; // a half-typed Ctrl+A, mid-gesture
        // (50, 10) is a CONTENT cell (row >= y+2): a forwarded event would
        // pass translate_mouse, so a None below proves interception, not
        // strip/border rejection.
        app.handle_embedded_mouse(m(MouseEventKind::ScrollRight, 50, 10));
        assert_eq!(app.embedded["w1"].active, 1, "focused tilt switches tabs");
        assert!(
            app.embedded["w1"].tabs[0].pane.last_input_at().is_none(),
            "the tilt was intercepted before the mouse-mode child"
        );
        assert!(!app.embedded_prefix, "a tab switch drops the half-typed prefix");
        app.handle_embedded_mouse(ms(MouseEventKind::ScrollDown, 50, 10));
        assert_eq!(app.embedded["w1"].active, 0, "focused shift+down = next, wrapping");
        assert!(app.embedded["w1"].tabs[0].pane.last_input_at().is_none());
    }

    #[test]
    fn single_tab_wheel_keeps_last_active_none() {
        let mut app = app_with_pane("sleep 30");
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollRight, 50, 10));
        assert_eq!(app.embedded["w1"].active, 0);
        assert!(
            app.embedded["w1"].last_active.is_none(),
            "a single-tab wheel must not clobber Ctrl+A l history"
        );
    }

    #[test]
    fn wheel_with_no_sessions_is_safe() {
        // The horizontal sibling of scroll_over_content_without_a_session_is_a_noop.
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.pane_areas.tree = ratatui::layout::Rect::new(0, 0, 30, 8);
        app.right_pane_area = ratatui::layout::Rect::new(30, 0, 70, 30);
        let before = app.selected_index;
        mouse::handle_mouse(&mut app, m(MouseEventKind::ScrollRight, 50, 10));
        assert_eq!(app.focus, Focus::Tree, "no sessions: focus untouched");
        assert_eq!(app.selected_index, before, "tree selection unchanged");
    }

    #[test]
    fn reap_clears_pane_selection() {
        let mut app = test_app(); // has workspace "w1"
        let mut s = WorkspaceSessions {
            tabs: vec![tab("a", &["-c", "sleep 30"])],
            active: 0,
            last_active: None,
        };
        s.tabs[0].pane.kill();
        wait_exit(&mut s.tabs[0].pane);
        app.embedded.insert("w1".to_string(), s);
        app.pane_selection =
            Some(mouse::PaneSelection { anchor: (0, 0), head: (1, 2), dragging: false });
        app.reap_embedded(Instant::now());
        assert!(app.pane_selection.is_none(), "a reaped tab clears the highlight");
    }

    #[tokio::test]
    async fn key_clears_pane_selection() {
        let mut app = test_app();
        app.pane_selection =
            Some(mouse::PaneSelection { anchor: (0, 0), head: (0, 3), dragging: false });
        press(&mut app, KeyCode::Char('j')).await;
        assert!(app.pane_selection.is_none(), "any key dismisses the highlight");
    }

    #[test]
    fn reap_drops_exited_tab_and_keeps_active_by_identity() {
        let mut app = test_app(); // has workspace "w1"
        let mut s = WorkspaceSessions {
            tabs: vec![
                tab("a", &["-c", "sleep 30"]),
                tab("b", &["-c", "sleep 30"]),
                tab("c", &["-c", "sleep 30"]),
            ],
            active: 2, // active on "c"
            last_active: None,
        };
        s.tabs[0].pane.kill(); // a non-active tab exits
        wait_exit(&mut s.tabs[0].pane);
        app.embedded.insert("w1".to_string(), s);

        app.reap_embedded(Instant::now());
        let s = &app.embedded["w1"];
        assert_eq!(ids(s), vec!["b", "c"], "exited tab a dropped");
        assert_eq!(s.tabs[s.active].id, "c", "active stays on c by identity");
    }

    #[tokio::test]
    async fn embed_error_banner_renders_over_a_live_pane() {
        // The detail-pane error surface is unreachable while embedded; the error
        // must show in the embedded view (here, the bottom border banner).
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        // tree: [Repo alpha, Workspace ws-one (w1), Repo beta]; select w1.
        app.selected_index = 1;
        let s = WorkspaceSessions {
            tabs: vec![tab("a", &["-c", "sleep 30"])],
            active: 0,
            last_active: None,
        };
        app.embedded.insert("w1".to_string(), s);
        app.focus = Focus::Embedded;
        app.embed_error = Some(("w1".to_string(), "BOOM-ERR".to_string()));

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("BOOM-ERR"),
            "error banner should render over the embedded pane:\n{text}"
        );
    }

    #[test]
    fn reap_resume_failure_affects_only_the_failed_tab() {
        // A resume failure must only ever touch the tab that failed: the gone id
        // is forgotten and a healthy sibling is never collateral. The in-place
        // heal itself (spawning a fresh session into the slot) is covered
        // end-to-end by the `stale_resume_auto_heals_to_a_fresh_session` e2e,
        // which can guarantee a real bin in a real cwd; here the fresh spawn's
        // success is environment-dependent, so we assert only what holds in both
        // the healed and the drop-fallback branch.
        let mut app = test_app();
        app.state.add_embedded_session("w1", "a");
        app.state.add_embedded_session("w1", "b");
        // "a" is a resume that exits non-zero at once (its session was purged);
        // "b" is a healthy resumed session that keeps running.
        let mut s = WorkspaceSessions {
            tabs: vec![
                SessionTab {
                    id: "a".to_string(),
                    pane: pane::Pane::spawn(
                        "sh",
                        &["-c", "exit 1"],
                        std::path::Path::new("/tmp"),
                        24,
                        80,
                    )
                    .unwrap(),
                    was_resume: true,
                    spawned: Instant::now(),
                    kind: TabKind::Claude,
                },
                tab("b", &["-c", "sleep 30"]),
            ],
            active: 0,
            last_active: None,
        };
        s.tabs[1].was_resume = true;
        wait_exit(&mut s.tabs[0].pane);
        app.embedded.insert("w1".to_string(), s);

        app.reap_embedded(Instant::now());
        // "a" is always forgotten; "b" is always preserved (never collateral).
        let persisted = app.state.embedded_session_ids("w1");
        assert!(persisted.contains(&"b".to_string()), "healthy id kept: {persisted:?}");
        assert!(!persisted.contains(&"a".to_string()), "gone id forgotten: {persisted:?}");

        let s = &app.embedded["w1"];
        let tab_ids = ids(s);
        assert!(tab_ids.contains(&"b"), "healthy tab kept: {tab_ids:?}");
        assert!(!tab_ids.contains(&"a"), "failed tab gone (healed or dropped): {tab_ids:?}");
        assert!(s.active < s.tabs.len(), "active stays in range");
    }

    #[tokio::test]
    async fn heal_resume_keeps_the_tab_position_in_persisted_order() {
        // The heal replaces the runtime tab in its slot; the persisted entry must
        // stay in the SAME slot too, or the row reopens out of order next time.
        let mut app = test_app();
        app.config.claude_bin = Some("sh".to_string()); // the fresh spawn must succeed
        for id in ["a", "b", "c"] {
            app.state.add_embedded_session("w1", id);
        }
        app.state.set_embedded_session_title("w1", "b", "build");
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![
                    tab("a", &["-c", "sleep 30"]),
                    tab("b", &["-c", "sleep 30"]),
                    tab("c", &["-c", "sleep 30"]),
                ],
                active: 0,
                last_active: None,
            },
        );

        assert!(
            app.heal_resume(TabKind::Claude, "w1", "b", "/tmp", Instant::now()),
            "fresh spawn succeeded"
        );

        let persisted = app.state.embedded_session_ids("w1").to_vec();
        assert_eq!(persisted.len(), 3, "still three entries: {persisted:?}");
        assert_eq!(persisted[0], "a");
        assert_eq!(persisted[2], "c");
        let new_id = &persisted[1];
        assert_ne!(new_id.as_str(), "b", "the gone id was replaced, in its own slot");
        assert_eq!(app.state.embedded_session_title("w1", "b"), None, "b's title is gone");
        assert_eq!(
            app.state.embedded_session_title("w1", new_id),
            Some("build"),
            "the replacement carries b's title"
        );
    }

    #[test]
    fn resume_miss_scan_is_paced_not_per_tick() {
        // The miss scan serializes the pane grid under the reader's parser
        // mutex, so it must run on the RESUME_MISS_SCAN_EVERY stride, not
        // every 50ms tick (exit reaping is untouched by the pacing).
        let mut app = test_app();
        let mut s = WorkspaceSessions {
            tabs: vec![tab("a", &["-c", &format!("printf '{RESUME_MISS_MARKER} a'; sleep 30")])],
            active: 0,
            last_active: None,
        };
        s.tabs[0].was_resume = true;
        app.embedded.insert("w1".to_string(), s);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !app.embedded["w1"].tabs[0].pane.screen_contents().contains(RESUME_MISS_MARKER) {
            assert!(Instant::now() < deadline, "pane never showed the miss marker");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Pin the stride start so the poll above can't eat the window.
        let t = Instant::now();
        app.last_resume_scan = t;
        app.reap_embedded(t);
        assert!(
            ids(&app.embedded["w1"]).contains(&"a"),
            "within the stride the screen is not scanned, the missed tab survives"
        );
        // At exactly the stride boundary the scan runs and the miss is
        // healed (fresh id) or dropped; either way "a" is gone.
        app.reap_embedded(t + RESUME_MISS_SCAN_EVERY);
        let gone = app.embedded.get("w1").map(|s| !ids(s).contains(&"a")).unwrap_or(true);
        assert!(gone, "past the stride the scan heals/drops the missed resume");
    }

    #[tokio::test]
    async fn ctrl_a_r_renames_active_tab_and_empty_clears() {
        let mut app = test_app();
        // Select the workspace row and give it one live embedded session tab.
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.state.add_embedded_session("w1", "sess-1");
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("sess-1", &["-c", "sleep 30"])],
                active: 0,
                last_active: None,
            },
        );
        app.focus = Focus::Embedded;

        // Ctrl+A r opens the rename modal; typing goes to it (not claude) because
        // the embedded block yields once a modal is active.
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        press(&mut app, KeyCode::Char('r')).await;
        assert!(app.modal.is_active(), "rename modal opened");
        for c in "auth".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        press(&mut app, KeyCode::Enter).await;
        assert!(!app.modal.is_active(), "modal closed on submit");
        assert_eq!(app.state.embedded_session_title("w1", "sess-1"), Some("auth"));
        // The typed name went to the modal, not the pane behind it.
        let pane_text = app.embedded["w1"].tabs[0].pane.screen_contents();
        assert!(!pane_text.contains("auth"), "rename input must not leak to claude");

        // The title renders in the tab strip.
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        assert!(
            buffer_text(&terminal).contains("auth"),
            "tab strip shows the title"
        );

        // Reopening prefills the current name; clearing it resets to the number.
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        press(&mut app, KeyCode::Char('r')).await;
        match &app.modal {
            modal::ModalState::RenameSession { input, .. } => assert_eq!(input, "auth"),
            _ => panic!("expected the rename modal prefilled with the current title"),
        }
        for _ in 0..4 {
            press(&mut app, KeyCode::Backspace).await;
        }
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.state.embedded_session_title("w1", "sess-1"), None);
    }

    #[tokio::test]
    async fn shell_tabs_can_be_renamed_and_title_persists() {
        // Shell tabs have stable persisted ids now, so the old "can't be
        // renamed" guard is gone and the rename pipeline is kind-agnostic.
        let mut app = test_app();
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.state.add_embedded_session("w1", "shell:s1");
        let mut sh = tab("shell:s1", &["-c", "sleep 30"]);
        sh.kind = TabKind::Shell;
        app.embedded.insert(
            "w1".to_string(),
            WorkspaceSessions { tabs: vec![sh], active: 0, last_active: None },
        );
        app.focus = Focus::Embedded;

        handle_key(&mut app, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        press(&mut app, KeyCode::Char('r')).await;
        assert!(app.modal.is_active(), "the rename modal opens for a shell tab");
        assert!(app.embed_error.is_none(), "no 'Shell tabs can't be renamed.' error");
        for c in "build".chars() {
            press(&mut app, KeyCode::Char(c)).await;
        }
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(app.state.embedded_session_title("w1", "shell:s1"), Some("build"));
    }

    #[tokio::test]
    async fn renders_help_overlay_after_question_mark() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?')).await;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Help"), "help overlay should render:\n{text}");
        assert!(
            text.contains("Move down"),
            "help should list bindings from the keymap:\n{text}"
        );
        // The embedded section sits below the fold at 30 rows — render taller
        // to bring it into the popup.
        let mut tall = Terminal::new(TestBackend::new(100, 80)).unwrap();
        tall.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&tall);
        assert!(
            text.contains("Alt+Enter"),
            "help should document the newline chord for embedded sessions:\n{text}"
        );
    }
}
