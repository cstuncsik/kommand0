//! Command palette: a flat, fuzzy list over every workspace (across all repos,
//! regardless of tree expand state) *and* the actions you can run on them —
//! jump-and-open, clean up, archive/activate, new session, and jump to a
//! specific session tab. Opened with `:`; Enter runs the selection.
//!
//! This is presentation logic, so it lives in the TUI (not core). The fuzzy
//! ranker is a small hand-rolled subsequence scorer — no dependency, and pure
//! (so it's trivially unit-testable).

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::modal::{render_input_with_cursor, sanitize_paste};
use super::theme::Theme;

/// What running a palette entry does. Dispatched by the app once the palette
/// closes; every variant carries the workspace it targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    /// Reveal + open the workspace's embedded session (the original jump).
    OpenWorkspace { ws_id: String },
    Cleanup { ws_id: String },
    /// Archive an active workspace, or re-activate an archived one.
    ArchiveToggle { ws_id: String },
    NewSession { ws_id: String },
    /// Switch to session tab `index` of an already-open workspace.
    JumpTab { ws_id: String, index: usize },
}

/// One palette entry: the display label + muted detail, the text the query is
/// scored against (label + workspace + branch + repo, so any of them narrows),
/// and the action Enter runs.
pub(crate) struct Candidate {
    pub label: String,
    pub detail: String,
    pub match_text: String,
    pub action: PaletteAction,
}

/// Live palette state: the typed query, the ranked result indices (into
/// `candidates`), and the selected row. Owns its candidate snapshot, taken when
/// the palette opens (the list can't change while the palette captures keys).
pub(crate) struct Palette {
    candidates: Vec<Candidate>,
    pub query: String,
    /// Index into `results` (NOT into `candidates`).
    pub selected: usize,
    /// Ranked indices into `candidates`, best first.
    pub results: Vec<usize>,
}

impl Palette {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        let mut p = Palette { candidates, query: String::new(), selected: 0, results: Vec::new() };
        p.rerank();
        p
    }

    /// Recompute `results` for the current query, then clamp `selected`.
    fn rerank(&mut self) {
        let q = self.query.trim();
        let mut scored: Vec<(usize, i32)> = if q.is_empty() {
            // Empty query: everything matches, in input order.
            (0..self.candidates.len()).map(|i| (i, 0)).collect()
        } else {
            self.candidates
                .iter()
                .enumerate()
                .filter_map(|(i, c)| score(q, &c.match_text).map(|s| (i, s)))
                .collect()
        };
        // Best score first; stable tiebreak on the original index.
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        self.results = scored.into_iter().map(|(i, _)| i).collect();
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0; // a new keystroke re-ranks; land on the best match
        self.rerank();
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.rerank();
    }

    /// Append pasted text to the query (sanitized for a single line via
    /// [`sanitize_paste`]). Reranks once for the whole paste, not per char; a
    /// paste that sanitizes to nothing is a no-op and keeps the selection.
    pub fn paste(&mut self, text: &str) {
        let clean = sanitize_paste(text);
        if clean.is_empty() {
            return;
        }
        self.query.push_str(&clean);
        self.selected = 0;
        self.rerank();
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    /// The action under the current selection, if any.
    pub fn selected_action(&self) -> Option<&PaletteAction> {
        self.results
            .get(self.selected)
            .and_then(|&i| self.candidates.get(i))
            .map(|c| &c.action)
    }
}

/// Fuzzy subsequence score: `query` must appear in `candidate` as an ordered
/// (case-insensitive) subsequence. Higher is better — bonuses for matches at a
/// word boundary, contiguous runs, and earlier positions. `None` if `query`
/// isn't a subsequence. Greedy (first-match) — good enough for a short jump
/// list, and cheap.
pub(crate) fn score(query: &str, candidate: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().collect();
    let mut qi = 0;
    let mut total = 0i32;
    let mut prev_matched = false;
    for (ci, &ch) in cand.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if ch.to_ascii_lowercase() == q[qi] {
            total += 1; // base point per matched char
            let at_boundary =
                ci == 0 || matches!(cand[ci - 1], '-' | '_' | '/' | ' ' | '.');
            if at_boundary {
                total += 8; // start of a word — the strongest signal
            }
            if prev_matched {
                total += 4; // contiguous run
            }
            if ci < 16 {
                total += (16 - ci as i32) / 4; // mild earliness bonus
            }
            qi += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
    }
    (qi == q.len()).then_some(total)
}

