mod buttons;
mod clipboard;
mod composer;
mod help;
mod modal;
mod mouse;
mod render;
mod scrollback;
mod selection;
mod session_manager;
mod wrap_map;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crossterm::event::{EventStream, KeyEventKind};
use futures::{FutureExt, StreamExt};
use kommand0_core::{AppState, RepoEntry, SessionStatus, Workspace};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{Event, KeyCode, KeyModifiers},
    style::Color,
};

use unicode_segmentation::UnicodeSegmentation;

use composer::Composer;
use scrollback::ScrollbackBuffer;
use selection::SelectionState;
use session_manager::{SessionEvent, SessionManager};
use wrap_map::WrapMap;

#[allow(dead_code)]
pub(crate) enum Status {
    Idle,
    Done,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Focus {
    Tree,
    Output,
    Composer,
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

#[allow(dead_code)]
pub(crate) struct App {
    pub(crate) repos: Vec<RepoEntry>,
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) state: AppState,
    pub(crate) expanded: HashSet<String>,
    pub(crate) tree_items: Vec<TreeNode>,
    pub(crate) selected_index: usize,
    pub(crate) status: Status,

    // Session fields
    pub(crate) session_manager: SessionManager,
    pub(crate) scrollbacks: HashMap<String, ScrollbackBuffer>,
    pub(crate) composer: Composer,
    pub(crate) active_session_id: Option<String>,
    pub(crate) focus: Focus,

    // UX state
    pub(crate) last_output_height: u16,
    pub(crate) show_help: bool,
    pub(crate) zoomed: bool,
    pub(crate) waiting_response: HashSet<String>,
    pub(crate) spinner_tick: u8,
    pub(crate) pane_areas: mouse::PaneAreas,
    pub(crate) mouse_pos: Option<(u16, u16)>,
    pub(crate) hit_regions: Vec<buttons::HitRegion>,
    pub(crate) pending_button_action: Option<buttons::HitAction>,
    pub(crate) modal: modal::ModalState,
    pub(crate) expanded_icon_rows: HashSet<String>,
    pub(crate) last_pane_width: u16,
    /// Accumulates streaming delta text per workspace until newlines flush to scrollback.
    pub(crate) streaming_text: HashMap<String, String>,
    tick_counter: u8,
    /// Per-workspace composer drafts so switching workspaces preserves unsent text.
    pub(crate) composer_drafts: HashMap<String, String>,

    // Selection/cursor state
    /// Per-workspace selection state (cursor position or range).
    pub(crate) selections: HashMap<String, SelectionState>,
    /// Per-workspace desired column for Up/Down movement across short lines.
    pub(crate) cursor_desired_col: HashMap<String, usize>,
    /// Cursor blink toggle -- flips every ~500ms.
    pub(crate) cursor_blink_on: bool,
    /// Workspaces where auto-scroll is suppressed (user placed cursor mid-document).
    pub(crate) auto_scroll_suppressed: HashSet<String>,
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
                                    for segment in content.split('\n') {
                                        buf.push_line(format!("> {}", segment));
                                    }
                                } else {
                                    for segment in content.split('\n') {
                                        buf.push_line(segment.to_string());
                                    }
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
            waiting_response: HashSet::new(),
            spinner_tick: 0,
            pane_areas: mouse::PaneAreas::default(),
            mouse_pos: None,
            hit_regions: Vec::new(),
            pending_button_action: None,
            modal: modal::ModalState::default(),
            expanded_icon_rows: HashSet::new(),
            last_pane_width: 0,
            streaming_text: HashMap::new(),
            tick_counter: 0,
            composer_drafts: HashMap::new(),
            selections: HashMap::new(),
            cursor_desired_col: HashMap::new(),
            cursor_blink_on: true,
            auto_scroll_suppressed: HashSet::new(),
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
        let old = self.selected_index;
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
        self.swap_composer_draft(old, next);
        self.selected_index = next;
        self.update_active_session();
    }

