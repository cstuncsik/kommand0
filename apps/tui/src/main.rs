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
    crossterm::event::{Event, KeyCode, KeyModifiers},
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
fn claude_session_args(state: &AppState, ws_id: &str) -> (Vec<String>, Option<String>) {
    match state.embedded_session_id(ws_id) {
        Some(uuid) => (vec!["--resume".to_string(), uuid.to_string()], None),
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
    /// Workspaces whose embedded pane is currently active (produced output over
    /// the last couple of ticks) — drives the tree activity spinner. Recomputed
    /// each tick from per-pane output deltas.
    pub(crate) waiting_response: HashSet<String>,
    pub(crate) spinner_tick: u8,
    /// Per-pane last-observed `output_seq`, to detect new output between ticks.
    pane_seen: HashMap<String, u64>,
    /// Panes that produced output on the previous tick but aren't armed yet — a
    /// one-tick debounce so a single keystroke echo or redraw doesn't flash active.
    pane_pending: HashSet<String>,
    /// Per-pane instant until which the pane counts as active, refreshed while
    /// its `output_seq` keeps advancing.
    pane_active_until: HashMap<String, Instant>,
    /// Per-pane `(spawn_instant, was_resume)` — used to detect a resumed pane
    /// that died immediately because its Claude session could not be found.
    pane_spawn: HashMap<String, (Instant, bool)>,
    pub(crate) pane_areas: mouse::PaneAreas,
    pub(crate) mouse_pos: Option<(u16, u16)>,
    pub(crate) hit_regions: Vec<buttons::HitRegion>,
    pub(crate) pending_button_action: Option<buttons::HitAction>,
    pub(crate) modal: modal::ModalState,
    pub(crate) expanded_icon_rows: HashSet<String>,
    pub(crate) last_pane_width: u16,
    tick_counter: u8,

    // Embedded PTY panes (migration Phase 2, experimental): per-workspace
    // interactive `claude` sessions composited into the right pane.
    pub(crate) embedded: HashMap<String, pane::Pane>,
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
            pane_spawn: HashMap::new(),
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
    fn toggle_embedded(&mut self) {
        let Some(ws) = self.selected_workspace() else {
            return;
        };
        let ws_id = ws.id.clone();
        let ws_name = ws.name.clone();
        let ws_dir = ws.working_dir.clone();
        if !self.embedded.contains_key(&ws_id) {
            let bin = std::env::var("KOMMAND0_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
            let wake: Option<Box<dyn Fn() + Send>> = self.embedded_wake.clone().map(|tx| {
                Box::new(move || {
                    let _ = tx.send(());
                }) as Box<dyn Fn() + Send>
            });
            // Spawn at the pane's final inner size so the first render needs no
            // resize — claude drops its initial screen (e.g. the trust prompt) on
            // a SIGWINCH that arrives mid-render.
            let inner = self
                .right_pane_area
                .inner(ratatui::layout::Margin::new(1, 1));
            let rows = if inner.height > 0 { inner.height } else { 24 };
            let cols = if inner.width > 0 { inner.width } else { 80 };
            // Resume the workspace's prior Claude session if we have one; else
            // assign a fresh session id to persist on success.
            let (args, new_session) = claude_session_args(&self.state, &ws_id);
            let was_resume = new_session.is_none();
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            match pane::Pane::spawn_with_wake(
                &bin,
                &arg_refs,
                std::path::Path::new(&ws_dir),
                rows,
                cols,
                wake,
            ) {
                Ok(p) => {
                    self.embedded.insert(ws_id.clone(), p);
                    self.embed_error = None;
                    if let Some(uuid) = new_session {
                        self.state.set_embedded_session(&ws_id, &uuid);
                        // This is the only durable write of a freshly-created
                        // session id; surface a failure so the user knows it may
                        // not resume after a restart.
                        if self.state.save().is_err() {
                            self.embed_error = Some((
                                ws_id.clone(),
                                "Couldn't persist this session — it may not resume \
                                 after restarting kommand0."
                                    .to_string(),
                            ));
                        }
                    }
                    self.pane_spawn
                        .insert(ws_id.clone(), (Instant::now(), was_resume));
                }
                Err(e) => {
                    // Surface the failure in this workspace's detail pane (the old
                    // scrollback chat surface is gone); stay on the tree.
                    self.embed_error =
                        Some((ws_id.clone(), format!("Failed to start claude in {ws_name}: {e}")));
                    return;
                }
            }
        }
        self.embed_error = None;
        self.focus = Focus::Embedded;
        self.embedded_prefix = false;
    }

    /// Select a workspace by id (if present in the tree) and open its embedded
    /// claude pane.
    fn embed_workspace_by_id(&mut self, ws_id: &str) {
        if let Some(i) = self
            .tree_items
            .iter()
            .position(|n| matches!(n, TreeNode::Workspace { ws, .. } if ws.id == ws_id))
        {
            self.selected_index = i;
        }
        self.toggle_embedded();
    }

    /// Forward a key to the selected workspace's embedded pane.
    /// Returns false when there is no pane to forward to.
    fn forward_to_embedded(&mut self, key: KeyEvent) -> bool {
        let Some(ws_id) = self.selected_workspace().map(|w| w.id.clone()) else {
            return false;
        };
        match self.embedded.get_mut(&ws_id) {
            Some(pane) => {
                let _ = pane.send_key(key);
                true
            }
            None => false,
        }
    }

    /// Drop embedded panes whose child has exited or whose workspace was deleted
    /// (the latter centrally covers every delete path). If the focused pane went
    /// away, leave Embedded focus. Pane's `Drop` terminates the child.
    fn reap_embedded(&mut self, now: Instant) {
        // Panes whose child has exited, with the exit code.
        let exited: Vec<(String, Option<i32>)> = self
            .embedded
            .iter_mut()
            .filter_map(|(k, p)| p.try_wait().map(|code| (k.clone(), code)))
            .collect();

        // Resume-failure safety net: a resumed pane that died immediately with a
        // non-zero code likely couldn't find its Claude session (purged). Forget
        // the id so a reopen starts fresh, and tell the user why.
        for (ws_id, code) in &exited {
            if let Some((spawned, was_resume)) = self.pane_spawn.get(ws_id)
                && resume_failed(*spawned, *was_resume, now, *code)
            {
                self.state.clear_embedded_session(ws_id);
                let _ = self.state.save();
                self.embed_error = Some((
                    ws_id.clone(),
                    "Couldn't resume the previous Claude session (it may have been \
                     cleared) — reopen to start fresh."
                        .to_string(),
                ));
            }
        }

        let mut dead: Vec<String> = exited.into_iter().map(|(k, _)| k).collect();
        for k in self.embedded.keys() {
            if !self.workspaces.iter().any(|w| &w.id == k) && !dead.contains(k) {
                dead.push(k.clone());
            }
        }
        for k in &dead {
            self.embedded.remove(k);
        }
        // Prune spawn bookkeeping for every pane that's no longer live — covers
        // both the reaped panes above and panes closed elsewhere (x / Stop).
        self.pane_spawn.retain(|k, _| self.embedded.contains_key(k));
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

    /// Refresh `waiting_response` (the tree activity-spinner set) from per-pane
    /// output deltas. Called every tick.
    fn update_pane_activity(&mut self, now: Instant) {
        let seqs: Vec<(String, u64)> = self
            .embedded
            .iter()
            .map(|(id, p)| (id.clone(), p.output_seq()))
            .collect();
        self.apply_pane_activity(now, &seqs);
    }

    /// Core of [`Self::update_pane_activity`], split out for testing: given the
    /// observed `(workspace_id, output_seq)` for the live panes, arm a pane as
    /// active only after two consecutive ticks of new output (debounce), let it
    /// decay after `ACTIVE_WINDOW`, and prune panes no longer present.
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
            match key.code {
                KeyCode::Char('q') => {
                    return Ok(KeyOutcome::Quit);
                }
                KeyCode::Char('t') | KeyCode::Tab | KeyCode::Esc => {
                    app.focus = Focus::Tree;
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
                            let ws_id = app.selected_workspace().map(|w| w.id.clone());
                            let sent = ws_id.and_then(|id| app.embedded.get_mut(&id)).map(|pane| {
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
                        // Embedded focus owns all input (keyboard already captured);
                        // drop mouse too so a click can't strand the pane by flipping
                        // focus to Output. (Forwarding mouse to the PTY is Phase 3.)
                        if !app.show_help && app.focus != Focus::Embedded {
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
    fn claude_session_args_assigns_then_resumes() {
        let mut state = AppState::default();
        // No stored session id: a fresh --session-id is assigned and returned.
        let (args, new) = claude_session_args(&state, "w1");
        assert_eq!(args[0], "--session-id");
        assert_eq!(new.as_deref(), Some(args[1].as_str()));

        // Once persisted, reopening resumes that exact id (no new id to store).
        state.set_embedded_session("w1", &args[1]);
        let (args2, new2) = claude_session_args(&state, "w1");
        assert_eq!(args2, vec!["--resume".to_string(), args[1].clone()]);
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
