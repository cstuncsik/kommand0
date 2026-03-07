mod composer;
mod scrollback;
mod session_manager;

use std::collections::HashSet;
use std::time::Duration;

use crossterm::event::{EventStream, KeyEventKind};
use futures::{FutureExt, StreamExt};
use kommand0_core::{AppState, RepoEntry, Workspace, run_git_status, workspace::format_timestamp};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{Event, KeyCode},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

enum Status {
    Idle,
    Done,
    #[allow(dead_code)]
    Error(String),
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

struct App {
    repos: Vec<RepoEntry>,
    workspaces: Vec<Workspace>,
    expanded: HashSet<String>,
    tree_items: Vec<TreeNode>,
    selected_index: usize,
    output: String,
    status: Status,
}

impl App {
    fn new(repos: Vec<RepoEntry>, workspaces: Vec<Workspace>) -> Self {
        let mut app = Self {
            repos,
            workspaces,
            expanded: HashSet::new(),
            tree_items: Vec::new(),
            selected_index: 0,
            output: String::new(),
            status: Status::Idle,
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
        // Skip hint nodes, with wrap-around protection
        let mut attempts = 0;
        while self.is_hint(next) && attempts < len {
            next = if next == 0 { len - 1 } else { next - 1 };
            attempts += 1;
        }
        self.selected_index = next;
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
        // Skip hint nodes, with wrap-around protection
        let mut attempts = 0;
        while self.is_hint(next) && attempts < len {
            next = if next >= len - 1 { 0 } else { next + 1 };
            attempts += 1;
        }
        self.selected_index = next;
    }

    fn toggle_expand(&mut self) {
        if let Some(TreeNode::Repo { id, .. }) = self.tree_items.get(self.selected_index) {
            let id = id.clone();
            if self.expanded.contains(&id) {
                self.expanded.remove(&id);
            } else {
                self.expanded.insert(id.clone());
            }
            // Find which repo index this is so we can keep selection on it
            let repo_id = id;
            self.rebuild_tree();
            // Find the repo node in the rebuilt tree
            for (i, node) in self.tree_items.iter().enumerate() {
                if let TreeNode::Repo { id, .. } = node {
                    if *id == repo_id {
                        self.selected_index = i;
                        break;
                    }
                }
            }
            // Clamp
            if !self.tree_items.is_empty() {
                self.selected_index = self.selected_index.min(self.tree_items.len() - 1);
            }
        }
    }

    fn run_status_for_repo_id(&mut self, repo_id: &str) {
        if let Some(repo) = self.repos.iter().find(|r| r.id == repo_id) {
            match run_git_status(&repo.path) {
                Ok(out) => {
                    self.output = out;
                    self.status = Status::Done;
                }
                Err(e) => {
                    self.output = String::new();
                    self.status = Status::Error(e.to_string());
                }
            }
        }
    }

    fn handle_enter(&mut self) {
        match self.tree_items.get(self.selected_index).cloned() {
            Some(TreeNode::Repo { .. }) => self.toggle_expand(),
            Some(TreeNode::Workspace { ws, .. }) => {
                let repo_id = ws.repo_id.clone();
                self.run_status_for_repo_id(&repo_id);
            }
            Some(TreeNode::Hint { .. }) | None => {}
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
    let mut app = App::new(state.repos, state.workspaces);

    let mut reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(250));

    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        tokio::select! {
            event = reader.next().fuse() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                            KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                            KeyCode::Enter => app.handle_enter(),
                            _ => {}
                        }
                    }
                    Some(Err(_)) => break,
                    None => break,
                    _ => {}
                }
            }
            _ = tick_interval.tick() => {
                // Future: refresh UI, poll background tasks
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
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(frame.area());

    // Left pane: tree view
    if app.tree_items.is_empty() {
        let hint = Paragraph::new("No repos tracked. Run: kmd repo add <path>")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(" Repos ").borders(Borders::ALL));
        frame.render_widget(hint, chunks[0]);
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
                        let style = if is_selected {
                            Style::default()
                                .fg(Color::Yellow)
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
                        let style = if is_selected {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else if !ws.active {
                            Style::default().fg(Color::DarkGray)
                        } else {
                            Style::default()
                        };
                        let spans = vec![
                            Span::styled(prefix, style),
                            Span::styled(dot, Style::default().fg(dot_color)),
                            Span::styled(format!(" {}", ws.name), style),
                        ];
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

        let list = List::new(items)
            .block(Block::default().title(" Repos ").borders(Borders::ALL))
            .highlight_style(Style::default());

        frame.render_stateful_widget(list, chunks[0], &mut list_state);
    }

    // Right pane: context-sensitive details
    let right_width = chunks[1].width.saturating_sub(4) as usize;

    let (right_title, right_content) = match app.tree_items.get(app.selected_index) {
        Some(TreeNode::Repo { id, name, .. }) => {
            // Find the repo for path info
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
            let lines = vec![
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

    let paragraph = Paragraph::new(right_content)
        .block(Block::default().title(right_title).borders(Borders::ALL));
    frame.render_widget(paragraph, chunks[1]);
}
