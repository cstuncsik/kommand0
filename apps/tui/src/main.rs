mod buttons;
mod help;
mod modal;
mod mouse;
// PTY-passthrough embedded `claude` pane — the app's only session view. The
// module exposes a small terminal API (resize/blit/send/…); a few accessors are
// kept for tests and future wiring, hence the module-level allow.
#[allow(dead_code)]
mod pane;
mod render;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{EventStream, KeyEvent, KeyEventKind};
use futures::{FutureExt, StreamExt};
use kommand0_core::{AppState, Config, RepoEntry, SessionStatus, Workspace};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{Event, KeyCode, KeyModifiers, MouseEvent},
};

#[allow(dead_code)]
pub(crate) enum Status {
    Idle,
    Done,
    Error(String),
}

/// Decide the `claude` CLI args for opening a workspace's embedded pane.
///
/// If the workspace already has a stored session id, resume it; otherwise assign
/// a fresh UUID. Returns `(args, new_session_id)` where `new_session_id` is
/// `Some` only when a new session was created (and should be persisted on a
/// successful spawn).
///
/// Known edge: if the very first launch is abandoned before any turn, the id is
/// still persisted but no conversation exists on disk. Reopening then runs
/// `--resume <id>`, which `claude` rejects ("No conversation found") and exits
/// non-zero — caught by [`resume_failed`], which forgets the id so the next open
/// starts fresh. So the worst case self-heals in one reopen.
/// Height of the session tab strip at the top of the right pane.
const TAB_BAR_HEIGHT: u16 = 1;

/// Maximum session tabs per workspace (keeps single-digit `1`–`9` shortcuts and
/// single-column tab labels).
pub(crate) const MAX_SESSION_TABS: usize = 9;

/// How often the periodic git-status refresh runs (off the render loop). Git
/// status is cheap but can stall on large/cold worktrees, so it never runs on
/// the 50ms tick; on-demand triggers (workspace create/close) keep it snappy.
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

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

/// Carries a PR-open result `(workspace_id, Ok(url) | Err(msg))` to the event
/// loop, sending on drop so a worker panic still clears `pr_inflight`.
struct PrOpenGuard {
    tx: tokio::sync::mpsc::UnboundedSender<(String, Result<String, String>)>,
    payload: Option<(String, Result<String, String>)>,
}

impl Drop for PrOpenGuard {
    fn drop(&mut self) {
        if let Some(p) = self.payload.take() {
            let _ = self.tx.send(p);
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
    "Couldn't resume the previous Claude session (it may have been cleared) — reopen to start fresh.";

/// A resumed tab still showing the resume-miss marker within this window of spawn
/// is a genuine miss (claude prints it at startup). After the window we stop
/// scanning so a live session's own output can't false-positive.
const RESUME_CHECK_WINDOW: Duration = Duration::from_secs(8);

fn claude_args(resume_id: Option<&str>) -> (Vec<String>, Option<String>) {
    match resume_id {
        Some(id) => (vec!["--resume".to_string(), id.to_string()], None),
        None => {
            let uuid = AppState::new_claude_session_id();
            (vec!["--session-id".to_string(), uuid.clone()], Some(uuid))
        }
    }
}

/// Resolve the `claude` binary: `KOMMAND0_CLAUDE_BIN` env (used by tests and
/// ad-hoc overrides) wins, then the config's `claude_bin`, then `claude`.
fn pick_claude_bin(env_bin: Option<String>, config_bin: Option<&str>) -> String {
    env_bin
        .filter(|s| !s.is_empty())
        .or_else(|| config_bin.map(str::to_string))
        .unwrap_or_else(|| "claude".to_string())
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
        #[allow(dead_code)]
        workspace_count: usize,
    },
    Workspace {
        ws: Workspace,
        repo_name: String,
    },
    Hint {
        text: String,
    },
}

/// One Claude session tab within a workspace: a live PTY pane plus the metadata
/// to persist/resume it and to detect a failed resume.
pub(crate) struct SessionTab {
    /// Claude session id (UUID) — also the stable key for activity tracking.
    pub(crate) id: String,
    pub(crate) pane: pane::Pane,
    was_resume: bool,
    spawned: Instant,
}

/// A workspace's open session tabs (tab order = creation order) and the active
/// tab. Kept in `App.embedded` only while non-empty.
#[derive(Default)]
pub(crate) struct WorkspaceSessions {
    pub(crate) tabs: Vec<SessionTab>,
    pub(crate) active: usize,
}

impl WorkspaceSessions {
    pub(crate) fn active_pane_mut(&mut self) -> Option<&mut pane::Pane> {
        self.tabs.get_mut(self.active).map(|t| &mut t.pane)
    }
    pub(crate) fn active_tab(&self) -> Option<&SessionTab> {
        self.tabs.get(self.active)
    }
    fn push(&mut self, tab: SessionTab) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }
    fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }
    fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }
    fn select(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
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

pub(crate) struct App {
    pub(crate) repos: Vec<RepoEntry>,
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) state: AppState,
    pub(crate) expanded: HashSet<String>,
    pub(crate) tree_items: Vec<TreeNode>,
    pub(crate) selected_index: usize,
    #[allow(dead_code)]
    pub(crate) status: Status,

    pub(crate) focus: Focus,

    // UX state
    pub(crate) show_help: bool,
    pub(crate) help_scroll: u16,
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
    /// Sessions that produced unseen output and then went quiet — they "need
    /// you". A latched set: a session stays here until the user views it (or its
    /// pane is gone), so a mid-turn pause that resumes can't strobe it on/off.
    pub(crate) attention: HashSet<String>,
    pub(crate) pane_areas: mouse::PaneAreas,
    pub(crate) mouse_pos: Option<(u16, u16)>,
    pub(crate) hit_regions: Vec<buttons::HitRegion>,
    pub(crate) pending_button_action: Option<buttons::HitAction>,
    pub(crate) modal: modal::ModalState,
    pub(crate) expanded_icon_rows: HashSet<String>,
    pub(crate) last_pane_width: u16,
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

    /// Workspaces with a PR-open in flight (gates re-triggering; shows progress).
    pub(crate) pr_inflight: HashSet<String>,
    /// Last PR-open outcome per workspace: `Ok(url)` or `Err(message)`.
    pub(crate) pr_result: HashMap<String, Result<String, String>>,
    /// PR worker → event-loop channel carrying `(workspace_id, result)`.
    pr_tx: Option<tokio::sync::mpsc::UnboundedSender<(String, Result<String, String>)>>,

    /// Workspaces with a cleanup in flight (gates re-triggering; shows progress).
    pub(crate) cleanup_inflight: HashSet<String>,
    /// Last cleanup *failure* per workspace (a success deletes the workspace).
    pub(crate) cleanup_result: HashMap<String, String>,
    /// Cleanup worker → event-loop channel carrying `(workspace_id, result)`.
    cleanup_tx: Option<tokio::sync::mpsc::UnboundedSender<(String, Result<(), String>)>>,

    /// User config (claude passthrough + tunables), loaded once at startup.
    pub(crate) config: Config,
}

