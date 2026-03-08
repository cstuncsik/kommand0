mod composer;
mod help;
mod scrollback;
mod session_manager;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crossterm::event::{EventStream, KeyEventKind};
use futures::{FutureExt, StreamExt};
use kommand0_core::{AppState, RepoEntry, SessionStatus, Workspace, workspace::format_timestamp};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{Event, KeyCode, KeyModifiers},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use composer::Composer;
use scrollback::ScrollbackBuffer;
use session_manager::{SessionEvent, SessionManager};

#[allow(dead_code)]
enum Status {
    Idle,
    Done,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Focus {
    Tree,
    Output,
    Composer,
}

#[derive(Clone)]
enum TreeNode {
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

#[allow(dead_code)]
struct App {
    repos: Vec<RepoEntry>,
    workspaces: Vec<Workspace>,
    state: AppState,
    expanded: HashSet<String>,
    tree_items: Vec<TreeNode>,
    selected_index: usize,
    status: Status,

    // Session fields
    session_manager: SessionManager,
    scrollbacks: HashMap<String, ScrollbackBuffer>,
    composer: Composer,
    active_session_id: Option<String>,
    focus: Focus,

    // UX state
    last_output_height: u16,
    show_help: bool,
    zoomed: bool,
}

impl App {
    fn new(state: AppState) -> Self {
        let repos = state.repos.clone();
        let workspaces = state.workspaces.clone();

        // Restore scrollback buffers from log files for existing sessions
        let mut scrollbacks: HashMap<String, ScrollbackBuffer> = HashMap::new();
        for session in &state.sessions {
            let buf = scrollbacks
                .entry(session.workspace_id.clone())
                .or_insert_with(|| ScrollbackBuffer::new(50_000));
            // Load log file contents into scrollback
            let log_path = std::path::Path::new(&session.log_file);
            if log_path.exists() {
                if let Ok(contents) = std::fs::read_to_string(log_path) {
                    let mut last_source = String::new();
                    for line in contents.lines() {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                            let source = val.get("source").and_then(|s| s.as_str()).unwrap_or("").to_string();
                            let content = val.get("content").and_then(|s| s.as_str()).unwrap_or("");
                            if !content.is_empty() {
                                // Add separator when switching between user and claude
                                if !last_source.is_empty() && source != last_source {
                                    buf.push_line("---".to_string());
                                }
                                if source == "user" {
                                    buf.push_line(format!("> {}", content));
                                } else {
                                    buf.push_line(content.to_string());
                                }
                                last_source = source;
                            }
                        }
                    }
                    // Ensure scrolled to bottom after loading
                    buf.reset_scroll();
                }
            }
        }

