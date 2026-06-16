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
use kommand0_core::{AppState, RepoEntry, SessionStatus, Workspace};
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

fn claude_args(resume_id: Option<&str>) -> (Vec<String>, Option<String>) {
    match resume_id {
        Some(id) => (vec!["--resume".to_string(), id.to_string()], None),
        None => {
            let uuid = AppState::new_claude_session_id();
            (vec!["--session-id".to_string(), uuid.clone()], Some(uuid))
        }
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
            waiting_response: HashSet::new(),
            spinner_tick: 0,
            pane_seen: HashMap::new(),
            pane_pending: HashSet::new(),
            pane_active_until: HashMap::new(),
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
        };
        app.rebuild_tree();
        if !app.tree_items.is_empty() {
            app.selected_index = 0;
        }
        app
    }

    fn rebuild_tree(&mut self) {
        self.tree_items.clear();
        for repo in &self.repos {
            let repo_workspaces: Vec<&Workspace> = self
                .workspaces
                .iter()
                .filter(|w| w.repo_id == repo.id)
                .collect();
            let workspace_count = repo_workspaces.len();

            self.tree_items.push(TreeNode::Repo {
                id: repo.id.clone(),
                name: repo.name.clone(),
                workspace_count,
            });

            if self.expanded.contains(&repo.id) {
                if repo_workspaces.is_empty() {
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
            if self.expanded.contains(id) {
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
            let persisted: Vec<String> = self.state.embedded_session_ids(&ws_id).to_vec();
            if persisted.is_empty() {
                self.spawn_session_tab(&ws_id, &ws_dir, &ws_name, None);
            } else {
                for id in &persisted {
                    self.spawn_session_tab(&ws_id, &ws_dir, &ws_name, Some(id));
                }
            }
            // If every spawn failed, embed_error is set — stay on the tree.
            let Some(sessions) = self.embedded.get_mut(&ws_id) else {
                return;
            };
            sessions.active = 0; // focus the first tab on open
            self.embed_error = None;
        } else {
            self.embed_error = None;
        }
        self.focus = Focus::Embedded;
        self.embedded_prefix = false;
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
        let bin = std::env::var("KOMMAND0_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
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
        let (args, new_id) = claude_args(resume_id);
        let was_resume = resume_id.is_some();
        let session_id = resume_id
            .map(String::from)
            .unwrap_or_else(|| new_id.clone().unwrap());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match pane::Pane::spawn_with_wake(
            &bin,
            &arg_refs,
            std::path::Path::new(ws_dir),
            rows,
            cols,
            wake,
        ) {
            Ok(pane) => {
                if let Some(uuid) = new_id {
                    self.state.add_embedded_session(ws_id, &uuid);
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
                self.embed_error =
                    Some((ws_id.to_string(), format!("Failed to start claude in {ws_name}: {e}")));
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
    }

    /// Open an additional session tab for a workspace (up to the cap) and focus it.
    fn new_session(&mut self, ws_id: &str) {
        self.select_workspace_row(ws_id);
        let count = self.embedded.get(ws_id).map(|s| s.tabs.len()).unwrap_or(0);
        if count >= MAX_SESSION_TABS {
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
        // One pass: collect exited tabs with the data the resume-failure net
        // needs (keep the signal-vs-exit distinction in `code`).
        let mut exited: Vec<(String, String, bool, Instant, Option<i32>)> = Vec::new();
        for (ws_id, sessions) in self.embedded.iter_mut() {
            for tab in sessions.tabs.iter_mut() {
                if let Some(code) = tab.pane.try_wait() {
                    exited.push((ws_id.clone(), tab.id.clone(), tab.was_resume, tab.spawned, code));
                }
            }
        }

        // Resume-failure safety net, per FAILED tab id only (never the siblings).
        for (ws_id, tab_id, was_resume, spawned, code) in &exited {
            if resume_failed(*spawned, *was_resume, now, *code) {
                self.state.remove_embedded_session(ws_id, tab_id);
                let _ = self.state.save();
                self.embed_error = Some((
                    ws_id.clone(),
                    "Couldn't resume the previous Claude session (it may have been \
                     cleared) — reopen to start fresh."
                        .to_string(),
                ));
            }
        }

        let dead: HashSet<(String, String)> = exited
            .into_iter()
            .map(|(ws, tab, ..)| (ws, tab))
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
#[derive(PartialEq)]
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
    if app.focus == Focus::Embedded {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if app.embedded_prefix {
            app.embedded_prefix = false;
            // The `!ctrl` guards keep `Ctrl+]` (decoded as Char(']') or Char('5')
            // with CTRL) from being read as a tab command after the prefix.
            match key.code {
                KeyCode::Char('q') => {
                    return Ok(KeyOutcome::Quit);
                }
                KeyCode::Char('t') | KeyCode::Tab | KeyCode::Esc => {
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

    loop {
        terminal.draw(|frame| render::ui(frame, &mut app))?;

        tokio::select! {
            _ = wake_rx.recv() => {
                // Drain any backlog so we coalesce into a single redraw.
                while wake_rx.try_recv().is_ok() {}
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
                        // multi-line paste stays one block).
                        if app.focus == Focus::Embedded {
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
                        if app.show_help {
                            // Overlay owns the screen — ignore mouse.
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
                                    app.waiting_response.remove(&workspace_id);
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
                                    app.waiting_response.remove(ws_id);
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
        });
        App::new(state)
    }

    async fn press(app: &mut App, code: KeyCode) -> KeyOutcome {
        handle_key(app, key(code)).await.unwrap()
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