    pub(crate) fn move_down(&mut self) {
        if self.tree_items.is_empty() {
            return;
        }
        let old = self.selected_index;
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
        self.swap_composer_draft(old, next);
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
    pub(crate) fn update_active_session(&mut self) {
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
    pub(crate) fn selected_workspace(&self) -> Option<&Workspace> {
        match self.tree_items.get(self.selected_index) {
            Some(TreeNode::Workspace { ws, .. }) => Some(ws),
            _ => None,
        }
    }

    /// Get the workspace ID at the given tree index, if it's a workspace node.
    fn workspace_id_at(&self, index: usize) -> Option<String> {
        match self.tree_items.get(index) {
            Some(TreeNode::Workspace { ws, .. }) => Some(ws.id.clone()),
            _ => None,
        }
    }

    /// Save current composer draft for old workspace, restore draft for new workspace.
    fn swap_composer_draft(&mut self, old_index: usize, new_index: usize) {
        // Save draft from old workspace
        if let Some(old_ws_id) = self.workspace_id_at(old_index) {
            let draft = self.composer.draft_text();
            if draft.trim().is_empty() {
                self.composer_drafts.remove(&old_ws_id);
            } else {
                self.composer_drafts.insert(old_ws_id, draft);
            }
        }
        // Restore draft for new workspace
        if let Some(new_ws_id) = self.workspace_id_at(new_index) {
            if let Some(draft) = self.composer_drafts.get(&new_ws_id) {
                self.composer.set_text(draft);
            } else {
                self.composer.clear();
            }
        } else {
            self.composer.clear();
        }
    }

    /// Get session status icon for a workspace (used by detail pane)
    #[allow(dead_code)]
    pub(crate) fn session_status_icon(&self, workspace_id: &str) -> Option<(String, Color)> {
        const SPINNER: &[&str] = &["\u{28CB}","\u{2819}","\u{2839}","\u{2838}","\u{283C}","\u{2834}","\u{2826}","\u{2827}","\u{2807}","\u{280F}"];
        self.state
            .find_session_by_workspace(workspace_id)
            .map(|s| match s.status {
                SessionStatus::Running => {
                    if self.waiting_response.contains(workspace_id) {
                        let frame = SPINNER[self.spinner_tick as usize % SPINNER.len()];
                        (format!(" {}", frame), Color::Cyan)
                    } else {
                        (" \u{25B6}".to_string(), Color::Green) // ▶
                    }
                }
                SessionStatus::Stopped => (" \u{25A0}".to_string(), Color::Yellow),  // ■
                SessionStatus::Failed => (" \u{2717}".to_string(), Color::Red),      // ✗
                SessionStatus::Exited => (" \u{2717}".to_string(), Color::DarkGray), // ✗
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

    // --- Cursor movement and selection helpers ---

    /// Get the scrollback lines and inner width for the given workspace.
    /// Returns (owned_lines, inner_width) or None if unavailable.
    fn output_context(&self, ws_id: &str) -> Option<(Vec<String>, usize)> {
        let buf = self.scrollbacks.get(ws_id)?;
        let lines: Vec<String> = buf.all_lines().iter().map(|s| s.to_string()).collect();
        let inner_width = if self.pane_areas.output.width > 2 {
            (self.pane_areas.output.width - 2) as usize
        } else {
            80
        };
        Some((lines, inner_width))
    }

    /// Initialize cursor to bottom-left if not already set for this workspace.
    fn init_cursor_if_needed(&mut self, ws_id: &str) {
        if self.selections.get(ws_id).map_or(true, |s| s.is_none()) {
            if let Some(buf) = self.scrollbacks.get(ws_id) {
                let total = buf.total_lines();
                let line = if total > 0 { total - 1 } else { 0 };
                self.selections.insert(ws_id.to_string(), SelectionState::Cursor { line, char_offset: 0 });
                self.cursor_desired_col.insert(ws_id.to_string(), 0);
                self.auto_scroll_suppressed.insert(ws_id.to_string());
            }
        }
    }

    /// Get current cursor position from selection state.
    fn cursor_pos(&self, ws_id: &str) -> Option<(usize, usize)> {
        match self.selections.get(ws_id)? {
            SelectionState::Cursor { line, char_offset } => Some((*line, *char_offset)),
            SelectionState::Range { cursor_line, cursor_char, .. } => Some((*cursor_line, *cursor_char)),
            SelectionState::None => None,
        }
    }

    /// Set cursor position (collapse any range to cursor).
    fn set_cursor(&mut self, ws_id: &str, line: usize, char_offset: usize) {
        self.selections.insert(ws_id.to_string(), SelectionState::Cursor { line, char_offset });
    }

    /// Extend selection: if currently Cursor, anchor at current pos and move cursor to new_pos.
    /// If currently Range, keep anchor and move cursor.
    fn extend_selection(&mut self, ws_id: &str, new_line: usize, new_char: usize) {
        let current = self.selections.get(ws_id).cloned().unwrap_or_default();
        match current {
            SelectionState::Cursor { line, char_offset } => {
                self.selections.insert(ws_id.to_string(), SelectionState::Range {
                    anchor_line: line,
                    anchor_char: char_offset,
                    cursor_line: new_line,
                    cursor_char: new_char,
                });
            }
            SelectionState::Range { anchor_line, anchor_char, .. } => {
                self.selections.insert(ws_id.to_string(), SelectionState::Range {
                    anchor_line,
                    anchor_char,
                    cursor_line: new_line,
                    cursor_char: new_char,
                });
            }
            SelectionState::None => {
                self.selections.insert(ws_id.to_string(), SelectionState::Cursor {
                    line: new_line,
                    char_offset: new_char,
                });
            }
        }
    }

    /// Ensure cursor is visible, adjusting scroll_offset.
    fn ensure_cursor_visible(&mut self, ws_id: &str) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        let total_visual = wrap_map.total_visual_rows();
        let inner_height = if self.pane_areas.output.height > 2 {
            (self.pane_areas.output.height - 2) as usize
        } else {
            1
        };
        let max_scroll = total_visual.saturating_sub(inner_height);

        // Find cursor's visual row (using scroll_from_top=0 to get absolute visual row)
        if let Some((_x, y)) = wrap_map.logical_to_screen(cursor_line, cursor_char, 0, &lines_ref) {
            let buf = match self.scrollbacks.get_mut(ws_id) {
                Some(b) => b,
                None => return,
            };
            let clamped_offset = buf.scroll_offset().min(max_scroll);
            let scroll_from_top = max_scroll.saturating_sub(clamped_offset);

            if (y as usize) < scroll_from_top {
                // Cursor above viewport
                let new_offset = max_scroll.saturating_sub(y as usize);
                buf.set_scroll_offset(new_offset);
            } else if (y as usize) >= scroll_from_top + inner_height {
                // Cursor below viewport
                let new_scroll_from_top = (y as usize).saturating_sub(inner_height) + 1;
                let new_offset = max_scroll.saturating_sub(new_scroll_from_top);
                buf.set_scroll_offset(new_offset);
            }
        }
    }

    /// Move cursor vertically by `direction` visual rows (-1 = up, 1 = down).
    fn move_cursor_vertical(&mut self, ws_id: &str, direction: i32) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let wrap_map = WrapMap::build(&lines_ref, inner_width);

        // Get current screen position (absolute, scroll_from_top=0)
        if let Some((_cx, cy)) = wrap_map.logical_to_screen(cursor_line, cursor_char, 0, &lines_ref) {
            let desired_col = self.cursor_desired_col.get(ws_id).copied().unwrap_or(0);
            let new_y = if direction < 0 {
                (cy as usize).saturating_sub((-direction) as usize)
            } else {
                let total = wrap_map.total_visual_rows();
                ((cy as usize) + direction as usize).min(total.saturating_sub(1))
            };

            // Convert back to logical using desired_col as x
            if let Some((new_line, new_char)) = wrap_map.screen_to_logical(desired_col as u16, new_y as u16, 0, &lines_ref) {
                self.set_cursor(ws_id, new_line, new_char);
                // Do NOT update desired_col on vertical movement
                self.auto_scroll_suppressed.insert(ws_id.to_string());
                self.ensure_cursor_visible(ws_id);
            }
        }
    }

    /// Extend selection vertically by `direction` visual rows.
    fn extend_selection_vertical(&mut self, ws_id: &str, direction: i32) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let wrap_map = WrapMap::build(&lines_ref, inner_width);

        if let Some((_cx, cy)) = wrap_map.logical_to_screen(cursor_line, cursor_char, 0, &lines_ref) {
            let desired_col = self.cursor_desired_col.get(ws_id).copied().unwrap_or(0);
            let new_y = if direction < 0 {
                (cy as usize).saturating_sub((-direction) as usize)
            } else {
                let total = wrap_map.total_visual_rows();
                ((cy as usize) + direction as usize).min(total.saturating_sub(1))
            };

            if let Some((new_line, new_char)) = wrap_map.screen_to_logical(desired_col as u16, new_y as u16, 0, &lines_ref) {
                self.extend_selection(ws_id, new_line, new_char);
                self.auto_scroll_suppressed.insert(ws_id.to_string());
                self.ensure_cursor_visible(ws_id);
            }
        }
    }

    /// Move cursor horizontally by `direction` characters (-1 = left, 1 = right).
    fn move_cursor_horizontal(&mut self, ws_id: &str, direction: i32) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();

        let (new_line, new_char) = if direction > 0 {
            // Move right
            let line_text = lines_ref.get(cursor_line).copied().unwrap_or("");
            let grapheme_count = line_text.graphemes(true).count();
            if cursor_char + 1 < grapheme_count {
                (cursor_line, cursor_char + 1)
            } else if cursor_line + 1 < lines_ref.len() {
                // Wrap to start of next line
                (cursor_line + 1, 0)
            } else {
                (cursor_line, cursor_char)
            }
        } else {
            // Move left
            if cursor_char > 0 {
                (cursor_line, cursor_char - 1)
            } else if cursor_line > 0 {
                // Wrap to end of previous line
                let prev_text = lines_ref.get(cursor_line - 1).copied().unwrap_or("");
                let prev_count = prev_text.graphemes(true).count();
                (cursor_line - 1, prev_count.saturating_sub(1))
            } else {
                (cursor_line, cursor_char)
            }
        };

        self.set_cursor(ws_id, new_line, new_char);
        // Update desired_col on horizontal movement
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        if let Some((x, _y)) = wrap_map.logical_to_screen(new_line, new_char, 0, &lines_ref) {
            self.cursor_desired_col.insert(ws_id.to_string(), x as usize);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Extend selection horizontally.
    fn extend_selection_horizontal(&mut self, ws_id: &str, direction: i32) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();

        let (new_line, new_char) = if direction > 0 {
            let line_text = lines_ref.get(cursor_line).copied().unwrap_or("");
            let grapheme_count = line_text.graphemes(true).count();
            if cursor_char + 1 < grapheme_count {
                (cursor_line, cursor_char + 1)
            } else if cursor_line + 1 < lines_ref.len() {
                (cursor_line + 1, 0)
            } else {
                (cursor_line, cursor_char)
            }
        } else {
            if cursor_char > 0 {
                (cursor_line, cursor_char - 1)
            } else if cursor_line > 0 {
                let prev_text = lines_ref.get(cursor_line - 1).copied().unwrap_or("");
                let prev_count = prev_text.graphemes(true).count();
                (cursor_line - 1, prev_count.saturating_sub(1))
            } else {
                (cursor_line, cursor_char)
            }
        };

        self.extend_selection(ws_id, new_line, new_char);
        // Update desired_col on horizontal movement
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        if let Some((x, _y)) = wrap_map.logical_to_screen(new_line, new_char, 0, &lines_ref) {
            self.cursor_desired_col.insert(ws_id.to_string(), x as usize);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Move cursor to next word boundary.
    fn move_cursor_word_right(&mut self, ws_id: &str) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let line_text = lines_ref.get(cursor_line).copied().unwrap_or("");
        let new_char = next_word_boundary(line_text, cursor_char);
        let grapheme_count = line_text.graphemes(true).count();

        let (new_line, new_char) = if new_char >= grapheme_count && cursor_line + 1 < lines_ref.len() {
            (cursor_line + 1, 0)
        } else {
            (cursor_line, new_char.min(grapheme_count.saturating_sub(1)))
        };

        self.set_cursor(ws_id, new_line, new_char);
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        if let Some((x, _y)) = wrap_map.logical_to_screen(new_line, new_char, 0, &lines_ref) {
            self.cursor_desired_col.insert(ws_id.to_string(), x as usize);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Move cursor to previous word boundary.
    fn move_cursor_word_left(&mut self, ws_id: &str) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let line_text = lines_ref.get(cursor_line).copied().unwrap_or("");
        let new_char = prev_word_boundary(line_text, cursor_char);

        let (new_line, new_char) = if new_char == 0 && cursor_char == 0 && cursor_line > 0 {
            let prev_text = lines_ref.get(cursor_line - 1).copied().unwrap_or("");
            let prev_count = prev_text.graphemes(true).count();
            (cursor_line - 1, prev_count.saturating_sub(1))
        } else {
            (cursor_line, new_char)
        };

        self.set_cursor(ws_id, new_line, new_char);
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        if let Some((x, _y)) = wrap_map.logical_to_screen(new_line, new_char, 0, &lines_ref) {
            self.cursor_desired_col.insert(ws_id.to_string(), x as usize);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Extend selection by word to the right.
    fn extend_selection_word_right(&mut self, ws_id: &str) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let line_text = lines_ref.get(cursor_line).copied().unwrap_or("");
        let new_char = next_word_boundary(line_text, cursor_char);
        let grapheme_count = line_text.graphemes(true).count();

        let (new_line, new_char) = if new_char >= grapheme_count && cursor_line + 1 < lines_ref.len() {
            (cursor_line + 1, 0)
        } else {
            (cursor_line, new_char.min(grapheme_count.saturating_sub(1)))
        };

        self.extend_selection(ws_id, new_line, new_char);
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        if let Some((x, _y)) = wrap_map.logical_to_screen(new_line, new_char, 0, &lines_ref) {
            self.cursor_desired_col.insert(ws_id.to_string(), x as usize);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Extend selection by word to the left.
    fn extend_selection_word_left(&mut self, ws_id: &str) {
        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let line_text = lines_ref.get(cursor_line).copied().unwrap_or("");
        let new_char = prev_word_boundary(line_text, cursor_char);

        let (new_line, new_char) = if new_char == 0 && cursor_char == 0 && cursor_line > 0 {
            let prev_text = lines_ref.get(cursor_line - 1).copied().unwrap_or("");
            let prev_count = prev_text.graphemes(true).count();
            (cursor_line - 1, prev_count.saturating_sub(1))
        } else {
            (cursor_line, new_char)
        };

        self.extend_selection(ws_id, new_line, new_char);
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        if let Some((x, _y)) = wrap_map.logical_to_screen(new_line, new_char, 0, &lines_ref) {
            self.cursor_desired_col.insert(ws_id.to_string(), x as usize);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Move cursor to document start (line 0, char 0).
    fn move_cursor_to_document_start(&mut self, ws_id: &str) {
        self.set_cursor(ws_id, 0, 0);
        self.cursor_desired_col.insert(ws_id.to_string(), 0);
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Move cursor to document end (last line, char 0).
    fn move_cursor_to_document_end(&mut self, ws_id: &str) {
        if let Some(buf) = self.scrollbacks.get(ws_id) {
            let total = buf.total_lines();
            let line = if total > 0 { total - 1 } else { 0 };
            self.set_cursor(ws_id, line, 0);
            self.cursor_desired_col.insert(ws_id.to_string(), 0);
            // Going to the end re-enables auto-scroll
            self.auto_scroll_suppressed.remove(ws_id);
            self.ensure_cursor_visible(ws_id);
        }
    }

    /// Extend selection to document start.
    fn extend_selection_to_document_start(&mut self, ws_id: &str) {
        self.extend_selection(ws_id, 0, 0);
        self.cursor_desired_col.insert(ws_id.to_string(), 0);
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Extend selection to document end.
    fn extend_selection_to_document_end(&mut self, ws_id: &str) {
        if let Some((owned_lines, _)) = self.output_context(ws_id) {
            let last_line = if owned_lines.is_empty() { 0 } else { owned_lines.len() - 1 };
            let last_char = if let Some(line) = owned_lines.last() {
                line.graphemes(true).count().saturating_sub(1)
            } else {
                0
            };
            self.extend_selection(ws_id, last_line, last_char);
            self.cursor_desired_col.insert(ws_id.to_string(), 0);
            self.auto_scroll_suppressed.insert(ws_id.to_string());
            self.ensure_cursor_visible(ws_id);
        }
    }

    /// Page up: move cursor and viewport up by page size.
    fn move_cursor_page_up(&mut self, ws_id: &str) {
        let page_size = if self.last_output_height > 2 {
            (self.last_output_height - 2) as usize
        } else {
            20
        };

        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let wrap_map = WrapMap::build(&lines_ref, inner_width);

        if let Some((_cx, cy)) = wrap_map.logical_to_screen(cursor_line, cursor_char, 0, &lines_ref) {
            let desired_col = self.cursor_desired_col.get(ws_id).copied().unwrap_or(0);
            let new_y = (cy as usize).saturating_sub(page_size);

            if let Some((new_line, new_char)) = wrap_map.screen_to_logical(desired_col as u16, new_y as u16, 0, &lines_ref) {
                self.set_cursor(ws_id, new_line, new_char);
            }
        }

        // Also scroll viewport
        if let Some(buf) = self.scrollbacks.get_mut(ws_id) {
            buf.scroll_up(page_size);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Page down: move cursor and viewport down by page size.
    fn move_cursor_page_down(&mut self, ws_id: &str) {
        let page_size = if self.last_output_height > 2 {
            (self.last_output_height - 2) as usize
        } else {
            20
        };

        let (cursor_line, cursor_char) = match self.cursor_pos(ws_id) {
            Some(pos) => pos,
            None => return,
        };
        let (owned_lines, inner_width) = match self.output_context(ws_id) {
            Some(ctx) => ctx,
            None => return,
        };
        let lines_ref: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
        let wrap_map = WrapMap::build(&lines_ref, inner_width);
        let total = wrap_map.total_visual_rows();

        if let Some((_cx, cy)) = wrap_map.logical_to_screen(cursor_line, cursor_char, 0, &lines_ref) {
            let desired_col = self.cursor_desired_col.get(ws_id).copied().unwrap_or(0);
            let new_y = ((cy as usize) + page_size).min(total.saturating_sub(1));

            if let Some((new_line, new_char)) = wrap_map.screen_to_logical(desired_col as u16, new_y as u16, 0, &lines_ref) {
                self.set_cursor(ws_id, new_line, new_char);
            }
        }

        // Also scroll viewport
        if let Some(buf) = self.scrollbacks.get_mut(ws_id) {
            buf.scroll_down(page_size);
        }
        self.auto_scroll_suppressed.insert(ws_id.to_string());
        self.ensure_cursor_visible(ws_id);
    }

    /// Select all text in the output pane.
    fn select_all(&mut self, ws_id: &str) {
        if let Some((owned_lines, _)) = self.output_context(ws_id) {
            let last_line = if owned_lines.is_empty() { 0 } else { owned_lines.len() - 1 };
            let last_char = if let Some(line) = owned_lines.last() {
                line.graphemes(true).count().saturating_sub(1)
            } else {
                0
            };
            self.selections.insert(ws_id.to_string(), SelectionState::Range {
                anchor_line: 0,
                anchor_char: 0,
                cursor_line: last_line,
                cursor_char: last_char,
            });
        }
    }

    /// Scroll and clear selection (for j/k scroll shortcuts).
    fn scroll_and_clear_selection(&mut self, ws_id: &str, up: bool) {
        if let Some(buf) = self.scrollbacks.get_mut(ws_id) {
            if up {
                buf.scroll_up(1);
            } else {
                buf.scroll_down(1);
            }
        }
        self.clear_selection_for_workspace(ws_id);
    }

    /// Clear selection for a workspace.
    pub(crate) fn clear_selection_for_workspace(&mut self, ws_id: &str) {
        if let Some(sel) = self.selections.get_mut(ws_id) {
            sel.clear();
        }
    }

    /// Collapse range selection to cursor at cursor end (for plain arrow after selection).
    fn collapse_selection_to_cursor(&mut self, ws_id: &str) {
        if let Some(SelectionState::Range { cursor_line, cursor_char, .. }) = self.selections.get(ws_id).cloned() {
            self.set_cursor(ws_id, cursor_line, cursor_char);
        }
    }
}

/// Find the next word boundary position moving right from `char_offset` in `line`.
fn next_word_boundary(line: &str, char_offset: usize) -> usize {
    let graphemes: Vec<&str> = line.graphemes(true).collect();
    if char_offset >= graphemes.len() {
        return graphemes.len();
    }
    let mut i = char_offset;
    // Skip current word (non-whitespace)
    while i < graphemes.len() && !graphemes[i].chars().all(|c| c.is_whitespace()) {
        i += 1;
    }
    // Skip whitespace
    while i < graphemes.len() && graphemes[i].chars().all(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// Find the previous word boundary position moving left from `char_offset` in `line`.
fn prev_word_boundary(line: &str, char_offset: usize) -> usize {
    let graphemes: Vec<&str> = line.graphemes(true).collect();
    if char_offset == 0 {
        return 0;
    }
    let mut i = char_offset;
    // Skip whitespace
    while i > 0 && graphemes[i - 1].chars().all(|c| c.is_whitespace()) {
        i -= 1;
    }
    // Skip word
    while i > 0 && !graphemes[i - 1].chars().all(|c| c.is_whitespace()) {
        i -= 1;
    }
    i
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    // DISAMBIGUATE_ESCAPE_CODES lets terminals report Shift+Enter distinctly from Enter
    let supports_enhanced_keys = crossterm::terminal::supports_keyboard_enhancement()
        .unwrap_or(false);
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    if supports_enhanced_keys {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
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
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
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

    let mut resumed_workspace_ids: Vec<String> = Vec::new();

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
                    resumed_workspace_ids.push(ws_id);
                }
                Err(_) => {
                    let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                }
            }
        }
    }

    // Auto-expand repos with resumed sessions and select the first resumed workspace
    if !resumed_workspace_ids.is_empty() {
        for ws_id in &resumed_workspace_ids {
            if let Some(ws) = app.workspaces.iter().find(|w| w.id == *ws_id) {
                app.expanded.insert(ws.repo_id.clone());
            }
        }
        app.rebuild_tree();

        // Select the first resumed workspace in the tree
        let first_ws_id = &resumed_workspace_ids[0];
        for (i, node) in app.tree_items.iter().enumerate() {
            if let TreeNode::Workspace { ws, .. } = node {
                if ws.id == *first_ws_id {
                    app.selected_index = i;
                    break;
                }
            }
        }
        app.update_active_session();
        app.focus = Focus::Tree;
    }

    let mut reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(50));

    loop {
        terminal.draw(|frame| render::ui(frame, &mut app))?;

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

                        // Modal dialog: swallow all keys
                        if app.modal.is_active() {
                            match modal::handle_modal_key(&mut app.modal, key) {
                                modal::ModalResult::Consumed | modal::ModalResult::Cancelled => {}
                                modal::ModalResult::SubmitRepo(path) => {
                                    match app.state.add_repo(&path) {
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
                                    }
                                }
                                modal::ModalResult::ConfirmDelete(target) => {
                                    match target {
                                        modal::DeleteTarget::Workspace { name } => {
                                            // Gather IDs before mutating
                                            let ws_info = app.state.workspaces.iter()
                                                .find(|w| w.name == name)
                                                .map(|w| {
                                                    let sid = app.state.find_session_by_workspace(&w.id)
                                                        .filter(|s| s.status == SessionStatus::Running)
                                                        .map(|s| s.id.clone());
                                                    (w.id.clone(), sid)
                                                });
                                            if let Some((ws_id, running_sid)) = ws_info {
                                                if let Some(sid) = running_sid {
                                                    let _ = app.session_manager.stop_session(&sid).await;
                                                    let _ = app.state.update_session_status(&sid, SessionStatus::Stopped);
                                                }
                                                app.scrollbacks.remove(&ws_id);
                                            }
                                            let _ = app.state.delete_workspace(&name);
                                            app.workspaces = app.state.workspaces.clone();
                                            app.rebuild_tree();
                                            app.update_active_session();
                                        }
                                        modal::DeleteTarget::Repo { id, .. } => {
                                            // Stop all running sessions for this repo's workspaces
                                            let ws_ids: Vec<String> = app.state.workspaces.iter()
                                                .filter(|w| w.repo_id == id)
                                                .map(|w| w.id.clone())
                                                .collect();
                                            for ws_id in &ws_ids {
                                                if let Some(s) = app.state.find_session_by_workspace(ws_id) {
                                                    if s.status == SessionStatus::Running {
                                                        let sid = s.id.clone();
                                                        let _ = app.session_manager.stop_session(&sid).await;
                                                        let _ = app.state.update_session_status(&sid, SessionStatus::Stopped);
                                                    }
                                                }
                                                app.scrollbacks.remove(ws_id);
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
                                    let repo_name = app.repos.iter()
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
                                                        .entry(ws_id.clone())
                                                        .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                                    // Add separator before user message if there's prior content
                                                    if !buf.is_empty() {
                                                        buf.push_line("---".to_string());
                                                    }
                                                    for line in text.split('\n') {
                                                        buf.push_line(format!("> {}", line));
                                                    }
                                                    buf.push_line("---".to_string());
                                                    buf.reset_scroll(); // pin to bottom so response is visible
                                                    app.waiting_response.insert(ws_id);
                                                }
                                                app.write_log(&session_id, "user", &text);
                                                let _ = app.session_manager.send_message(&session_id, &text).await;
                                            }
                                        }
                                    }
                                    Focus::Output => {
                                        // Cursor movement, selection, and navigation
                                        let ws_id = app.selected_workspace().map(|ws| ws.id.clone());
                                        if let Some(ws_id) = ws_id {
                                            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                                            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                                            match (key.code, ctrl, shift) {
                                                // --- Cursor movement (no modifiers) ---
                                                (KeyCode::Up, false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.collapse_selection_to_cursor(&ws_id);
                                                    app.move_cursor_vertical(&ws_id, -1);
                                                }
                                                (KeyCode::Down, false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.collapse_selection_to_cursor(&ws_id);
                                                    app.move_cursor_vertical(&ws_id, 1);
                                                }
                                                (KeyCode::Left, false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.collapse_selection_to_cursor(&ws_id);
                                                    app.move_cursor_horizontal(&ws_id, -1);
                                                }
                                                (KeyCode::Right, false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.collapse_selection_to_cursor(&ws_id);
                                                    app.move_cursor_horizontal(&ws_id, 1);
                                                }

                                                // --- Word jump (Ctrl+arrow) ---
                                                (KeyCode::Left, true, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.collapse_selection_to_cursor(&ws_id);
                                                    app.move_cursor_word_left(&ws_id);
                                                }
                                                (KeyCode::Right, true, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.collapse_selection_to_cursor(&ws_id);
                                                    app.move_cursor_word_right(&ws_id);
                                                }

                                                // --- Selection extend (Shift+arrow) ---
                                                (KeyCode::Up, false, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_vertical(&ws_id, -1);
                                                }
                                                (KeyCode::Down, false, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_vertical(&ws_id, 1);
                                                }
                                                (KeyCode::Left, false, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_horizontal(&ws_id, -1);
                                                }
                                                (KeyCode::Right, false, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_horizontal(&ws_id, 1);
                                                }

                                                // --- Word-extend selection (Shift+Ctrl+arrow) ---
                                                (KeyCode::Left, true, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_word_left(&ws_id);
                                                }
                                                (KeyCode::Right, true, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_word_right(&ws_id);
                                                }

                                                // --- Document navigation ---
                                                (KeyCode::Home, _, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.move_cursor_to_document_start(&ws_id);
                                                }
                                                (KeyCode::End, _, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.move_cursor_to_document_end(&ws_id);
                                                }
                                                (KeyCode::Home, _, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_to_document_start(&ws_id);
                                                }
                                                (KeyCode::End, _, true) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.extend_selection_to_document_end(&ws_id);
                                                }

                                                // --- Page navigation ---
                                                (KeyCode::PageUp, false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.move_cursor_page_up(&ws_id);
                                                }
                                                (KeyCode::PageDown, false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.move_cursor_page_down(&ws_id);
                                                }

                                                // --- Select all ---
                                                (KeyCode::Char('a'), true, _) => {
                                                    app.select_all(&ws_id);
                                                }

                                                // --- Scroll-only shortcuts (j/k remain for scrolling) ---
                                                (KeyCode::Char('k'), false, false) => {
                                                    app.scroll_and_clear_selection(&ws_id, true);
                                                }
                                                (KeyCode::Char('j'), false, false) => {
                                                    app.scroll_and_clear_selection(&ws_id, false);
                                                }

                                                // --- Legacy shortcuts remapped ---
                                                (KeyCode::Char('g'), false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.move_cursor_to_document_start(&ws_id);
                                                }
                                                (KeyCode::Char('G'), false, false) => {
                                                    app.init_cursor_if_needed(&ws_id);
                                                    app.move_cursor_to_document_end(&ws_id);
                                                }

                                                // --- Existing ---
                                                (KeyCode::Char('z'), false, false) => {
                                                    app.zoomed = !app.zoomed;
                                                }
                                                (KeyCode::Char('i'), false, false) => {
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
                                                        let ws_count = app.workspaces.iter()
                                                            .filter(|w| w.repo_id == id)
                                                            .count();
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
                                                        let ws_info = app.state.workspaces.iter()
                                                            .find(|w| w.name == ws.name)
                                                            .map(|w| {
                                                                let sid = app.state.find_session_by_workspace(&w.id)
                                                                    .filter(|s| s.status == SessionStatus::Running)
                                                                    .map(|s| s.id.clone());
                                                                (w.id.clone(), sid)
                                                            });
                                                        if let Some((ws_id, running_sid)) = ws_info {
                                                            if let Some(sid) = running_sid {
                                                                let _ = app.session_manager.stop_session(&sid).await;
                                                                let _ = app.state.update_session_status(&sid, SessionStatus::Stopped);
                                                            }
                                                            app.scrollbacks.remove(&ws_id);
                                                        }
                                                        let _ = app.state.delete_workspace(&ws.name);
                                                        app.workspaces = app.state.workspaces.clone();
                                                        app.rebuild_tree();
                                                        app.update_active_session();
                                                    }
                                                    Some(TreeNode::Repo { id, .. }) => {
                                                        let ws_ids: Vec<String> = app.state.workspaces.iter()
                                                            .filter(|w| w.repo_id == id)
                                                            .map(|w| w.id.clone())
                                                            .collect();
                                                        for ws_id in &ws_ids {
                                                            if let Some(s) = app.state.find_session_by_workspace(ws_id) {
                                                                if s.status == SessionStatus::Running {
                                                                    let sid = s.id.clone();
                                                                    let _ = app.session_manager.stop_session(&sid).await;
                                                                    let _ = app.state.update_session_status(&sid, SessionStatus::Stopped);
                                                                }
                                                            }
                                                            app.scrollbacks.remove(ws_id);
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
                    }
                    Some(Ok(Event::Mouse(mouse_event))) => {
                        if !app.show_help {
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
                        // Detail-pane buttons (use selected workspace)
                        buttons::HitAction::StartSession => {
                            if let Some(ws) = app.selected_workspace().cloned() {
                                let claude_sid: Option<String> = None;
                                if let Ok(new_session) = app.state.create_session(&ws.id) {
                                    let session_id = new_session.id.clone();
                                    match app.session_manager.start_session(&session_id, &ws.working_dir, claude_sid.as_deref()) {
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
                        }
                        buttons::HitAction::ResumeSession => {
                            if let Some(ws) = app.selected_workspace().cloned() {
                                let claude_sid = app.state.find_session_by_workspace(&ws.id)
                                    .and_then(|s| s.claude_session_id.clone());
                                if let Some(old_id) = app.state.find_session_by_workspace(&ws.id).map(|s| s.id.clone()) {
                                    let _ = app.state.update_session_status(&old_id, SessionStatus::Stopped);
                                }
                                if let Ok(new_session) = app.state.create_session(&ws.id) {
                                    let session_id = new_session.id.clone();
                                    match app.session_manager.start_session(&session_id, &ws.working_dir, claude_sid.as_deref()) {
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
                        }
                        buttons::HitAction::StopSession => {
                            if let Some(ws) = app.selected_workspace().cloned() {
                                if let Some(session_id) = app.state.find_session_by_workspace(&ws.id)
                                    .filter(|s| s.status == SessionStatus::Running)
                                    .map(|s| s.id.clone())
                                {
                                    let _ = app.session_manager.stop_session(&session_id).await;
                                    let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                    if let Some(buf) = app.scrollbacks.get_mut(&ws.id) {
                                        buf.push_line("--- Session stopped ---".to_string());
                                    }
                                    app.focus = Focus::Tree;
                                    app.composer.set_active(false);
                                }
                            }
                        }
                        // Tree-icon buttons (use carried workspace_id, do NOT change focus)
                        buttons::HitAction::StartSessionFor { workspace_id } => {
                            if let Some(ws) = app.state.workspaces.iter().find(|w| w.id == workspace_id).cloned() {
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
                                                .or_insert_with(|| ScrollbackBuffer::new(50_000))
                                                .reset_scroll();
                                            app.update_active_session();
                                        }
                                        Err(_) => {
                                            let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                                        }
                                    }
                                }
                            }
                        }
                        buttons::HitAction::StopSessionFor { workspace_id } => {
                            if let Some(session_id) = app.state.find_session_by_workspace(&workspace_id)
                                .filter(|s| s.status == SessionStatus::Running)
                                .map(|s| s.id.clone())
                            {
                                let _ = app.session_manager.stop_session(&session_id).await;
                                let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                if let Some(buf) = app.scrollbacks.get_mut(&workspace_id) {
                                    buf.push_line("--- Session stopped ---".to_string());
                                }
                                app.update_active_session();
                            }
                        }
                        buttons::HitAction::ResumeSessionFor { workspace_id } => {
                            if let Some(ws) = app.state.workspaces.iter().find(|w| w.id == workspace_id).cloned() {
                                let claude_sid = app.state.find_session_by_workspace(&ws.id)
                                    .and_then(|s| s.claude_session_id.clone());
                                if let Some(old_id) = app.state.find_session_by_workspace(&ws.id).map(|s| s.id.clone()) {
                                    let _ = app.state.update_session_status(&old_id, SessionStatus::Stopped);
                                }
                                if let Ok(new_session) = app.state.create_session(&ws.id) {
                                    let session_id = new_session.id.clone();
                                    match app.session_manager.start_session(&session_id, &ws.working_dir, claude_sid.as_deref()) {
                                        Ok(pid) => {
                                            if let Some(s) = app.state.find_session_mut(&session_id) {
                                                s.pid = Some(pid);
                                                s.claude_session_id = claude_sid;
                                            }
                                            let _ = app.state.save();
                                            app.scrollbacks
                                                .entry(ws.id.clone())
                                                .or_insert_with(|| ScrollbackBuffer::new(50_000))
                                                .reset_scroll();
                                            app.update_active_session();
                                        }
                                        Err(_) => {
                                            let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                                        }
                                    }
                                }
                            }
                        }
                        buttons::HitAction::RetrySessionFor { workspace_id } => {
                            if let Some(ws) = app.state.workspaces.iter().find(|w| w.id == workspace_id).cloned() {
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
                                                .or_insert_with(|| ScrollbackBuffer::new(50_000))
                                                .reset_scroll();
                                            app.update_active_session();
                                        }
                                        Err(_) => {
                                            let _ = app.state.update_session_status(&new_session.id, SessionStatus::Failed);
                                        }
                                    }
                                }
                            }
                        }
                        buttons::HitAction::FocusComposerFor { workspace_id } => {
                            // Focus the composer for the given workspace's running session
                            if app.state.find_session_by_workspace(&workspace_id)
                                .map(|s| s.status == SessionStatus::Running)
                                .unwrap_or(false)
                            {
                                // Navigate selection to this workspace
                                let old = app.selected_index;
                                for (i, node) in app.tree_items.iter().enumerate() {
                                    if let TreeNode::Workspace { ws, .. } = node {
                                        if ws.id == workspace_id {
                                            app.swap_composer_draft(old, i);
                                            app.selected_index = i;
                                            break;
                                        }
                                    }
                                }
                                app.update_active_session();
                                app.focus = Focus::Composer;
                                app.composer.set_active(true);
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
                                    let _ = app.session_manager.stop_session(&session_id).await;
                                    let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                }
                                if let Ok(_) = app.state.delete_workspace(&ws.name) {
                                    app.scrollbacks.remove(&workspace_id);
                                    app.waiting_response.remove(&workspace_id);
                                    app.expanded_icon_rows.remove(&workspace_id);
                                    app.composer_drafts.remove(&workspace_id);
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
                                        let _ = app.session_manager.stop_session(&session_id).await;
                                        let _ = app.state.update_session_status(&session_id, SessionStatus::Stopped);
                                    }
                                    app.scrollbacks.remove(ws_id);
                                    app.waiting_response.remove(ws_id);
                                    app.expanded_icon_rows.remove(ws_id);
                                    app.composer_drafts.remove(ws_id);
                                }
                                if let Ok(_) = app.state.delete_repo(&repo_name) {
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
                // Advance spinner animation (every 5th tick = ~250ms at 50ms interval)
                app.tick_counter = app.tick_counter.wrapping_add(1);
                if app.tick_counter % 5 == 0 {
                    app.spinner_tick = (app.spinner_tick + 1) % 10;
                }
                // Cursor blink toggle every 10th tick (~500ms at 50ms interval)
                if app.tick_counter % 10 == 0 {
                    app.cursor_blink_on = !app.cursor_blink_on;
                }

                // Poll session events
                let events = app.session_manager.poll_events();
                for event in events {
                    match event {
                        SessionEvent::StreamDelta { session_id, text } => {
                            let ws_id = app.state.sessions.iter()
                                .find(|s| s.id == session_id)
                                .map(|s| s.workspace_id.clone());
                            if let Some(ws_id) = ws_id {
                                // Clear waiting/thinking on first delta
                                app.waiting_response.remove(&ws_id);

                                let stream_buf = app.streaming_text
                                    .entry(ws_id.clone())
                                    .or_default();
                                stream_buf.push_str(&text);

                                // Flush completed lines (up to last \n) to scrollback
                                let buf = app.scrollbacks
                                    .entry(ws_id.clone())
                                    .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                let stream = app.streaming_text.get_mut(&ws_id).unwrap();
                                while let Some(nl) = stream.find('\n') {
                                    let line = stream[..nl].to_string();
                                    buf.push_line(line);
                                    *stream = stream[nl + 1..].to_string();
                                }
                                // Remaining partial text stays in streaming_text
                                // and will be shown as an in-progress line by the renderer
                            }
                            app.write_log(&session_id, "claude", &text);
                        }
                        SessionEvent::StreamEnd { session_id } => {
                            let ws_id = app.state.sessions.iter()
                                .find(|s| s.id == session_id)
                                .map(|s| s.workspace_id.clone());
                            if let Some(ws_id) = ws_id {
                                // Flush any remaining partial line
                                if let Some(remaining) = app.streaming_text.remove(&ws_id) {
                                    if !remaining.is_empty() {
                                        let buf = app.scrollbacks
                                            .entry(ws_id)
                                            .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                        buf.push_line(remaining);
                                    }
                                }
                            }
                        }
                        SessionEvent::Output { session_id, line, source } => {
                            // Complete message (non-streaming content or stderr).
                            let ws_id = app.state.sessions.iter()
                                .find(|s| s.id == session_id)
                                .map(|s| s.workspace_id.clone());
                            if let Some(ws_id) = ws_id {
                                let buf = app.scrollbacks
                                    .entry(ws_id.clone())
                                    .or_insert_with(|| ScrollbackBuffer::new(50_000));
                                for segment in line.split('\n') {
                                    buf.push_line(segment.to_string());
                                }
                                // Clear "Thinking..." for stdout content (actual responses).
                                // Don't clear for stderr (CLI warnings, version info).
                                if !line.is_empty() && source == session_manager::OutputSource::Stdout {
                                    app.waiting_response.remove(&ws_id);
                                }
                            }
                            app.write_log(&session_id, "claude", &line);
                        }
                        SessionEvent::Exited { session_id, exit_code } => {
                            let status = match exit_code {
                                Some(0) | None => SessionStatus::Exited,
                                Some(_) => SessionStatus::Failed,
                            };
                            // Update claude_session_id from session_manager before removing.
                            // If the process never produced a session_id (e.g. --resume with a
                            // stale ID caused immediate exit), clear the stored one so the next
                            // attempt starts fresh instead of repeating the same failure.
                            // BUT: don't clear if session was explicitly stopped (status already
                            // Stopped) — stop_session() removes from manager before Exited arrives,
                            // so get_claude_session_id() returns None even though the ID is valid.
                            if let Some(csid) = app.session_manager.get_claude_session_id(&session_id) {
                                if let Some(s) = app.state.find_session_mut(&session_id) {
                                    s.claude_session_id = Some(csid);
                                }
                            } else {
                                let already_stopped = app.state.sessions.iter()
                                    .find(|s| s.id == session_id)
                                    .map(|s| s.status == SessionStatus::Stopped)
                                    .unwrap_or(false);
                                if !already_stopped {
                                    if let Some(s) = app.state.find_session_mut(&session_id) {
                                        s.claude_session_id = None;
                                    }
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