        let mut app = Self {
            repos,
            workspaces,
            state,
            expanded: HashSet::new(),
            tree_items: Vec::new(),
            selected_index: 0,
            status: Status::Idle,
            session_manager: SessionManager::new(),
            scrollbacks,
            composer: Composer::new(),
            active_session_id: None,
            focus: Focus::Tree,
            last_output_height: 0,
            show_help: false,
            zoomed: false,
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

    fn is_hint(&self, index: usize) -> bool {
        matches!(self.tree_items.get(index), Some(TreeNode::Hint { .. }))
    }

    fn move_up(&mut self) {
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

    fn move_down(&mut self) {
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

    fn toggle_expand(&mut self) {
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
                if let TreeNode::Repo { id, .. } = node {
                    if *id == repo_id {
                        self.selected_index = i;
                        break;
                    }
                }
            }
            if !self.tree_items.is_empty() {
                self.selected_index = self.selected_index.min(self.tree_items.len() - 1);
            }
        }
    }

    /// Update active_session_id based on current selection
    fn update_active_session(&mut self) {
        if let Some(TreeNode::Workspace { ws, .. }) = self.tree_items.get(self.selected_index) {
            let ws_id = ws.id.clone();
            self.active_session_id = self
                .state
                .find_session_by_workspace(&ws_id)
                .map(|s| s.id.clone());

            // Update composer active state based on whether there's a running session
            let has_running = self
                .state
                .find_session_by_workspace(&ws_id)
                .map(|s| s.status == SessionStatus::Running)
                .unwrap_or(false);
            if !has_running && self.focus == Focus::Composer {
                self.focus = Focus::Tree;
                self.composer.set_active(false);
            }
        } else {
            self.active_session_id = None;
            if self.focus != Focus::Tree {
                self.focus = Focus::Tree;
            }
            self.composer.set_active(false);
        }
    }

    /// Get the selected workspace, if any
    fn selected_workspace(&self) -> Option<&Workspace> {
        match self.tree_items.get(self.selected_index) {
            Some(TreeNode::Workspace { ws, .. }) => Some(ws),
            _ => None,
        }
    }

    /// Get session status icon for a workspace
    fn session_status_icon(&self, workspace_id: &str) -> Option<(&str, Color)> {
        self.state
            .find_session_by_workspace(workspace_id)
            .map(|s| match s.status {
                SessionStatus::Running => (" \u{25B6}", Color::Green),   // ▶
                SessionStatus::Stopped => (" \u{25A0}", Color::Yellow),  // ■
                SessionStatus::Failed => (" \u{2717}", Color::Red),      // ✗
                SessionStatus::Exited => (" \u{2717}", Color::DarkGray), // ✗
            })
    }

    /// Write a log line to the session's log file
    fn write_log(&self, session_id: &str, source: &str, content: &str) {
        if let Some(session) = self.state.sessions.iter().find(|s| s.id == session_id) {
            let log_path = std::path::Path::new(&session.log_file);
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let entry = serde_json::json!({
                "timestamp": timestamp,
                "source": source,
                "content": content,
            });
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
            {
                let _ = writeln!(f, "{}", entry);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal).await;
    ratatui::restore();
    result
}

async fn run(terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
    let state = AppState::load()?;
    let mut app = App::new(state);

    // Auto-resume sessions that have history (stopped/exited with log files)
    let sessions_to_resume: Vec<(String, String, String, Option<String>)> = app
        .state
        .sessions
        .iter()
        .filter(|s| s.status != SessionStatus::Running)
        .filter(|s| {
            app.scrollbacks
                .get(&s.workspace_id)
                .map_or(false, |b| !b.is_empty())
        })
        .map(|s| {
            let ws_dir = app
                .workspaces
                .iter()
                .find(|w| w.id == s.workspace_id)
                .map(|w| w.working_dir.clone())
                .unwrap_or_default();
            (
                s.id.clone(),
                s.workspace_id.clone(),
                ws_dir,
                s.claude_session_id.clone(),
            )
        })
        .collect();

    for (old_sid, ws_id, ws_dir, claude_sid) in sessions_to_resume {
        if ws_dir.is_empty() {
            continue;
        }
        // Mark old session as stopped
        let _ = app.state.update_session_status(&old_sid, SessionStatus::Stopped);
        // Create new state session first to get the canonical ID
        if let Ok(new_session) = app.state.create_session(&ws_id) {
            let session_id = new_session.id.clone();
            // Start process using the state session's ID
            match app.session_manager.start_session(&session_id, &ws_dir, claude_sid.as_deref()) {
                Ok(pid) => {
                    if let Some(s) = app.state.find_session_mut(&session_id) {
                        s.pid = Some(pid);
                        s.claude_session_id = claude_sid;
                    }
                    let _ = app.state.save();
                    app.active_session_id = Some(session_id);
                    if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                        buf.reset_scroll();
                    }
                }
                Err(_) => {
                    let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                }
            }
        }
    }

    let mut reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(250));

    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        tokio::select! {
            event = reader.next().fuse() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        // Help modal: swallow all keys except ?/Esc
                        if app.show_help {
                            match key.code {
                                KeyCode::Char('?') | KeyCode::Esc => app.show_help = false,
                                _ => {} // swallow all other keys
                            }
                            continue;
                        }

                        // Global keys (work in any focus)
                        match key.code {
                            KeyCode::Char('q') if app.focus != Focus::Composer => {
                                app.session_manager.shutdown_all().await?;
                                let running_ids: Vec<String> = app.state.sessions.iter()
                                    .filter(|s| s.status == SessionStatus::Running)
                                    .map(|s| s.id.clone())
                                    .collect();
                                for sid in running_ids {
                                    let _ = app.state.update_session_status(&sid, SessionStatus::Stopped);
                                }
                                break;
                            }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                match app.focus {
                                    Focus::Composer => {
                                        // Ctrl+C in Composer always clears and stays in Composer
                                        app.composer.clear();
                                    }
                                    _ => {
                                        // Stop session if running, otherwise quit
                                        let should_quit = if let Some(ws) = app.selected_workspace().cloned() {
                                            let session_info = app.state.find_session_by_workspace(&ws.id)
                                                .filter(|s| s.status == SessionStatus::Running)
                                                .map(|s| s.id.clone());
                                            if let Some(session_id) = session_info {
                                                let _ = app.session_manager.stop_session(&session_id).await;
                                                let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                                app.focus = Focus::Tree;
                                                app.composer.set_active(false);
                                                if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
                                                    buf.push_line("--- Session stopped ---".to_string());
                                                }
                                                false
                                            } else {
                                                true
                                            }
                                        } else {
                                            true
                                        };
                                        if should_quit {
                                            app.session_manager.shutdown_all().await?;
                                            break;
                                        }
                                    }
                                }
                            }
                            KeyCode::Tab => {
                                let has_session = app.selected_workspace()
                                    .and_then(|ws| app.state.find_session_by_workspace(&ws.id))
                                    .is_some();
                                let has_running = app.selected_workspace()
                                    .and_then(|ws| app.state.find_session_by_workspace(&ws.id))
                                    .map(|s| s.status == SessionStatus::Running)
                                    .unwrap_or(false);
                                if app.zoomed {
                                    // In zoom mode, only cycle Output <-> Composer
                                    app.focus = match app.focus {
                                        Focus::Output if has_running => {
                                            app.composer.set_active(true);
                                            Focus::Composer
                                        }
                                        Focus::Composer => {
                                            app.composer.set_active(false);
                                            Focus::Output
                                        }
                                        _ => Focus::Output,
                                    };
                                } else {
                                    // Normal cycle: Tree -> Output -> Composer -> Tree
                                    app.focus = match app.focus {
                                        Focus::Tree if has_session => Focus::Output,
                                        Focus::Output if has_running => {
                                            app.composer.set_active(true);
                                            Focus::Composer
                                        }
                                        Focus::Output => Focus::Tree,
                                        Focus::Composer => {
                                            app.composer.set_active(false);
                                            Focus::Tree
                                        }
                                        _ => Focus::Tree,
                                    };
                                }
                            }
                            KeyCode::BackTab => {
                                let has_session = app.selected_workspace()
                                    .and_then(|ws| app.state.find_session_by_workspace(&ws.id))
                                    .is_some();
                                let has_running = app.selected_workspace()
                                    .and_then(|ws| app.state.find_session_by_workspace(&ws.id))
                                    .map(|s| s.status == SessionStatus::Running)
                                    .unwrap_or(false);
                                if app.zoomed {
                                    // In zoom mode, only cycle Output <-> Composer
                                    app.focus = match app.focus {
                                        Focus::Composer => {
                                            app.composer.set_active(false);
                                            Focus::Output
                                        }
                                        Focus::Output if has_running => {
                                            app.composer.set_active(true);
                                            Focus::Composer
                                        }
                                        _ => Focus::Output,
                                    };
                                } else {
                                    // Reverse cycle: Tree -> Composer -> Output -> Tree
                                    app.focus = match app.focus {
                                        Focus::Tree if has_running => {
                                            app.composer.set_active(true);
                                            Focus::Composer
                                        }
                                        Focus::Tree if has_session => Focus::Output,
                                        Focus::Output => Focus::Tree,
                                        Focus::Composer => {
                                            app.composer.set_active(false);
                                            Focus::Output
                                        }
                                        _ => Focus::Tree,
                                    };
                                }
                            }
                            KeyCode::Esc => {
                                if app.zoomed {
                                    app.zoomed = false;
                                    // Don't change focus when exiting zoom
                                } else {
                                    if app.focus == Focus::Composer {
                                        app.composer.set_active(false);
                                    }
                                    app.focus = Focus::Tree;
                                }
                            }
                            KeyCode::Char('?') if app.focus != Focus::Composer => {
                                app.show_help = !app.show_help;
                            }
                            _ => {
                                // Focus-specific keys
                                match app.focus {
                                    Focus::Composer => {
                                        if let Some(text) = app.composer.handle_key(key) {
                                            if let Some(session_id) = app.active_session_id.clone() {
                                                let ws_id = app.state.sessions.iter()
                                                    .find(|s| s.id == session_id)
                                                    .map(|s| s.workspace_id.clone());
                                                if let Some(ws_id) = ws_id {
                                                    let buf = app.scrollbacks
                                                        .entry(ws_id)
                                                        .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                                    buf.push_line(format!("> {}", text));
                                                    buf.push_line("---".to_string());
                                                }
                                                app.write_log(&session_id, "user", &text);
                                                let _ = app.session_manager.send_message(&session_id, &text).await;
                                            }
                                        }
                                    }
                                    Focus::Output => {
                                        // Output scrolling with j/k/arrows/PageUp/PageDown
                                        let ws_id = app.selected_workspace().map(|ws| ws.id.clone());
                                        if let Some(ws_id) = ws_id {
                                            match key.code {
                                                KeyCode::Up | KeyCode::Char('k') => {
                                                    if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                                                        buf.scroll_up(1);
                                                    }
                                                }
                                                KeyCode::Down | KeyCode::Char('j') => {
                                                    if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                                                        buf.scroll_down(1);
                                                    }
                                                }
                                                KeyCode::PageUp => {
                                                    let page = if app.last_output_height > 2 {
                                                        (app.last_output_height - 2) as usize
                                                    } else {
                                                        20
                                                    };
                                                    if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                                                        buf.scroll_up(page);
                                                    }
                                                }
                                                KeyCode::PageDown => {
                                                    let page = if app.last_output_height > 2 {
                                                        (app.last_output_height - 2) as usize
                                                    } else {
                                                        20
                                                    };
                                                    if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                                                        buf.scroll_down(page);
                                                    }
                                                }
                                                KeyCode::Char('g') | KeyCode::Home => {
                                                    // Jump to top
                                                    if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                                                        buf.scroll_to_top();
                                                    }
                                                }
                                                KeyCode::Char('G') | KeyCode::End => {
                                                    // Jump to bottom
                                                    if let Some(buf) = app.scrollbacks.get_mut(&ws_id) {
                                                        buf.reset_scroll();
                                                    }
                                                }
                                                KeyCode::Char('z') => {
                                                    app.zoomed = !app.zoomed;
                                                }
                                                KeyCode::Char('i') => {
                                                    // Enter composer from output
                                                    let has_running = app.selected_workspace()
                                                        .and_then(|ws| app.state.find_session_by_workspace(&ws.id))
                                                        .map(|s| s.status == SessionStatus::Running)
                                                        .unwrap_or(false);
                                                    if has_running {
                                                        app.focus = Focus::Composer;
                                                        app.composer.set_active(true);
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    Focus::Tree => {
                                        match key.code {
                                            KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                                            KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                                            KeyCode::Enter => {
                                                match app.tree_items.get(app.selected_index).cloned() {
                                                    Some(TreeNode::Repo { .. }) => app.toggle_expand(),
                                                    Some(TreeNode::Workspace { ws, .. }) => {
                                                        // Enter on workspace: start/resume session + focus Composer
                                                        let session = app.state.find_session_by_workspace(&ws.id)
                                                            .map(|s| (s.id.clone(), s.status.clone(), s.claude_session_id.clone()));
                                                        match session {
                                                            Some((_, SessionStatus::Running, _)) => {
                                                                // Already running: just focus Composer
                                                                app.focus = Focus::Composer;
                                                                app.composer.set_active(true);
                                                            }
                                                            Some((old_id, _, claude_sid)) => {
                                                                // Stopped/exited/failed: restart (same as R key)
                                                                let _ = app.state.update_session_status(&old_id, SessionStatus::Stopped);
                                                                if let Ok(new_session) = app.state.create_session(&ws.id) {
                                                                    let session_id = new_session.id.clone();
                                                                    match app.session_manager.start_session(
                                                                        &session_id,
                                                                        &ws.working_dir,
                                                                        claude_sid.as_deref(),
                                                                    ) {
                                                                        Ok(pid) => {
                                                                            if let Some(s) = app.state.find_session_mut(&session_id) {
                                                                                s.pid = Some(pid);
                                                                                s.claude_session_id = claude_sid;
                                                                            }
                                                                            let _ = app.state.save();
                                                                            app.active_session_id = Some(session_id);
                                                                            app.scrollbacks
                                                                                .entry(ws.id.clone())
                                                                                .or_insert_with(|| ScrollbackBuffer::new(50_000))
                                                                                .reset_scroll();
                                                                            app.focus = Focus::Composer;
                                                                            app.composer.set_active(true);
                                                                        }
                                                                        Err(_) => {
                                                                            let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            None => {
                                                                // No session: create + start (same as r key)
                                                                if let Ok(new_session) = app.state.create_session(&ws.id) {
                                                                    let session_id = new_session.id.clone();
                                                                    match app.session_manager.start_session(&session_id, &ws.working_dir, None) {
                                                                        Ok(pid) => {
                                                                            if let Some(s) = app.state.find_session_mut(&session_id) {
                                                                                s.pid = Some(pid);
                                                                            }
                                                                            let _ = app.state.save();
                                                                            app.scrollbacks
                                                                                .entry(ws.id.clone())
                                                                                .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                                                            app.active_session_id = Some(session_id);
                                                                            app.focus = Focus::Composer;
                                                                            app.composer.set_active(true);
                                                                        }
                                                                        Err(_) => {
                                                                            let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    Some(TreeNode::Hint { .. }) | None => {}
                                                }
                                            }
                                            KeyCode::Char('r') => {
                                                if let Some(ws) = app.selected_workspace().cloned() {
                                                    let has_running = app.state.find_session_by_workspace(&ws.id)
                                                        .map(|s| s.status == SessionStatus::Running)
                                                        .unwrap_or(false);
                                                    if !has_running {
                                                        match app.state.create_session(&ws.id) {
                                                            Ok(session) => {
                                                                let session_id = session.id.clone();
                                                                let ws_dir = ws.working_dir.clone();
                                                                match app.session_manager.start_session(&session_id, &ws_dir, None) {
                                                                    Ok(pid) => {
                                                                        if let Some(s) = app.state.find_session_mut(&session_id) {
                                                                            s.pid = Some(pid);
                                                                        }
                                                                        let _ = app.state.save();
                                                                        app.scrollbacks
                                                                            .entry(ws.id.clone())
                                                                            .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                                                        app.active_session_id = Some(session_id);
                                                                        // Stay in tree focus -- don't auto-focus composer
                                                                        app.focus = Focus::Output;
                                                                    }
                                                                    Err(_e) => {
                                                                        let _ = app.state.update_session_status(&session_id, SessionStatus::Failed);
                                                                    }
                                                                }
                                                            }
                                                            Err(_) => {}
                                                        }
                                                    }
                                                }
                                            }
                                            KeyCode::Char('x') | KeyCode::Delete => {
                                                if let Some(ws) = app.selected_workspace().cloned() {
                                                    let session_info = app.state.find_session_by_workspace(&ws.id)
                                                        .filter(|s| s.status == SessionStatus::Running)
                                                        .map(|s| s.id.clone());
                                                    if let Some(session_id) = session_info {
                                                        let _ = app.session_manager.stop_session(&session_id).await;
                                                        let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                                        if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
                                                            buf.push_line("--- Session stopped ---".to_string());
                                                        }
                                                    }
                                                }
                                            }
                                            KeyCode::Char('R') => {
                                                if let Some(ws) = app.selected_workspace().cloned() {
                                                    let session_info = app.state.find_session_by_workspace(&ws.id)
                                                        .filter(|s| s.status != SessionStatus::Running)
                                                        .map(|s| (s.id.clone(), s.claude_session_id.clone()));
                                                    if let Some((old_session_id, claude_sid)) = session_info {
                                                        // Stop old, create new state session, start with its ID
                                                        let _ = app.state.update_session_status(&old_session_id, SessionStatus::Stopped);
                                                        if let Ok(new_session) = app.state.create_session(&ws.id) {
                                                            let session_id = new_session.id.clone();
                                                            match app.session_manager.start_session(
                                                                &session_id,
                                                                &ws.working_dir,
                                                                claude_sid.as_deref(),
                                                            ) {
                                                                Ok(pid) => {
                                                                    if let Some(s) = app.state.find_session_mut(&session_id) {
                                                                        s.pid = Some(pid);
                                                                        s.claude_session_id = claude_sid.clone();
                                                                    }
                                                                    let _ = app.state.save();
                                                                    app.active_session_id = Some(session_id);
                                                                    app.scrollbacks
                                                                        .entry(ws.id.clone())
                                                                        .or_insert_with(|| ScrollbackBuffer::new(50_000))
                                                                        .reset_scroll();
                                                                    app.focus = Focus::Output;
                                                                }
                                                                Err(_) => {
                                                                    let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(_)) => break,
                    None => break,
                    _ => {}
                }
            }
            _ = tick_interval.tick() => {
                // Poll session events
                let events = app.session_manager.poll_events();
                for event in events {
                    match event {
                        SessionEvent::Output { session_id, line } => {
                            // Find workspace_id for this session
                            let ws_id = app.state.sessions.iter()
                                .find(|s| s.id == session_id)
                                .map(|s| s.workspace_id.clone());
                            if let Some(ws_id) = ws_id {
                                let buf = app.scrollbacks
                                    .entry(ws_id)
                                    .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                buf.push_line(line.clone());
                            }
                            // Write log
                            app.write_log(&session_id, "claude", &line);
                        }
                        SessionEvent::Exited { session_id, exit_code } => {
                            let status = match exit_code {
                                Some(0) | None => SessionStatus::Exited,
                                Some(_) => SessionStatus::Failed,
                            };
                            // Update claude_session_id from session_manager before removing
                            if let Some(csid) = app.session_manager.get_claude_session_id(&session_id) {
                                if let Some(s) = app.state.find_session_mut(&session_id) {
                                    s.claude_session_id = Some(csid);
                                }
                            }
                            let _ = app.state.update_session_status(&session_id, status);
                            // Push exit message to scrollback
                            let ws_id = app.state.sessions.iter()
                                .find(|s| s.id == session_id)
                                .map(|s| s.workspace_id.clone());
                            if let Some(ws_id) = ws_id {
                                let buf = app.scrollbacks
                                    .entry(ws_id)
                                    .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                let msg = match exit_code {
                                    Some(code) => format!("--- Session exited (code: {}) ---", code),
                                    None => "--- Session exited ---".to_string(),
                                };
                                buf.push_line(msg);
                            }
                            app.focus = Focus::Tree;
                            app.composer.set_active(false);
                        }
                        SessionEvent::ClaudeSessionId { session_id, claude_session_id } => {
                            if let Some(s) = app.state.find_session_mut(&session_id) {
                                if s.claude_session_id.is_none() {
                                    s.claude_session_id = Some(claude_session_id);
                                    let _ = app.state.save();
                                }
                            }
                        }
                        SessionEvent::Error { session_id, error } => {
                            let ws_id = app.state.sessions.iter()
                                .find(|s| s.id == session_id)
                                .map(|s| s.workspace_id.clone());
                            if let Some(ws_id) = ws_id {
                                let buf = app.scrollbacks
                                    .entry(ws_id)
                                    .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                buf.push_line(format!("[ERROR] {}", error));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn truncate_path(path: &str, max_width: usize) -> String {
    if path.len() <= max_width {
        return path.to_string();
    }
    if max_width < 4 {
        return "...".to_string();
    }
    let keep = max_width - 3;
    format!("...{}", &path[path.len() - keep..])
}

fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    if app.zoomed {
        render_zoomed(frame, app);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(frame.area());

        // Left pane: tree view
        render_tree(frame, app, chunks[0]);

        // Right pane: context-sensitive details or session view
        render_right_pane(frame, app, chunks[1]);
    }

    // Help overlay on top of any layout
    if app.show_help {
        help::render_help_overlay(frame, app.focus);
    }
}

fn render_tree(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    if app.tree_items.is_empty() {
        let border_style = if app.focus == Focus::Tree {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let hint = Paragraph::new("No repos tracked. Run: kmd repo add <path>")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(" Repos ").borders(Borders::ALL).border_style(border_style));
        frame.render_widget(hint, area);
    } else {
        let items: Vec<ListItem> = app
            .tree_items
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let is_selected = i == app.selected_index;
                match node {
                    TreeNode::Repo { id, name, .. } => {
                        let expanded = app.expanded.contains(id);
                        let indicator = if expanded { "\u{25BC} " } else { "\u{25B6} " };
                        let text = format!("{}{}", indicator, name);
                        let style = if is_selected && app.focus == Focus::Tree {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else if is_selected {
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        ListItem::new(Line::styled(text, style))
                    }
                    TreeNode::Workspace { ws, .. } => {
                        let (dot, dot_color) = if ws.active {
                            ("\u{25CF}", Color::Green)
                        } else {
                            ("\u{25CB}", Color::DarkGray)
                        };
                        let prefix = "  \u{251C}\u{2500} ";
                        let style = if is_selected && app.focus == Focus::Tree {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else if is_selected {
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD)
                        } else if !ws.active {
                            Style::default().fg(Color::DarkGray)
                        } else {
                            Style::default()
                        };
                        let mut spans = vec![
                            Span::styled(prefix, style),
                            Span::styled(dot, Style::default().fg(dot_color)),
                            Span::styled(format!(" {}", ws.name), style),
                        ];
                        // Session status icon
                        if let Some((icon, color)) = app.session_status_icon(&ws.id) {
                            spans.push(Span::styled(icon, Style::default().fg(color)));
                        }
                        ListItem::new(Line::from(spans))
                    }
                    TreeNode::Hint { text } => {
                        let display = format!("     {}", text);
                        let style = Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC);
                        ListItem::new(Line::styled(display, style))
                    }
                }
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(app.selected_index));

        let tree_border_style = if app.focus == Focus::Tree {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let list = List::new(items)
            .block(Block::default().title(" Repos ").borders(Borders::ALL).border_style(tree_border_style))
            .highlight_style(Style::default());

        frame.render_stateful_widget(list, area, &mut list_state);
    }
}

fn render_right_pane(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let right_width = area.width.saturating_sub(4) as usize;

    // Check if selected workspace has an active session (running, or stopped/exited with scrollback)
    let session_info = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Workspace { ws, .. }) => {
            app.state
                .find_session_by_workspace(&ws.id)
                .filter(|s| {
                    s.status == SessionStatus::Running
                        || app.scrollbacks.get(&ws.id).map_or(false, |b| !b.is_empty())
                })
                .map(|s| (ws.clone(), s.id.clone(), s.status.clone()))
        }
        _ => None,
    };

    if let Some((ws, _session_id, session_status)) = session_info {
        // Session view: output + composer
        let (status_str, status_color) = match session_status {
            SessionStatus::Running => ("\u{25B6}", Color::Green),
            SessionStatus::Stopped => ("\u{25A0}", Color::Yellow),
            SessionStatus::Failed => ("\u{2717}", Color::Red),
            SessionStatus::Exited => ("\u{2717}", Color::DarkGray),
        };
        let right_title = Line::from(vec![
            Span::raw(format!(" Workspace: {} ", ws.name)),
            Span::styled(status_str, Style::default().fg(status_color)),
            Span::raw(" "),
        ]);

        let composer_height = app.composer.height_hint();

        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(composer_height),
            ])
            .split(area);

        // Output area
        let output_area = right_chunks[0];
        app.last_output_height = output_area.height;
        let inner_height = output_area.height.saturating_sub(2) as usize; // account for borders

        let scrollback = app.scrollbacks.get(&ws.id);
        let visible: Vec<&str> = scrollback
            .map(|buf| buf.visible_lines(inner_height))
            .unwrap_or_default();

        render_output_content(frame, output_area, &visible, &session_status, app.focus, right_title);

        // Scrollbar
        if let Some(buf) = scrollback {
            render_scrollbar(frame, output_area, buf.total_lines(), inner_height, buf.clamped_offset(inner_height));
        }

        // New lines indicator when scrolled up
        if let Some(buf) = scrollback {
            if !buf.is_at_bottom() {
                let new_count = buf.new_lines_count();
                if new_count > 0 {
                    let indicator = format!(" \u{2193} {} new lines ", new_count);
                    let indicator_width = indicator.len() as u16;
                    let indicator_area = Rect::new(
                        output_area.x + output_area.width.saturating_sub(indicator_width + 2),
                        output_area.y + output_area.height.saturating_sub(1),
                        indicator_width,
                        1,
                    );
                    let indicator_widget = Paragraph::new(indicator)
                        .style(Style::default().fg(Color::Cyan));
                    frame.render_widget(indicator_widget, indicator_area);
                }
            }
        }

        // Composer area
        let composer_area = right_chunks[1];
        if session_status == SessionStatus::Running {
            frame.render_widget(app.composer.widget(), composer_area);
            // Char/line count overlay in bottom-right corner
            let status = app.composer.status_text();
            let status_width = status.len() as u16 + 1;
            if composer_area.width > status_width + 2 && composer_area.height > 0 {
                let status_area = Rect::new(
                    composer_area.x + composer_area.width.saturating_sub(status_width + 1),
                    composer_area.y + composer_area.height.saturating_sub(1),
                    status_width,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
                    status_area,
                );
            }
        } else {
            // Show hint to resume
            let hint = Paragraph::new("Press 'R' to resume session")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().title(" Composer ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
            frame.render_widget(hint, composer_area);
        }
    } else {
        // No session: show workspace/repo details (original behavior)
        let (right_title, right_content) = match app.tree_items.get(app.selected_index) {
            Some(TreeNode::Repo { id, name, .. }) => {
                let repo_path = app
                    .repos
                    .iter()
                    .find(|r| r.id == *id)
                    .map(|r| r.path.as_str())
                    .unwrap_or("unknown");
                let total: usize = app.workspaces.iter().filter(|w| w.repo_id == *id).count();
                let active: usize = app
                    .workspaces
                    .iter()
                    .filter(|w| w.repo_id == *id && w.active)
                    .count();

                let title = format!(" Repo: {} ", name);
                let lines = vec![
                    Line::from(vec![
                        Span::styled(
                            "Name: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Path: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(truncate_path(repo_path, right_width)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Workspaces: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format!("{} active, {} total", active, total)),
                    ]),
                ];
                (title, lines)
            }
            Some(TreeNode::Workspace { ws, repo_name }) => {
                let title = format!(" Workspace: {} ", ws.name);
                let status_span = if ws.active {
                    Span::styled("active", Style::default().fg(Color::Green))
                } else {
                    Span::styled("archived", Style::default().fg(Color::DarkGray))
                };
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(
                            "Name: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(ws.name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Repo: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(repo_name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Dir: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(truncate_path(&ws.working_dir, right_width)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Status: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        status_span,
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "Created: ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format_timestamp(ws.created_at)),
                    ]),
                ];
                // Hint for starting session
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Press 'r' to start a session",
                    Style::default().fg(Color::DarkGray),
                ));
                (title, lines)
            }
            Some(TreeNode::Hint { .. }) | None => {
                let title = " Details ".to_string();
                let lines = vec![Line::styled(
                    "Select a workspace to see details",
                    Style::default().fg(Color::DarkGray),
                )];
                (title, lines)
            }
        };

        let right_border = if app.focus == Focus::Output {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let paragraph = Paragraph::new(right_content)
            .block(Block::default().title(right_title).borders(Borders::ALL).border_style(right_border));
        frame.render_widget(paragraph, area);
    }
}

fn render_output_content(
    frame: &mut ratatui::Frame,
    output_area: Rect,
    visible: &[&str],
    session_status: &SessionStatus,
    focus: Focus,
    title: Line<'_>,
) {
    let inner_width = output_area.width.saturating_sub(2) as usize;

    let lines: Vec<Line> = if visible.is_empty() {
        if *session_status == SessionStatus::Running {
            vec![Line::styled(
                "Session started. Waiting for output...",
                Style::default().fg(Color::DarkGray),
            )]
        } else {
            vec![Line::styled(
                "No output.",
                Style::default().fg(Color::DarkGray),
            )]
        }
    } else {
        visible.iter().map(|l| {
            if l.starts_with("> ") {
                let content = &l[2..];
                let content_len = content.len();
                let avail = inner_width.saturating_sub(1);
                if content_len <= avail {
                    let padding = avail - content_len;
                    Line::from(vec![
                        Span::raw(" ".repeat(padding)),
                        Span::styled(content, Style::default().bg(Color::DarkGray)),
                    ])
                } else {
                    Line::styled(content.to_string(), Style::default().bg(Color::DarkGray))
                }
            } else if *l == "---" {
                Line::styled("---", Style::default().fg(Color::DarkGray))
            } else {
                Line::raw(*l)
            }
        }).collect()
    };

    let output_border_style = if focus == Focus::Output {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let output_block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(output_border_style);
    let paragraph = Paragraph::new(lines)
        .block(output_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, output_area);
}

fn render_zoomed(frame: &mut ratatui::Frame, app: &mut App) {
    let ws_info = app.selected_workspace().cloned().and_then(|ws| {
        app.state
            .find_session_by_workspace(&ws.id)
            .map(|s| (ws, s.id.clone(), s.status.clone()))
    });

    let Some((ws, _session_id, session_status)) = ws_info else {
        // No workspace selected; exit zoom
        app.zoomed = false;
        return;
    };

    let composer_height = if session_status == SessionStatus::Running {
        app.composer.height_hint()
    } else {
        3
    };

    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Output area
    let output_area = chunks[0];
    app.last_output_height = output_area.height;
    let inner_height = output_area.height.saturating_sub(2) as usize;

    let scrollback = app.scrollbacks.get(&ws.id);
    let visible: Vec<&str> = scrollback
        .map(|buf| buf.visible_lines(inner_height))
        .unwrap_or_default();

    let (status_str, status_color) = match session_status {
        SessionStatus::Running => ("\u{25B6}", Color::Green),
        SessionStatus::Stopped => ("\u{25A0}", Color::Yellow),
        SessionStatus::Failed => ("\u{2717}", Color::Red),
        SessionStatus::Exited => ("\u{2717}", Color::DarkGray),
    };
    let output_title = Line::from(vec![
        Span::raw(format!(" {} ", ws.name)),
        Span::styled(status_str, Style::default().fg(status_color)),
        Span::raw(" "),
    ]);

    render_output_content(frame, output_area, &visible, &session_status, app.focus, output_title);

    // Scrollbar
    if let Some(buf) = scrollback {
        render_scrollbar(frame, output_area, buf.total_lines(), inner_height, buf.clamped_offset(inner_height));
    }

    // Composer area
    let composer_area = chunks[1];
    if session_status == SessionStatus::Running {
        frame.render_widget(app.composer.widget(), composer_area);
        // Char/line count overlay
        let status = app.composer.status_text();
        let status_width = status.len() as u16 + 1;
        if composer_area.width > status_width + 2 && composer_area.height > 0 {
            let status_area = Rect::new(
                composer_area.x + composer_area.width.saturating_sub(status_width + 1),
                composer_area.y + composer_area.height.saturating_sub(1),
                status_width,
                1,
            );
            frame.render_widget(
                Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
                status_area,
            );
        }
    } else {
        let hint = Paragraph::new("Press 'R' to resume session")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(" Composer ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(hint, composer_area);
    }

    // Status bar (bottom line)
    let status_bar_area = chunks[2];
    let (bar_icon, bar_color) = match session_status {
        SessionStatus::Running => ("\u{25B6}", Color::Green),
        SessionStatus::Stopped => ("\u{25A0}", Color::Yellow),
        SessionStatus::Failed => ("\u{2717}", Color::Red),
        SessionStatus::Exited => ("\u{2717}", Color::DarkGray),
    };

    let scroll_info = if let Some(buf) = app.scrollbacks.get(&ws.id) {
        let total = buf.total_lines();
        let offset = buf.clamped_offset(inner_height);
        let current_line = total.saturating_sub(offset);
        format!("line {}/{}", current_line, total)
    } else {
        String::new()
    };

    let status_line = Line::from(vec![
        Span::styled(format!(" {} ", ws.name), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {} ", bar_icon), Style::default().fg(bar_color)),
        Span::raw(" "),
        Span::styled(scroll_info, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled("[z] exit zoom", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(
        Paragraph::new(status_line).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        status_bar_area,
    );
}

fn render_scrollbar(frame: &mut ratatui::Frame, area: Rect, total_lines: usize, viewport_height: usize, offset: usize) {
    if total_lines <= viewport_height || area.height < 3 {
        return;
    }
    let track_height = area.height.saturating_sub(2) as usize;
    if track_height == 0 {
        return;
    }
    let thumb_size = ((viewport_height as f64 / total_lines as f64) * track_height as f64).max(1.0) as usize;
    let max_offset = total_lines.saturating_sub(viewport_height);
    let thumb_pos = if max_offset == 0 {
        0
    } else {
        ((max_offset.saturating_sub(offset)) as f64 / max_offset as f64 * (track_height.saturating_sub(thumb_size)) as f64) as usize
    };
    for i in 0..track_height {
        let ch = if i >= thumb_pos && i < thumb_pos + thumb_size { "\u{2588}" } else { "\u{2502}" };
        let y = area.y + 1 + i as u16;
        let x = area.x + area.width.saturating_sub(2); // inside right border
        frame.render_widget(
            Paragraph::new(ch).style(Style::default().fg(Color::DarkGray)),
            Rect::new(x, y, 1, 1),
        );
    }
}
