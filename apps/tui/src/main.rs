use std::io::stdout;

use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use kommand0_core::{AppState, RepoEntry, run_git_status};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    crossterm::event::{self, Event, KeyCode},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

enum Status {
    Idle,
    Done,
    Error(String),
}

struct App {
    repos: Vec<RepoEntry>,
    selected: ListState,
    output: String,
    status: Status,
}

impl App {
    fn new(repos: Vec<RepoEntry>) -> Self {
        let mut selected = ListState::default();
        if !repos.is_empty() {
            selected.select(Some(0));
        }
        Self {
            repos,
            selected,
            output: String::new(),
            status: Status::Idle,
        }
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected.selected()
    }

    fn move_up(&mut self) {
        if self.repos.is_empty() {
            return;
        }
        let i = self.selected_index().unwrap_or(0);
        let next = if i == 0 { self.repos.len() - 1 } else { i - 1 };
        self.selected.select(Some(next));
    }

    fn move_down(&mut self) {
        if self.repos.is_empty() {
            return;
        }
        let i = self.selected_index().unwrap_or(0);
        let next = if i >= self.repos.len() - 1 { 0 } else { i + 1 };
        self.selected.select(Some(next));
    }

    fn run_status(&mut self) {
        if let Some(i) = self.selected_index() {
            let repo = &self.repos[i];
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
}

fn main() -> anyhow::Result<()> {
    let state = AppState::load()?;
    let mut app = App::new(state.repos);

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(frame.area());

            // Left pane: repo list
            let items: Vec<ListItem> = app
                .repos
                .iter()
                .map(|r| ListItem::new(Line::raw(&r.name)))
                .collect();

            let list = List::new(items)
                .block(Block::default().title(" Repos ").borders(Borders::ALL))
                .highlight_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> ");

            frame.render_stateful_widget(list, chunks[0], &mut app.selected);

            // Right pane: output
            let status_title = match &app.status {
                Status::Idle => " Output ",
                Status::Done => " Output (done) ",
                Status::Error(e) => {
                    // Truncate error for title; full error could go in body
                    if e.len() > 40 {
                        " Output (error) "
                    } else {
                        " Output (error) "
                    }
                }
            };

            let output_text = match &app.status {
                Status::Error(e) => e.as_str(),
                _ => app.output.as_str(),
            };

            let paragraph = Paragraph::new(output_text)
                .block(Block::default().title(status_title).borders(Borders::ALL));

            frame.render_widget(paragraph, chunks[1]);
        })?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Enter => app.run_status(),
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}