/// Centered popup rect (same percentage split used by the modal/help overlays).
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(v[1])[1]
}

/// Render the palette overlay: a query box above a ranked, scrollable list.
pub(crate) fn render_palette(frame: &mut ratatui::Frame, p: &Palette, theme: Theme) {
    let th = theme;
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Command palette ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.accent));
    let inner = Layout::vertical([
        Constraint::Length(1), // query
        Constraint::Min(1),    // results
        Constraint::Length(1), // footer
    ])
    .split(Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    ));
    frame.render_widget(block, area);

    // Query line: "> " prompt + the input with a visible cursor.
    let cols = Layout::horizontal([Constraint::Length(2), Constraint::Min(0)]).split(inner[0]);
    frame.render_widget(
        Paragraph::new(Span::styled("> ", Style::default().fg(th.accent))),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(render_input_with_cursor(
            &p.query,
            p.query.len(),
            cols[1].width as usize,
            th,
        ))
        .style(Style::default().fg(th.text)),
        cols[1],
    );

    // Results, windowed so the selection stays on screen.
    let rows_area = inner[1];
    let max_rows = rows_area.height as usize;
    let mut lines: Vec<Line> = Vec::new();
    if p.results.is_empty() {
        let msg = if p.candidates.is_empty() {
            "  (nothing to run)"
        } else {
            "  (no matches)"
        };
        lines.push(Line::styled(msg, Style::default().fg(th.muted)));
    } else {
        let window_start = if max_rows > 0 && p.selected >= max_rows {
            p.selected - max_rows + 1
        } else {
            0
        };
        for (idx, &cand_i) in p.results.iter().enumerate().skip(window_start).take(max_rows) {
            let c = &p.candidates[cand_i];
            let is_sel = idx == p.selected;
            let (marker, name_style) = if is_sel {
                ("▸ ", Style::default().fg(th.selected).add_modifier(Modifier::BOLD))
            } else {
                ("  ", Style::default().fg(th.text))
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{marker}{}", c.label), name_style),
                Span::styled(format!("  —  {}", c.detail), Style::default().fg(th.muted)),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), rows_area);

    // Footer hints.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Enter", Style::default().fg(th.accent)),
            Span::raw(" run  "),
            Span::styled("↑↓", Style::default().fg(th.accent)),
            Span::raw(" move  "),
            Span::styled("Esc", Style::default().fg(th.accent)),
            Span::raw(" cancel"),
        ]))
        .style(Style::default().fg(th.muted)),
        inner[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(ws_id: &str, name: &str, repo: &str, branch: Option<&str>) -> Candidate {
        let match_text = match branch {
            Some(b) => format!("{name} {b} {repo}"),
            None => format!("{name} {repo}"),
        };
        Candidate {
            label: name.into(),
            detail: repo.into(),
            match_text,
            action: PaletteAction::OpenWorkspace { ws_id: ws_id.into() },
        }
    }

    /// The ws_id under the selection, if it's an OpenWorkspace entry.
    fn sel_ws(p: &Palette) -> Option<&str> {
        match p.selected_action() {
            Some(PaletteAction::OpenWorkspace { ws_id }) => Some(ws_id.as_str()),
            _ => None,
        }
    }

    #[test]
    fn score_requires_subsequence() {
        assert!(score("auth", "auth-refactor").is_some());
        assert!(score("atr", "auth-refactor").is_some()); // scattered subsequence
        assert!(score("zzz", "auth-refactor").is_none());
        assert!(score("", "anything").is_some()); // empty query matches
    }

    #[test]
    fn score_is_case_insensitive() {
        assert!(score("AUTH", "auth-refactor").is_some());
        assert!(score("AuTh", "Auth-Refactor").is_some());
    }

    #[test]
    fn score_prefers_word_boundary_and_contiguous() {
        // A prefix match beats the same letters scattered mid-word.
        assert!(score("auth", "auth-refactor").unwrap() > score("auth", "reauthorize").unwrap());
        // A boundary match ("ar" = auth-refactor) beats a non-boundary one.
        assert!(score("ar", "auth-refactor").unwrap() > score("ar", "aardvark").unwrap());
    }

    #[test]
    fn rerank_orders_by_score_and_clamps_selection() {
        let cands = vec![
            cand("w1", "reauthorize", "api", None),
            cand("w2", "auth-refactor", "web", None),
            cand("w3", "docs", "web", None),
        ];
        let mut p = Palette::new(cands);
        // Empty query: all three present, input order.
        assert_eq!(p.results.len(), 3);

        p.push_char('a');
        p.push_char('u');
        p.push_char('t');
        p.push_char('h');
        // Only the two auth-ish rows match; the boundary match ranks first.
        assert_eq!(p.results.len(), 2);
        assert_eq!(sel_ws(&p), Some("w2"));

        // Selection clamps when results shrink under it.
        p.move_down();
        assert_eq!(p.selected, 1);
        p.push_char('x'); // "authx" matches nothing
        assert_eq!(p.results.len(), 0);
        assert_eq!(p.selected, 0);
        assert_eq!(sel_ws(&p), None);
    }

    #[test]
    fn matches_against_branch_and_repo_too() {
        let cands = vec![cand("w1", "misc", "myrepo", Some("kommand0/billing"))];
        let mut p = Palette::new(cands);
        for ch in "billing".chars() {
            p.push_char(ch);
        }
        assert_eq!(sel_ws(&p), Some("w1"), "branch text is searchable");
    }

    #[test]
    fn action_entries_match_their_verb() {
        let cands = vec![
            cand("w2", "foo", "web", None),
            Candidate {
                label: "Clean up — foo".into(),
                detail: "web".into(),
                match_text: "clean up cleanup foo web".into(),
                action: PaletteAction::Cleanup { ws_id: "w1".into() },
            },
        ];
        let mut p = Palette::new(cands);
        for ch in "cleanup".chars() {
            p.push_char(ch);
        }
        assert_eq!(
            p.selected_action(),
            Some(&PaletteAction::Cleanup { ws_id: "w1".into() }),
            "typing 'cleanup' surfaces the Clean up action over the plain workspace jump"
        );
    }

    #[test]
    fn paste_appends_to_query_reranks_and_strips_control() {
        let cands = vec![
            cand("w1", "reauthorize", "api", None),
            cand("w2", "auth-refactor", "web", None),
        ];
        let mut p = Palette::new(cands);
        p.paste("auth\n"); // newline stripped; query narrows to the two auth rows
        assert_eq!(p.query, "auth");
        assert_eq!(p.results.len(), 2);
        assert_eq!(sel_ws(&p), Some("w2"), "boundary match ranks first after paste");
    }

    #[test]
    fn paste_of_only_control_chars_is_a_noop_and_keeps_selection() {
        let mut p = Palette::new(vec![cand("w1", "a", "r", None), cand("w2", "b", "r", None)]);
        p.move_down(); // selected = 1
        p.paste("\n\t"); // sanitizes to empty
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 1, "a no-op paste must not rerank or reset the selection");
    }

    #[test]
    fn move_up_down_stay_in_bounds() {
        let cands = vec![cand("w1", "a", "r", None), cand("w2", "b", "r", None)];
        let mut p = Palette::new(cands);
        p.move_up(); // already at 0
        assert_eq!(p.selected, 0);
        p.move_down();
        p.move_down(); // can't pass the last row
        assert_eq!(p.selected, 1);
    }
}