impl App {
    fn new(state: AppState) -> Self {
        let repos = state.repos.clone();
        let workspaces = state.workspaces.clone();

        let mut app = Self {
            repos,
            workspaces,
            state,
            expanded: HashSet::new(),
            tree_items: Vec::new(),
            selected_index: 0,
            status: Status::Idle,
            focus: Focus::Tree,
            show_help: false,
            help_scroll: 0,
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
            attention: HashSet::new(),
            pane_areas: mouse::PaneAreas::default(),
            mouse_pos: None,
            hit_regions: Vec::new(),
            pending_button_action: None,
            modal: modal::ModalState::default(),
            expanded_icon_rows: HashSet::new(),
            last_pane_width: 0,
            tick_counter: 0,
            embedded: HashMap::new(),
            embedded_wake: None,
            right_pane_area: ratatui::layout::Rect::default(),
            embedded_prefix: false,
            embed_error: None,
            branch_status: HashMap::new(),
            status_inflight: false,
            status_tx: None,
            last_status_refresh: None,
            pr_inflight: HashSet::new(),
            pr_result: HashMap::new(),
            pr_tx: None,
            cleanup_inflight: HashSet::new(),
            cleanup_result: HashMap::new(),
            cleanup_tx: None,
            config: Config::load(),
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
            let total = self.workspaces.iter().filter(|w| w.repo_id == repo.id).count();
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
                workspace_count: total,
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
                    text: "(no workspaces)".into(),
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
    /// resume every persisted session id as a tab (or start a first session when
    /// none are stored), then focus the first tab.
    fn toggle_embedded(&mut self) {
        let Some(ws) = self.selected_workspace() else {
            return;
        };
        let ws_id = ws.id.clone();
        let ws_name = ws.name.clone();
        let ws_dir = ws.working_dir.clone();
        if !self.embedded.contains_key(&ws_id) {
            // Cleared up front; spawn_session_tab re-sets it on any failure, so a
            // partial-resume failure's message survives (a later clear would
            // swallow it).
            self.embed_error = None;
            let persisted: Vec<String> = self.state.embedded_session_ids(&ws_id).to_vec();
            if persisted.is_empty() {
                self.spawn_session_tab(&ws_id, &ws_dir, &ws_name, None);
            } else {
                for id in persisted.iter().take(MAX_SESSION_TABS) {
                    self.spawn_session_tab(&ws_id, &ws_dir, &ws_name, Some(id));
                }
            }
            // If every spawn failed, embed_error is set — stay on the tree.
            let Some(sessions) = self.embedded.get_mut(&ws_id) else {
                return;
            };
            sessions.active = 0; // focus the first tab on open
        } else {
            self.embed_error = None;
        }
        self.focus = Focus::Embedded;
        self.embedded_prefix = false;
    }

    /// Spawn a claude pane (no persistence, no tab append). `resume_id` resumes
    /// that session; `None` assigns a fresh session id. Returns the pane plus its
    /// `(session_id, was_resume)`, or the spawn error.
    fn spawn_pane(
        &self,
        ws_dir: &str,
        resume_id: Option<&str>,
    ) -> anyhow::Result<(pane::Pane, String, bool)> {
        let bin = pick_claude_bin(
            std::env::var("KOMMAND0_CLAUDE_BIN").ok(),
            self.config.claude_bin.as_deref(),
        );
        let wake: Option<Box<dyn Fn() + Send>> = self.embedded_wake.clone().map(|tx| {
            Box::new(move || {
                let _ = tx.send(());
            }) as Box<dyn Fn() + Send>
        });
        // Spawn at the pane's final inner size so the first render needs no resize
        // (claude drops its first screen on a SIGWINCH mid-render).
        let inner = pane_content_rect(self.right_pane_area);
        let rows = if inner.height > 0 { inner.height } else { 24 };
        let cols = if inner.width > 0 { inner.width } else { 80 };
        let (mut args, new_id) = claude_args(resume_id);
        // Append the user's configured passthrough args (e.g. `--model sonnet`).
        args.extend(self.config.claude_args.iter().cloned());
        let was_resume = resume_id.is_some();
        let session_id = resume_id
            .map(String::from)
            .unwrap_or_else(|| new_id.unwrap());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let pane = pane::Pane::spawn_with_wake(
            &bin,
            &arg_refs,
            std::path::Path::new(ws_dir),
            rows,
            cols,
            wake,
        )?;
        Ok((pane, session_id, was_resume))
    }

    /// Spawn one Claude session (a tab) for a workspace and append it. `resume_id`
    /// resumes that session; `None` assigns + persists a fresh id. Returns whether
    /// the pane started (on failure, `embed_error` is set).
    fn spawn_session_tab(
        &mut self,
        ws_id: &str,
        ws_dir: &str,
        ws_name: &str,
        resume_id: Option<&str>,
    ) -> bool {
        match self.spawn_pane(ws_dir, resume_id) {
            Ok((pane, session_id, was_resume)) => {
                if !was_resume {
                    self.state.add_embedded_session(ws_id, &session_id);
                    if self.state.save().is_err() {
                        self.embed_error = Some((
                            ws_id.to_string(),
                            "Couldn't persist this session — it may not resume \
                             after restarting kommand0."
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
                    });
                true
            }
            Err(e) => {
                // A resume that couldn't even spawn: forget the id so the
                // persisted Vec stays aligned with the runtime tabs.
                if let Some(id) = resume_id {
                    self.state.remove_embedded_session(ws_id, id);
                    let _ = self.state.save();
                }
                self.embed_error =
                    Some((ws_id.to_string(), format!("Failed to start claude in {ws_name}: {e}")));
                false
            }
        }
    }

    /// Auto-heal a resume that found no session: forget the gone id and replace
    /// its tab in place with a fresh session (same slot, so the active tab and
    /// numbering are preserved). Returns `false` if the fresh spawn itself failed
    /// (the caller then drops the tab). `now` stamps the new tab.
    fn heal_resume(&mut self, ws_id: &str, gone_id: &str, ws_dir: &str, now: Instant) -> bool {
        // Carry the user's tab title to the replacement (capture before the
        // remove, which now also forgets the title). The tab's *purpose* is still
        // meaningful even though its conversation is gone.
        let prior_title = self
            .state
            .embedded_session_title(ws_id, gone_id)
            .map(str::to_string);
        self.state.remove_embedded_session(ws_id, gone_id);
        match self.spawn_pane(ws_dir, None) {
            Ok((pane, new_id, was_resume)) => {
                self.state.add_embedded_session(ws_id, &new_id);
                if let Some(title) = &prior_title {
                    self.state.set_embedded_session_title(ws_id, &new_id, title);
                }
                let _ = self.state.save();
                if let Some(sessions) = self.embedded.get_mut(ws_id)
                    && let Some(slot) = sessions.tabs.iter().position(|t| t.id == gone_id)
                {
                    sessions.tabs[slot] = SessionTab {
                        id: new_id,
                        pane,
                        was_resume,
                        spawned: now,
                    };
                }
                self.embed_error = Some((
                    ws_id.to_string(),
                    "The previous Claude session was gone — started a fresh one.".to_string(),
                ));
                true
            }
            Err(_) => {
                let _ = self.state.save();
                false
            }
        }
    }

    /// The active session's pane for the selected workspace, if any.
    fn active_pane_mut(&mut self) -> Option<&mut pane::Pane> {
        let ws_id = self.selected_workspace()?.id.clone();
        self.embedded.get_mut(&ws_id)?.active_pane_mut()
    }

    /// Whether any of a workspace's session tabs is currently producing output.
    pub(crate) fn ws_has_active_session(&self, ws_id: &str) -> bool {
        self.embedded
            .get(ws_id)
            .map(|s| s.tabs.iter().any(|t| self.waiting_response.contains(&t.id)))
            .unwrap_or(false)
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
        // Closing a tab forgets its session (it won't resume next time).
        self.state.remove_embedded_session(&ws_id, &tab_id);
        let _ = self.state.save();
        if now_empty {
            self.embedded.remove(&ws_id);
            self.focus = Focus::Tree;
        }
        // The session likely committed; refresh this workspace's branch status.
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

    /// Open an additional session tab for a workspace (up to the cap) and focus it.
    fn new_session(&mut self, ws_id: &str) {
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
        if self.spawn_session_tab(ws_id, &ws.working_dir, &ws.name, None) {
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

    /// Forward a mouse event to the focused embedded pane, translated into the
    /// pane's inner coordinate space. The pane only acts on it if claude enabled
    /// mouse reporting. Events outside the pane's inner area are ignored (so a
    /// click on the border/tree can't strand the pane).
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
        match mouse.kind {
            MouseEventKind::Moved => {
                // Keep mouse_pos current so tab hover styling works in Embedded mode.
                self.mouse_pos = Some((mouse.column, mouse.row));
                self.forward_mouse_to_embedded(mouse);
            }
            MouseEventKind::Down(MouseButton::Left) => {
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
        let mut exited: Vec<(String, String, bool, Instant, Option<i32>)> = Vec::new();
        let mut resume_missed: Vec<(String, String)> = Vec::new();
        for (ws_id, sessions) in self.embedded.iter_mut() {
            for tab in sessions.tabs.iter_mut() {
                if let Some(code) = tab.pane.try_wait() {
                    exited.push((ws_id.clone(), tab.id.clone(), tab.was_resume, tab.spawned, code));
                } else if tab.was_resume
                    && now.saturating_duration_since(tab.spawned) < RESUME_CHECK_WINDOW
                {
                    // Require BOTH the marker and this tab's (random uuid) session
                    // id, so a resumed conversation that merely mentions the phrase
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
        // gone session with a fresh one in the SAME tab slot (the replacement
        // gets a new id, so the retain pass below leaves it alone). If the fresh
        // spawn also fails, fall back to dropping the tab with the reopen message.
        let mut failed_resume: Vec<(String, String)> = exited
            .iter()
            .filter(|(_, _, was_resume, spawned, code)| resume_failed(*spawned, *was_resume, now, *code))
            .map(|(ws, tab, ..)| (ws.clone(), tab.clone()))
            .collect();
        failed_resume.extend(resume_missed.iter().cloned());
        for (ws_id, tab_id) in &failed_resume {
            let ws_dir = self
                .workspaces
                .iter()
                .find(|w| &w.id == ws_id)
                .map(|w| w.working_dir.clone());
            match ws_dir {
                Some(dir) if self.heal_resume(ws_id, tab_id, &dir, now) => {}
                _ => {
                    // Couldn't start a fresh session — forget the id and drop the
                    // stuck/dead tab; the message tells the user to reopen.
                    self.state.remove_embedded_session(ws_id, tab_id);
                    let _ = self.state.save();
                    self.embed_error = Some((ws_id.clone(), RESUME_FAIL_MSG.to_string()));
                }
            }
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

    /// Refresh `waiting_response` (the activity-spinner set) from per-session
    /// output deltas across all tabs of all workspaces. Called every tick.
    fn update_pane_activity(&mut self, now: Instant) {
        let seqs: Vec<(String, u64)> = self
            .embedded
            .values()
            .flat_map(|s| s.tabs.iter().map(|t| (t.id.clone(), t.pane.output_seq())))
            .collect();
        self.apply_pane_activity(now, &seqs);
        // Keep the on-screen session marked seen, then latch any others that went
        // quiet with unseen output. Order matters: clear before latching so the
        // session you're watching is never flagged.
        self.mark_active_viewed();
        self.recompute_attention(now, &seqs);
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
    fn recompute_attention(&mut self, now: Instant, seqs: &[(String, u64)]) {
        const ATTENTION_SETTLE: Duration = Duration::from_millis(1500);
        for (id, seq) in seqs {
            let seen = self.viewed_seq.get(id).copied().unwrap_or(0);
            let unseen = *seq > seen;
            let settled = self
                .last_output_at
                .get(id)
                .is_some_and(|t| now.duration_since(*t) >= ATTENTION_SETTLE);
            if unseen && settled {
                self.attention.insert(id.clone());
            }
        }
        // Forget sessions whose pane is gone (closed/healed-to-a-new-id).
        let live: HashSet<&str> = seqs.iter().map(|(id, _)| id.as_str()).collect();
        self.viewed_seq.retain(|id, _| live.contains(id.as_str()));
        self.attention.retain(|id| live.contains(id.as_str()));
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

    /// Open a GitHub PR for a workspace's branch off the render loop: push the
    /// branch and run `gh pr create`. No-op if one is already in flight; sets a
    /// `pr_result` error immediately when the workspace has no own branch.
    fn open_pr(&mut self, ws_id: &str) {
        if self.pr_inflight.contains(ws_id) || self.cleanup_inflight.contains(ws_id) {
            return; // don't race a cleanup of the same worktree
        }
        let Some(ws) = self.workspaces.iter().find(|w| w.id == ws_id) else {
            return;
        };
        let (Some(worktree), Some(branch)) = (ws.worktree_path.clone(), ws.branch_name.clone())
        else {
            self.pr_result.insert(
                ws_id.to_string(),
                Err("this workspace has no branch to open a PR from".to_string()),
            );
            return;
        };
        let Some(tx) = self.pr_tx.clone() else {
            return; // not wired (unit tests drive pr_result/pr_inflight directly)
        };
        self.pr_inflight.insert(ws_id.to_string());
        self.pr_result.remove(ws_id); // clear any stale outcome
        let id = ws_id.to_string();
        std::thread::spawn(move || {
            // Default to an error so a panic before completion still clears the
            // inflight flag with a sensible message.
            let mut guard = PrOpenGuard {
                tx,
                payload: Some((id.clone(), Err("opening the PR was interrupted".to_string()))),
            };
            let result = kommand0_core::open_pull_request(&worktree, &branch);
            guard.payload = Some((id, result));
        });
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
        if self.cleanup_inflight.contains(ws_id) || self.pr_inflight.contains(ws_id) {
            return; // don't remove the worktree while a PR push runs in it
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
    /// observed `(session_id, output_seq)` for the live panes, arm a session as
    /// active only after two consecutive ticks of new output (debounce), let it
    /// decay after `ACTIVE_WINDOW`, and prune sessions no longer present.
    fn apply_pane_activity(&mut self, now: Instant, seqs: &[(String, u64)]) {
        const ACTIVE_WINDOW: Duration = Duration::from_millis(500);
        for (id, seq) in seqs {
            let had_new = self.pane_seen.get(id) != Some(seq);
            self.pane_seen.insert(id.clone(), *seq);
            if had_new {
                // Stamp the last-output time (no debounce) for the settle check.
                self.last_output_at.insert(id.clone(), now);
                if self.pane_pending.contains(id) {
                    self.pane_active_until.insert(id.clone(), now + ACTIVE_WINDOW);
                } else {
                    self.pane_pending.insert(id.clone());
                }
            } else {
                self.pane_pending.remove(id);
            }
        }
        // Drop bookkeeping for panes no longer present this tick.
        let live: HashSet<&str> = seqs.iter().map(|(id, _)| id.as_str()).collect();
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

/// Handle one key press. Extracted from the main event loop so tests can
/// drive the app without a real terminal.
async fn handle_key(app: &mut App, key: KeyEvent) -> anyhow::Result<KeyOutcome> {
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
                        app.new_session(&ws_id);
                    }
                    return Ok(KeyOutcome::Continue);
                }
                KeyCode::Char('x') if !ctrl => {
                    app.close_active_session();
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

    // Modal dialog: swallow all keys
    if app.modal.is_active() {
        match modal::handle_modal_key(&mut app.modal, key) {
            modal::ModalResult::Consumed | modal::ModalResult::Cancelled => {}
            modal::ModalResult::SubmitRepo(path) => match app.state.add_repo(&path) {
                Ok(_) => {
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
                    modal::DeleteTarget::Workspace { name } => {
                        // Gather IDs before mutating
                        let ws_info =
                            app.state
                                .workspaces
                                .iter()
                                .find(|w| w.name == name)
                                .map(|w| {
                                    let sid = app
                                        .state
                                        .find_session_by_workspace(&w.id)
                                        .filter(|s| s.status == SessionStatus::Running)
                                        .map(|s| s.id.clone());
                                    (w.id.clone(), sid)
                                });
                        if let Some((ws_id, running_sid)) = ws_info {
                            if let Some(sid) = running_sid {
                                let _ = app
                                    .state
                                    .update_session_status(&sid, SessionStatus::Stopped);
                            }
                            // Tear the embedded pane down here (at user-action
                            // time) rather than letting reap_embedded block the
                            // 50ms tick on the pane's Drop.
                            app.embedded.remove(&ws_id);
                        }
                        let _ = app.state.delete_workspace(&name);
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
            modal::ModalResult::SubmitWorkspace(repo_id, name) => {
                let repo_name = app
                    .repos
                    .iter()
                    .find(|r| r.id == repo_id)
                    .map(|r| r.name.clone())
                    .unwrap_or_default();
                match app.state.create_workspace(Some(&name), &repo_name) {
                    Ok(_) => {
                        app.workspaces = app.state.workspaces.clone();
                        // Auto-expand the repo
                        app.expanded.insert(repo_id);
                        app.rebuild_tree();
                        // Surface the new workspace's branch status promptly.
                        app.request_branch_status_refresh();
                    }
                    Err(e) => {
                        app.modal = modal::ModalState::AddWorkspace {
                            repo_id,
                            repo_name,
                            input: name,
                            cursor: 0,
                            error: Some(e.to_string()),
                        };
                    }
                }
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
                    let _ = app.state.save();
                }
            }
            modal::ModalResult::ConfirmCleanup(ws_id) => {
                app.start_cleanup(&ws_id);
            }
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

    // Vim `gg`: consume the pending flag; only a bare `g` below re-arms it
    let g_was_pending = std::mem::take(&mut app.pending_g);

    // Global keys (work in any focus)
    match key.code {
        KeyCode::Char('q') => {
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
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
            app.help_scroll = 0;
        }
        _ => {
            // Focus-specific keys
            match app.focus {
                // Embedded keys are intercepted at the top of handle_key.
                Focus::Embedded => {}
                Focus::Tree => {
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                        KeyCode::Left | KeyCode::Char('h') => app.tree_collapse_or_parent(),
                        KeyCode::Right | KeyCode::Char('l') => app.tree_expand_or_enter(),
                        KeyCode::Char('g') => {
                            if g_was_pending {
                                app.tree_select_first();
                            } else {
                                app.pending_g = true;
                            }
                        }
                        KeyCode::Char('G') => app.tree_select_last(),
                        KeyCode::Char('e') => app.toggle_embedded(),
                        KeyCode::Enter => {
                            match app.tree_items.get(app.selected_index) {
                                // Enter on a repo expands it; on a workspace it
                                // opens the embedded interactive claude (the
                                // default session experience).
                                Some(TreeNode::Repo { .. }) => app.toggle_expand(),
                                Some(TreeNode::Workspace { .. }) => app.toggle_embedded(),
                                Some(TreeNode::Hint { .. }) | None => {}
                            }
                        }
                        // 'r' is an alias for Enter/'e': open the embedded claude.
                        KeyCode::Char('r') => app.toggle_embedded(),
                        KeyCode::Char('x') | KeyCode::Delete => {
                            if let Some(ws) = app.selected_workspace().cloned() {
                                // Close an embedded claude pane (Pane's Drop kills it).
                                app.embedded.remove(&ws.id);
                                // Also stop any legacy stream session.
                                let session_info = app
                                    .state
                                    .find_session_by_workspace(&ws.id)
                                    .filter(|s| s.status == SessionStatus::Running)
                                    .map(|s| s.id.clone());
                                if let Some(session_id) = session_info {
                                    let _ = app
                                        .state
                                        .update_session_status(&session_id, SessionStatus::Stopped);
                                }
                            }
                        }
                        // 'R' is an alias for Enter/'e': open the embedded claude.
                        KeyCode::Char('R') => app.toggle_embedded(),
                        KeyCode::Char('p') => {
                            // Open a GitHub PR for the selected workspace's branch.
                            if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone()) {
                                app.open_pr(&ws_id);
                            }
                        }
                        KeyCode::Char('c') => {
                            // Clean up the selected (merged) workspace, via a
                            // confirmation modal.
                            if let Some(ws_id) = app.selected_workspace().map(|w| w.id.clone()) {
                                app.cleanup_workspace_prompt(&ws_id);
                            }
                        }
                        KeyCode::Char('/') => {
                            // Enter the tree filter; keep any existing query to edit.
                            app.filter_input = true;
                        }
                        KeyCode::Esc => {
                            // Clear an applied filter (no-op otherwise).
                            if !app.filter_query.is_empty() {
                                app.filter_query.clear();
                                app.apply_filter();
                            }
                        }
                        KeyCode::Char('A') => {
                            // Toggle the selected workspace's archived/active state.
                            if let Some(ws) = app.selected_workspace().cloned() {
                                let res = if ws.active {
                                    app.state.archive_workspace(&ws.name)
                                } else {
                                    app.state.activate_workspace(&ws.name)
                                };
                                if res.is_ok() {
                                    app.workspaces = app.state.workspaces.clone();
                                    app.rebuild_tree();
                                    app.select_workspace_row(&ws.id);
                                    app.clamp_selection();
                                }
                            }
                        }
                        KeyCode::Char('a') => {
                            // Open Add Repo modal
                            app.modal = modal::ModalState::AddRepo {
                                input: String::new(),
                                cursor: 0,
                                error: None,
                                completions: Vec::new(),
                                completion_index: None,
                            };
                        }
                        KeyCode::Char('w') => {
                            // Open Add Workspace modal for selected repo
                            let repo_info = match app.tree_items.get(app.selected_index) {
                                Some(TreeNode::Repo { id, name, .. }) => {
                                    Some((id.clone(), name.clone()))
                                }
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
                                    error: None,
                                };
                            }
                        }
                        KeyCode::Char('d') => {
                            // Delete selected item with confirmation
                            match app.tree_items.get(app.selected_index).cloned() {
                                Some(TreeNode::Workspace { ws, .. }) => {
                                    app.modal = modal::ModalState::ConfirmDelete {
                                        target: modal::DeleteTarget::Workspace {
                                            name: ws.name.clone(),
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
                            }
                        }
                        KeyCode::Char('D') => {
                            // Force delete without confirmation
                            match app.tree_items.get(app.selected_index).cloned() {
                                Some(TreeNode::Workspace { ws, .. }) => {
                                    let ws_info = app
                                        .state
                                        .workspaces
                                        .iter()
                                        .find(|w| w.name == ws.name)
                                        .map(|w| {
                                            let sid = app
                                                .state
                                                .find_session_by_workspace(&w.id)
                                                .filter(|s| s.status == SessionStatus::Running)
                                                .map(|s| s.id.clone());
                                            (w.id.clone(), sid)
                                        });
                                    if let Some((ws_id, running_sid)) = ws_info {
                                        if let Some(sid) = running_sid {
                                            let _ = app.state.update_session_status(
                                                &sid,
                                                SessionStatus::Stopped,
                                            );
                                        }
                                        app.embedded.remove(&ws_id);
                                    }
                                    let _ = app.state.delete_workspace(&ws.name);
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
                                            let _ = app.state.update_session_status(
                                                &sid,
                                                SessionStatus::Stopped,
                                            );
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
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(KeyOutcome::Continue)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    // DISAMBIGUATE_ESCAPE_CODES lets terminals report Shift+Enter distinctly from Enter
    let supports_enhanced_keys =
        crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)?;
    if supports_enhanced_keys {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
            )
        );
    }
    let result = run(&mut terminal).await;
    if supports_enhanced_keys {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

async fn run(terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
    let state = AppState::load()?;
    let mut app = App::new(state);

    // Reconcile persisted status with the (empty) session_manager. No stream
    // session is ever resurrected now, so a persisted `Running` is stale — left
    // behind by a crash/SIGKILL that skipped the clean-quit normalization.
    // Flipping it to Stopped prevents a phantom (silently-failing) legacy
    // composer and a stale "running" tree icon on the first launch after upgrade.
    let stale_running: Vec<String> = app
        .state
        .sessions
        .iter()
        .filter(|s| s.status == SessionStatus::Running)
        .map(|s| s.id.clone())
        .collect();
    if !stale_running.is_empty() {
        for sid in stale_running {
            let _ = app
                .state
                .update_session_status(&sid, SessionStatus::Stopped);
        }
        let _ = app.state.save();
    }

    // Drop persisted Claude session ids for workspaces that no longer exist.
    let before = app.state.embedded_sessions.len();
    app.state.prune_embedded_sessions();
    if app.state.embedded_sessions.len() != before {
        let _ = app.state.save();
    }

    let mut reader = EventStream::new();
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

    // PR-open worker → event loop, carrying `(workspace_id, Ok(url) | Err(msg))`.
    let (pr_tx, mut pr_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<String, String>)>();
    app.pr_tx = Some(pr_tx);

    // Cleanup worker → event loop, carrying `(workspace_id, Ok(()) | Err(msg))`.
    let (cleanup_tx, mut cleanup_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Result<(), String>)>();
    app.cleanup_tx = Some(cleanup_tx);

    loop {
        // Seed the viewed session before drawing so a just-opened/just-switched
        // tab is never momentarily flagged "needs you" before the next tick.
        app.mark_active_viewed();
        terminal.draw(|frame| render::ui(frame, &mut app))?;

        tokio::select! {
            _ = wake_rx.recv() => {
                // Drain any backlog so we coalesce into a single redraw.
                while wake_rx.try_recv().is_ok() {}
            }
            Some(status) = status_rx.recv() => {
                // A status refresh finished: replace the cache wholesale (drops
                // entries for deleted workspaces) and allow the next refresh.
                app.branch_status = status;
                app.status_inflight = false;
            }
            Some((ws_id, result)) = pr_rx.recv() => {
                // A PR-open finished: record the outcome, clear in-flight, and
                // refresh branch status (the push changed ahead/behind).
                app.pr_inflight.remove(&ws_id);
                app.pr_result.insert(ws_id, result);
                app.request_branch_status_refresh();
            }
            Some((ws_id, result)) = cleanup_rx.recv() => {
                app.cleanup_inflight.remove(&ws_id);
                match result {
                    Ok(()) => {
                        // The worktree + branch are gone — drop the workspace too.
                        if let Some(name) = app.state.workspaces.iter()
                            .find(|w| w.id == ws_id).map(|w| w.name.clone())
                        {
                            let _ = app.state.delete_workspace(&name);
                            app.workspaces = app.state.workspaces.clone();
                            app.expanded_icon_rows.remove(&ws_id);
                            app.pr_result.remove(&ws_id);
                            app.cleanup_result.remove(&ws_id);
                            app.rebuild_tree();
                            if app.selected_index >= app.tree_items.len() && !app.tree_items.is_empty() {
                                app.selected_index = app.tree_items.len() - 1;
                            }
                        }
                    }
                    Err(msg) => {
                        app.cleanup_result.insert(ws_id, msg);
                    }
                }
                app.request_branch_status_refresh();
            }
            event = reader.next().fuse() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        if handle_key(&mut app, key).await? == KeyOutcome::Quit {
                            break;
                        }
                    }
                    Some(Ok(Event::Paste(text))) => {
                        // Only the embedded pane consumes paste; forward it as a
                        // bracketed paste (claude enables bracketed paste, so a
                        // multi-line paste stays one block). A modal over the pane
                        // (e.g. Rename Session) suppresses the passthrough.
                        if app.focus == Focus::Embedded && !app.modal.is_active() {
                            let sent = app.active_pane_mut().map(|pane| {
                                let mut bytes = b"\x1b[200~".to_vec();
                                bytes.extend_from_slice(text.as_bytes());
                                bytes.extend_from_slice(b"\x1b[201~");
                                let _ = pane.send(&bytes);
                            });
                            if sent.is_none() {
                                app.focus = Focus::Tree;
                            }
                        }
                    }
                    Some(Ok(Event::Mouse(mouse_event))) => {
                        if app.show_help || app.modal.is_active() {
                            // An overlay owns the screen — ignore mouse (don't leak
                            // stray clicks to the embedded claude behind a modal).
                        } else if app.focus == Focus::Embedded {
                            app.handle_embedded_mouse(mouse_event);
                        } else {
                            mouse::handle_mouse(&mut app, mouse_event);
                        }
                    }
                    Some(Err(_)) => break,
                    None => break,
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
                            app.new_session(&workspace_id);
                        }
                        buttons::HitAction::OpenPrFor { workspace_id } => {
                            app.open_pr(&workspace_id);
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
                            // Find workspace name from ID
                            if let Some(ws) = app.state.workspaces.iter().find(|w| w.id == workspace_id).cloned() {
                                // Stop running session first
                                if let Some(session_id) = app.state.find_session_by_workspace(&workspace_id)
                                    .filter(|s| s.status == SessionStatus::Running)
                                    .map(|s| s.id.clone())
                                {
                                    let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                }
                                if app.state.delete_workspace(&ws.name).is_ok() {
                                    app.embedded.remove(&workspace_id);
                                    app.expanded_icon_rows.remove(&workspace_id);
                                    app.repos = app.state.repos.clone();
                                    app.workspaces = app.state.workspaces.clone();
                                    app.rebuild_tree();
                                    if app.selected_index >= app.tree_items.len() && !app.tree_items.is_empty() {
                                        app.selected_index = app.tree_items.len() - 1;
                                    }
                                    app.update_active_session();
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
                                    if app.selected_index >= app.tree_items.len() && !app.tree_items.is_empty() {
                                        app.selected_index = app.tree_items.len() - 1;
                                    }
                                    app.update_active_session();
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
                    .map(Duration::from_secs)
                    .unwrap_or(STATUS_REFRESH_INTERVAL);
                if app
                    .last_status_refresh
                    .is_none_or(|t| now.duration_since(t) >= status_interval)
                {
                    app.last_status_refresh = Some(now);
                    app.request_branch_status_refresh();
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
        App::new(state)
    }

    async fn press(app: &mut App, code: KeyCode) -> KeyOutcome {
        handle_key(app, key(code)).await.unwrap()
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
            mk_ws("w3", "misc", "r1", Some("kommand0/billing")),
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
            out.push('\n');
        }
        out
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
        app.apply_pane_activity(t, &[("w1".to_string(), 1)]);
        assert!(
            !app.waiting_response.contains("w1"),
            "a single output tick must not arm (debounce)"
        );

        // A second consecutive tick of new output arms it.
        app.apply_pane_activity(t, &[("w1".to_string(), 2)]);
        assert!(
            app.waiting_response.contains("w1"),
            "two consecutive output ticks should mark the pane active"
        );

        // No new output past the active window: decays to idle.
        app.apply_pane_activity(t + Duration::from_millis(600), &[("w1".to_string(), 2)]);
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
    fn attention_latches_on_unseen_quiet_and_sticks() {
        let mut app = test_app(); // focus Tree -> nothing is "viewed"
        let t = Instant::now();

        // Fresh output isn't attention until it has gone quiet (settled).
        app.apply_pane_activity(t, &[("s1".to_string(), 1)]);
        app.recompute_attention(t, &[("s1".to_string(), 1)]);
        assert!(!app.attention.contains("s1"), "fresh output isn't attention yet");

        // Unseen + quiet past the settle window -> latched.
        let later = t + Duration::from_millis(1500);
        app.recompute_attention(later, &[("s1".to_string(), 1)]);
        assert!(app.attention.contains("s1"), "unseen + settled => needs you");

        // Resumed output must NOT clear it (no mid-turn strobe) — only viewing does.
        app.apply_pane_activity(later, &[("s1".to_string(), 2)]);
        app.recompute_attention(later, &[("s1".to_string(), 2)]);
        assert!(
            app.attention.contains("s1"),
            "latched attention survives resumed output"
        );

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
        app.apply_pane_activity(t, &[("s1".to_string(), 1)]);
        app.recompute_attention(t + Duration::from_millis(1500), &[("s1".to_string(), 1)]);
        assert!(app.attention.contains("s1"), "first latch");

        // Simulate viewing it (seen up to seq 1, cleared).
        app.viewed_seq.insert("s1".to_string(), 1);
        app.attention.remove("s1");

        // New output after viewing, still fresh -> not yet.
        let t2 = t + Duration::from_millis(2000);
        app.apply_pane_activity(t2, &[("s1".to_string(), 2)]);
        app.recompute_attention(t2, &[("s1".to_string(), 2)]);
        assert!(!app.attention.contains("s1"), "fresh post-view output isn't attention yet");

        // ...once it settles, it re-latches.
        app.recompute_attention(t2 + Duration::from_millis(1500), &[("s1".to_string(), 2)]);
        assert!(app.attention.contains("s1"), "unseen output after a view re-latches");
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
            },
        );
        app.embedded.insert(
            "w2".to_string(),
            WorkspaceSessions {
                tabs: vec![tab("c", &["-c", "sleep 30"])],
                active: 0,
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
        // default workspace has no worktree_path).
        app.workspaces[0].worktree_path = Some("/tmp/alpha".into());
        app.workspaces[0].branch_name = Some("kommand0/ws-one".into());
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");
        app.branch_status.insert(
            "w1".to_string(),
            kommand0_core::BranchStatus {
                branch: Some("kommand0/ws-one".into()),
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
        assert!(text.contains("kommand0/ws-one"), "detail shows the branch name");
        assert!(text.contains("↑2 ↓1"), "detail shows ahead/behind");
        assert!(text.contains("uncommitted changes"), "detail shows dirty state");
        // Tree row segment (compact, no spaces): " ↑2↓1*".
        assert!(text.contains("↑2↓1*"), "tree row shows the compact status segment:\n{text}");
    }

    #[test]
    fn open_pr_without_a_branch_records_an_error() {
        let mut app = test_app(); // w1 has worktree_path: None
        let id = app.workspaces[0].id.clone();
        app.open_pr(&id);
        match app.pr_result.get(&id) {
            Some(Err(msg)) => assert!(msg.contains("no branch"), "got: {msg}"),
            other => panic!("expected a no-branch error, got {other:?}"),
        }
        assert!(!app.pr_inflight.contains(&id), "no worker spawned for a branchless workspace");
    }

    #[tokio::test]
    async fn pr_affordance_and_states_render() {
        let mut app = test_app();
        app.workspaces[0].worktree_path = Some("/tmp/alpha".into());
        app.workspaces[0].branch_name = Some("kommand0/ws-one".into());
        app.expanded.insert("r1".to_string());
        app.rebuild_tree();
        app.select_workspace_row("w1");

        let draw = |app: &mut App| {
            let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
            t.draw(|frame| render::ui(frame, app)).unwrap();
            buffer_text(&t)
        };

        // Idle: the button is offered.
        assert!(draw(&mut app).contains("[Open PR]"), "idle shows the button");

        // In flight: progress instead of the button.
        app.pr_inflight.insert("w1".to_string());
        let text = draw(&mut app);
        assert!(text.contains("Opening PR"), "in-flight shows progress");
        assert!(!text.contains("[Open PR]"), "button hidden while in flight");
        app.pr_inflight.remove("w1");

        // Success: the URL.
        app.pr_result
            .insert("w1".to_string(), Ok("https://github.com/x/y/pull/1".to_string()));
        let text = draw(&mut app);
        assert!(text.contains("pull/1"), "shows the PR URL:\n{text}");

        // Failure: the error.
        app.pr_result
            .insert("w1".to_string(), Err("boom".to_string()));
        let text = draw(&mut app);
        assert!(text.contains("PR failed:") && text.contains("boom"), "shows the error:\n{text}");
    }

    #[tokio::test]
    async fn c_opens_cleanup_modal_for_own_branch_and_noops_for_fallback() {
        // Own-branch workspace: `c` opens the confirmation modal.
        let mut app = test_app();
        app.workspaces[0].worktree_path = Some("/tmp/alpha".into());
        app.workspaces[0].branch_name = Some("kommand0/ws-one".into());
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
        app.workspaces[0].branch_name = Some("kommand0/ws-one".into());
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
    fn claude_args_assigns_or_resumes() {
        // No resume id: a fresh --session-id is assigned and returned to persist.
        let (args, new) = claude_args(None);
        assert_eq!(args[0], "--session-id");
        assert_eq!(new.as_deref(), Some(args[1].as_str()));

        // With a resume id: resume that exact id (no new id to store).
        let (args2, new2) = claude_args(Some("sess-1"));
        assert_eq!(args2, vec!["--resume".to_string(), "sess-1".to_string()]);
        assert!(new2.is_none());
    }

    #[test]
    fn pick_claude_bin_precedence() {
        // Env wins (tests/e2e rely on this); empty env is ignored.
        assert_eq!(pick_claude_bin(Some("envbin".into()), Some("cfgbin")), "envbin");
        assert_eq!(pick_claude_bin(Some(String::new()), Some("cfgbin")), "cfgbin");
        // No env -> config; nothing -> default.
        assert_eq!(pick_claude_bin(None, Some("cfgbin")), "cfgbin");
        assert_eq!(pick_claude_bin(None, None), "claude");
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
    fn reap_drops_exited_tab_and_keeps_active_by_identity() {
        let mut app = test_app(); // has workspace "w1"
        let mut s = WorkspaceSessions {
            tabs: vec![
                tab("a", &["-c", "sleep 30"]),
                tab("b", &["-c", "sleep 30"]),
                tab("c", &["-c", "sleep 30"]),
            ],
            active: 2, // active on "c"
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
                },
                tab("b", &["-c", "sleep 30"]),
            ],
            active: 0,
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
    async fn renders_help_overlay_after_question_mark() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?')).await;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render::ui(frame, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Help"), "help overlay should render:\n{text}");
        assert!(
            text.contains("Navigate"),
            "help should list bindings:\n{text}"
        );
    }
}
