//! Rebindable key bindings: the tree pane, and the embedded pane's post-prefix
//! layer (`Ctrl+A` then a key).
//!
//! A [`KeyMap`] maps a normalized [`KeyChord`] to an [`Action`] per [`Layer`];
//! `handle_key` resolves a press to an action and dispatches it. Defaults match
//! the built-in bindings; a user's `config.json` `keybindings` (action name → key
//! specs) replace an action's keys in every layer it lives in. Fixed (not
//! rebindable): the `gg` motion and `Esc` (clears the filter) in the tree; the
//! `Ctrl+A` prefix key itself, `Ctrl+A 1`-`9` and `Ctrl+A Ctrl+A` in the pane.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A rebindable command; [`Action::layers`] says where it is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Action {
    MoveUp,
    MoveDown,
    CollapseOrParent,
    StepInto,
    SelectLast,
    PrevRepo,
    NextRepo,
    WidenTree,
    ShrinkTree,
    ActivateSelection,
    OpenSession,
    CloseSession,
    ReviewDiff,
    OpenPrInBrowser,
    Cleanup,
    Filter,
    Palette,
    NextWaiting,
    PrevWaiting,
    ArchiveToggle,
    MoveItemUp,
    MoveItemDown,
    SortByName,
    SortByAdded,
    AddRepo,
    AddWorkspace,
    Delete,
    ForceDelete,
    Help,
    OpenSettings,
    Quit,
    // Embedded-pane post-prefix layer (`Ctrl+A` then a key).
    NewClaudeTab,
    NewShellTab,
    NewCodexTab,
    NewGeminiTab,
    NewOpencodeTab,
    PrevTab,
    NextTab,
    LastTab,
    RenameTab,
    CloseTab,
    Detach,
    BackToTree,
}

/// A key layer: the tree pane, or the embedded pane after an armed `Ctrl+A`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layer {
    Tree,
    Pane,
}

/// Every action, in the order shown in the help overlay (per layer).
pub(crate) const ALL_ACTIONS: &[Action] = &[
    Action::MoveUp,
    Action::MoveDown,
    Action::CollapseOrParent,
    Action::StepInto,
    Action::SelectLast,
    Action::PrevRepo,
    Action::NextRepo,
    Action::WidenTree,
    Action::ShrinkTree,
    Action::ActivateSelection,
    Action::OpenSession,
    Action::CloseSession,
    Action::ReviewDiff,
    Action::OpenPrInBrowser,
    Action::Cleanup,
    Action::Filter,
    Action::Palette,
    Action::NextWaiting,
    Action::PrevWaiting,
    Action::ArchiveToggle,
    Action::MoveItemUp,
    Action::MoveItemDown,
    Action::SortByName,
    Action::SortByAdded,
    Action::AddRepo,
    Action::AddWorkspace,
    Action::Delete,
    Action::ForceDelete,
    Action::Help,
    Action::OpenSettings,
    Action::NewClaudeTab,
    Action::NewShellTab,
    Action::NewCodexTab,
    Action::NewGeminiTab,
    Action::NewOpencodeTab,
    Action::PrevTab,
    Action::NextTab,
    Action::LastTab,
    Action::RenameTab,
    Action::CloseTab,
    Action::Detach,
    Action::BackToTree,
    Action::Quit,
];

impl Action {
    /// Stable config name (kebab-case).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Action::MoveUp => "move-up",
            Action::MoveDown => "move-down",
            Action::CollapseOrParent => "collapse",
            Action::StepInto => "expand",
            Action::SelectLast => "last",
            Action::PrevRepo => "prev-repo",
            Action::NextRepo => "next-repo",
            Action::WidenTree => "widen-tree",
            Action::ShrinkTree => "shrink-tree",
            Action::ActivateSelection => "activate",
            Action::OpenSession => "open",
            Action::CloseSession => "close",
            Action::ReviewDiff => "review-diff",
            Action::OpenPrInBrowser => "open-pr-web",
            Action::Cleanup => "cleanup",
            Action::Filter => "filter",
            Action::Palette => "palette",
            Action::NextWaiting => "next-waiting",
            Action::PrevWaiting => "prev-waiting",
            Action::ArchiveToggle => "archive",
            Action::MoveItemUp => "move-item-up",
            Action::MoveItemDown => "move-item-down",
            Action::SortByName => "sort-by-name",
            Action::SortByAdded => "sort-by-added",
            Action::AddRepo => "add-repo",
            Action::AddWorkspace => "add-workspace",
            Action::Delete => "delete",
            Action::ForceDelete => "force-delete",
            Action::Help => "help",
            Action::OpenSettings => "settings",
            Action::Quit => "quit",
            Action::NewClaudeTab => "new-claude",
            Action::NewShellTab => "new-shell",
            Action::NewCodexTab => "new-codex",
            Action::NewGeminiTab => "new-gemini",
            Action::NewOpencodeTab => "new-opencode",
            Action::PrevTab => "prev-tab",
            Action::NextTab => "next-tab",
            Action::LastTab => "last-tab",
            Action::RenameTab => "rename-tab",
            Action::CloseTab => "close-tab",
            Action::Detach => "detach",
            Action::BackToTree => "back-to-tree",
        }
    }

    /// Help-overlay description.
    pub(crate) fn description(self) -> &'static str {
        match self {
            Action::MoveUp => "Move up",
            Action::MoveDown => "Move down",
            Action::CollapseOrParent => "Collapse repo / jump to parent",
            Action::StepInto => "Expand repo / step in",
            Action::SelectLast => "Last item (gg = first)",
            Action::PrevRepo => "Jump to the previous repo",
            Action::NextRepo => "Jump to the next repo",
            Action::WidenTree => "Widen tree pane",
            Action::ShrinkTree => "Shrink tree pane",
            Action::ActivateSelection => "Open workspace / expand repo",
            Action::OpenSession => "Open embedded claude",
            Action::CloseSession => "Close embedded claude",
            Action::ReviewDiff => "Review changes (PR-style diff)",
            Action::OpenPrInBrowser => "Open the PR in a browser",
            Action::Cleanup => "Clean up a merged workspace",
            Action::Filter => "Filter workspaces (Esc clears)",
            Action::Palette => "Go to workspace",
            Action::NextWaiting => "Next workspace that needs you",
            Action::PrevWaiting => "Previous workspace that needs you",
            Action::ArchiveToggle => "Archive / activate workspace",
            Action::MoveItemUp => "Move selected item up",
            Action::MoveItemDown => "Move selected item down",
            Action::SortByName => "Sort by name (asc/desc/off)",
            Action::SortByAdded => "Sort by date added (asc/desc/off)",
            Action::AddRepo => "Add repository",
            Action::AddWorkspace => "Add workspace",
            Action::Delete => "Delete selected",
            Action::ForceDelete => "Force delete",
            Action::Help => "Help",
            Action::OpenSettings => "Settings",
            Action::Quit => "Quit",
            Action::NewClaudeTab => "New Claude Code session tab",
            Action::NewShellTab => "New shell tab (reopens fresh)",
            Action::NewCodexTab => "New codex session tab",
            Action::NewGeminiTab => "New gemini session tab",
            Action::NewOpencodeTab => "New opencode session tab",
            Action::PrevTab => "Previous tab",
            Action::NextTab => "Next tab",
            Action::LastTab => "Jump to the last-active tab",
            Action::RenameTab => "Rename the active tab",
            Action::CloseTab => "Close the active tab",
            Action::Detach => "Detach: close panes, keep sessions",
            Action::BackToTree => "Back to tree",
        }
    }

    /// The layer(s) this action is bound in. `Quit` is in both: the same chord
    /// quits from the tree and after the prefix in the pane.
    pub(crate) fn layers(self) -> &'static [Layer] {
        match self {
            Action::Quit => &[Layer::Tree, Layer::Pane],
            Action::NewClaudeTab
            | Action::NewShellTab
            | Action::NewCodexTab
            | Action::NewGeminiTab
            | Action::NewOpencodeTab
            | Action::PrevTab
            | Action::NextTab
            | Action::LastTab
            | Action::RenameTab
            | Action::CloseTab
            | Action::Detach
            | Action::BackToTree => &[Layer::Pane],
            _ => &[Layer::Tree],
        }
    }

    fn from_name(name: &str) -> Option<Action> {
        ALL_ACTIONS.iter().copied().find(|a| a.name() == name)
    }
}

/// A normalized key chord (see [`normalize`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

/// Normalize a (code, modifiers) pair so a binding matches the events a terminal
/// actually sends (which varies with the enhanced-keyboard flags):
/// - for a character, SHIFT is dropped (it's encoded in the char's case);
/// - with CTRL/ALT, a letter is lower-cased (crossterm reports `Ctrl+A` as
///   `Char('a')` + CONTROL).
fn normalize(code: KeyCode, mods: KeyModifiers) -> KeyChord {
    let mut mods = mods;
    let code = match code {
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) => {
            KeyCode::Char(c.to_ascii_lowercase())
        }
        other => other,
    };
    // SHIFT is never part of a chord: for chars the case carries it; we don't
    // bind shifted non-char keys.
    mods.remove(KeyModifiers::SHIFT);
    KeyChord { code, mods }
}

/// Chords handled by fixed (non-rebindable) pre-checks in `handle_key` for a
/// layer, with a reason for the warning. Binding an action to one of these
/// would never fire.
fn reserved_reason(layer: Layer, chord: &KeyChord) -> Option<&'static str> {
    let plain = chord.mods.is_empty();
    match layer {
        Layer::Tree if chord.code == KeyCode::Char('g') && plain => Some("the gg motion"),
        Layer::Tree if chord.code == KeyCode::Esc => Some("clearing the filter"),
        Layer::Pane if matches!(chord.code, KeyCode::Char('1'..='9')) && plain => {
            Some("jumping to tab N")
        }
        Layer::Pane if chord.code == KeyCode::Char('a') && chord.mods == KeyModifiers::CONTROL => {
            Some("the literal Ctrl+A")
        }
        _ => None,
    }
}

/// Parse a key spec like `"j"`, `"Up"`, `"Enter"`, `"/"`, `"ctrl+a"`, `"G"` into
/// a normalized chord. Returns `None` for an unrecognized spec.
pub(crate) fn parse_chord(spec: &str) -> Option<KeyChord> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let mut mods = KeyModifiers::empty();
    // Modifiers are '+'-separated, with the key in the final segment. (Binding
    // to the literal '+' key isn't supported — an empty key segment is invalid.)
    let parts: Vec<&str> = spec.split('+').collect();
    let (key_part, mod_parts) = parts.split_last().unwrap();
    let key_part = *key_part;
    if key_part.is_empty() {
        return None;
    }
    for m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "option" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let code = match key_part {
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        s => match s.to_ascii_lowercase().as_str() {
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "space" => KeyCode::Char(' '),
            "delete" | "del" => KeyCode::Delete,
            "backspace" => KeyCode::Backspace,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            _ => return None,
        },
    };
    // A shifted letter spec ("shift+g") folds into the upper-case char so it
    // matches `normalize`.
    let code = if mods.contains(KeyModifiers::SHIFT) {
        if let KeyCode::Char(c) = code {
            KeyCode::Char(c.to_ascii_uppercase())
        } else {
            code
        }
    } else {
        code
    };
    Some(normalize(code, mods))
}

/// Format a chord for the help overlay (e.g. `k`, `Up`, `Ctrl+a`, `Enter`).
fn format_chord(chord: &KeyChord) -> String {
    let mut s = String::new();
    if chord.mods.contains(KeyModifiers::CONTROL) {
        s.push_str("Ctrl+");
    }
    if chord.mods.contains(KeyModifiers::ALT) {
        s.push_str("Alt+");
    }
    match chord.code {
        KeyCode::Char(' ') => s.push_str("Space"),
        KeyCode::Char(c) => s.push(c),
        KeyCode::Up => s.push_str("Up"),
        KeyCode::Down => s.push_str("Down"),
        KeyCode::Left => s.push_str("Left"),
        KeyCode::Right => s.push_str("Right"),
        KeyCode::Enter => s.push_str("Enter"),
        KeyCode::Esc => s.push_str("Esc"),
        KeyCode::Tab => s.push_str("Tab"),
        KeyCode::Delete => s.push_str("Delete"),
        KeyCode::Backspace => s.push_str("Backspace"),
        KeyCode::Home => s.push_str("Home"),
        KeyCode::End => s.push_str("End"),
        other => s.push_str(&format!("{other:?}")),
    }
    s
}

pub(crate) struct KeyMap {
    tree: HashMap<KeyChord, Action>,
    pane: HashMap<KeyChord, Action>,
}

impl Default for KeyMap {
    fn default() -> Self {
        let mut keymap = KeyMap { tree: HashMap::new(), pane: HashMap::new() };
        for (spec, action) in DEFAULT_BINDINGS {
            if let Some(chord) = parse_chord(spec) {
                for &layer in action.layers() {
                    keymap.map_mut(layer).insert(chord, *action);
                }
            }
        }
        keymap
    }
}

/// Built-in bindings (spec, action). Multiple specs may map to one action.
const DEFAULT_BINDINGS: &[(&str, Action)] = &[
    ("Up", Action::MoveUp),
    ("k", Action::MoveUp),
    ("Down", Action::MoveDown),
    ("j", Action::MoveDown),
    ("Left", Action::CollapseOrParent),
    ("h", Action::CollapseOrParent),
    ("Right", Action::StepInto),
    ("l", Action::StepInto),
    ("G", Action::SelectLast),
    ("{", Action::PrevRepo),
    ("}", Action::NextRepo),
    (">", Action::WidenTree),
    ("<", Action::ShrinkTree),
    ("Enter", Action::ActivateSelection),
    ("e", Action::OpenSession),
    ("r", Action::OpenSession),
    ("R", Action::OpenSession),
    ("x", Action::CloseSession),
    ("Delete", Action::CloseSession),
    ("v", Action::ReviewDiff),
    ("p", Action::OpenPrInBrowser),
    ("c", Action::Cleanup),
    ("/", Action::Filter),
    (":", Action::Palette),
    ("n", Action::NextWaiting),
    ("N", Action::PrevWaiting),
    ("A", Action::ArchiveToggle),
    ("K", Action::MoveItemUp),
    ("J", Action::MoveItemDown),
    ("s", Action::SortByName),
    ("t", Action::SortByAdded),
    ("a", Action::AddRepo),
    ("w", Action::AddWorkspace),
    ("d", Action::Delete),
    ("D", Action::ForceDelete),
    ("?", Action::Help),
    (",", Action::OpenSettings),
    ("q", Action::Quit),
    // Embedded-pane post-prefix layer (`Ctrl+A` then the key).
    ("c", Action::NewClaudeTab),
    ("s", Action::NewShellTab),
    ("e", Action::NewCodexTab),
    ("g", Action::NewGeminiTab),
    ("o", Action::NewOpencodeTab),
    ("[", Action::PrevTab),
    ("]", Action::NextTab),
    ("l", Action::LastTab),
    ("r", Action::RenameTab),
    ("x", Action::CloseTab),
    ("d", Action::Detach),
    ("t", Action::BackToTree),
    ("Tab", Action::BackToTree),
    ("Esc", Action::BackToTree),
];

impl KeyMap {
    fn map(&self, layer: Layer) -> &HashMap<KeyChord, Action> {
        match layer {
            Layer::Tree => &self.tree,
            Layer::Pane => &self.pane,
        }
    }

    fn map_mut(&mut self, layer: Layer) -> &mut HashMap<KeyChord, Action> {
        match layer {
            Layer::Tree => &mut self.tree,
            Layer::Pane => &mut self.pane,
        }
    }

    /// Resolve a tree-pane key press to its bound action, if any.
    pub(crate) fn resolve(&self, key: &KeyEvent) -> Option<Action> {
        self.tree.get(&normalize(key.code, key.modifiers)).copied()
    }

    /// Resolve the key after an armed `Ctrl+A` prefix to its bound action, if any.
    pub(crate) fn resolve_pane(&self, key: &KeyEvent) -> Option<Action> {
        self.pane.get(&normalize(key.code, key.modifiers)).copied()
    }

    /// Build the keymap from config overrides. For each named action, its
    /// default chords are replaced by the configured ones in every layer it
    /// lives in. Returns warnings for unknown actions, unparseable specs,
    /// reserved keys, and reassignments.
    pub(crate) fn build(config: &HashMap<String, Vec<String>>) -> (Self, Vec<String>) {
        let mut keymap = Self::default();
        let mut warnings = Vec::new();

        // Deterministic order so warnings/result don't depend on HashMap order.
        let mut entries: Vec<(&String, &Vec<String>)> = config.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));

        for (name, specs) in entries {
            let Some(action) = Action::from_name(name) else {
                warnings.push(format!("unknown keybinding action '{name}'"));
                continue;
            };
            // Replace this action's keys: drop its current chords first.
            for &layer in action.layers() {
                keymap.map_mut(layer).retain(|_, a| *a != action);
            }
            for spec in specs {
                let Some(chord) = parse_chord(spec) else {
                    warnings.push(format!("invalid key '{spec}' for '{name}'"));
                    continue;
                };
                if let Some(reason) =
                    action.layers().iter().find_map(|&layer| reserved_reason(layer, &chord))
                {
                    warnings.push(format!("key '{spec}' is reserved for {reason}; ignored for '{name}'"));
                    continue;
                }
                for &layer in action.layers() {
                    let map = keymap.map_mut(layer);
                    if let Some(prev) = map.get(&chord)
                        && *prev != action
                    {
                        warnings.push(format!(
                            "key '{spec}' reassigned from '{}' to '{name}'",
                            prev.name()
                        ));
                    }
                    map.insert(chord, action);
                }
            }
        }
        // `back-to-tree` is the keyboard's way out of the embedded pane: it can
        // be rebound but never unbound.
        if keymap.chords_for(Action::BackToTree).is_empty() {
            warnings.push("'back-to-tree' cannot be unbound; its default keys are restored".to_string());
            for (spec, action) in DEFAULT_BINDINGS {
                if *action == Action::BackToTree
                    && let Some(chord) = parse_chord(spec)
                {
                    keymap.pane.insert(chord, *action);
                }
            }
        }
        (keymap, warnings)
    }

    /// Chords bound to an action, sorted for stable help display. An action in
    /// both layers has the same chords in each, so its first layer serves.
    fn chords_for(&self, action: Action) -> Vec<KeyChord> {
        let mut chords: Vec<KeyChord> = self
            .map(action.layers()[0])
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(c, _)| *c)
            .collect();
        chords.sort_by_key(format_chord);
        chords
    }

    /// The help-overlay keys column for an action; `None` when unbound.
    fn keys_label(&self, action: Action) -> Option<String> {
        let chords = self.chords_for(action);
        (!chords.is_empty()).then(|| chords.iter().map(format_chord).collect::<Vec<_>>().join(" / "))
    }

    /// `(keys, description)` rows for the help overlay's tree section, in
    /// [`ALL_ACTIONS`] order. An unbound action shows `(unbound)`.
    pub(crate) fn help_rows(&self) -> Vec<(String, &'static str)> {
        ALL_ACTIONS
            .iter()
            .filter(|a| a.layers().contains(&Layer::Tree))
            .map(|&action| {
                let keys = self.keys_label(action).unwrap_or_else(|| "(unbound)".to_string());
                (keys, action.description())
            })
            .collect()
    }

    /// `(keys, description)` rows for the help overlay's embedded section: the
    /// post-prefix actions in [`ALL_ACTIONS`] order, each as `Ctrl+A then <keys>`.
    pub(crate) fn pane_help_rows(&self) -> Vec<(String, &'static str)> {
        ALL_ACTIONS
            .iter()
            .filter(|a| a.layers().contains(&Layer::Pane))
            .map(|&action| {
                let keys = self
                    .keys_label(action)
                    .map(|k| format!("Ctrl+A then {k}"))
                    .unwrap_or_else(|| "(unbound)".to_string());
                (keys, action.description())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn defaults_resolve() {
        let km = KeyMap::default();
        assert_eq!(km.resolve(&ev(KeyCode::Char('j'), KeyModifiers::NONE)), Some(Action::MoveDown));
        assert_eq!(km.resolve(&ev(KeyCode::Down, KeyModifiers::NONE)), Some(Action::MoveDown));
        assert_eq!(km.resolve(&ev(KeyCode::Char('q'), KeyModifiers::NONE)), Some(Action::Quit));
        assert_eq!(km.resolve(&ev(KeyCode::Char('/'), KeyModifiers::NONE)), Some(Action::Filter));
        assert_eq!(km.resolve(&ev(KeyCode::Char(':'), KeyModifiers::NONE)), Some(Action::Palette));
        assert_eq!(km.resolve(&ev(KeyCode::Char(','), KeyModifiers::NONE)), Some(Action::OpenSettings));
        assert_eq!(km.resolve(&ev(KeyCode::Enter, KeyModifiers::NONE)), Some(Action::ActivateSelection));
        // Enter, step-in, and open-session are three distinct actions
        // (Enter activates; l/Right steps into a repo; e/r/R open a session).
        assert_eq!(km.resolve(&ev(KeyCode::Enter, KeyModifiers::NONE)), Some(Action::ActivateSelection));
        assert_eq!(km.resolve(&ev(KeyCode::Right, KeyModifiers::NONE)), Some(Action::StepInto));
        assert_eq!(km.resolve(&ev(KeyCode::Char('e'), KeyModifiers::NONE)), Some(Action::OpenSession));
        assert_eq!(km.resolve(&ev(KeyCode::Char('R'), KeyModifiers::NONE)), Some(Action::OpenSession));
        // n / N jump to the next / previous waiting workspace (distinct chords:
        // SHIFT is carried by the char case, not a modifier).
        assert_eq!(km.resolve(&ev(KeyCode::Char('n'), KeyModifiers::NONE)), Some(Action::NextWaiting));
        assert_eq!(km.resolve(&ev(KeyCode::Char('N'), KeyModifiers::SHIFT)), Some(Action::PrevWaiting));
        // { / } jump between repo headers.
        assert_eq!(km.resolve(&ev(KeyCode::Char('{'), KeyModifiers::NONE)), Some(Action::PrevRepo));
        assert_eq!(km.resolve(&ev(KeyCode::Char('}'), KeyModifiers::NONE)), Some(Action::NextRepo));
        // < shrinks the tree pane, > widens it.
        assert_eq!(km.resolve(&ev(KeyCode::Char('<'), KeyModifiers::NONE)), Some(Action::ShrinkTree));
        assert_eq!(km.resolve(&ev(KeyCode::Char('>'), KeyModifiers::NONE)), Some(Action::WidenTree));
        // p opens the selected workspace's PR in the browser.
        assert_eq!(km.resolve(&ev(KeyCode::Char('p'), KeyModifiers::NONE)), Some(Action::OpenPrInBrowser));
    }

    #[test]
    fn parse_specs() {
        assert_eq!(parse_chord("j"), Some(normalize(KeyCode::Char('j'), KeyModifiers::NONE)));
        assert_eq!(parse_chord("Up"), Some(KeyChord { code: KeyCode::Up, mods: KeyModifiers::NONE }));
        assert_eq!(parse_chord("Enter").unwrap().code, KeyCode::Enter);
        assert_eq!(parse_chord("/").unwrap().code, KeyCode::Char('/'));
        assert_eq!(parse_chord("Space").unwrap().code, KeyCode::Char(' '));
        // ctrl+letter normalizes to lowercase + CONTROL (matches crossterm).
        let c = parse_chord("ctrl+A").unwrap();
        assert_eq!(c.code, KeyCode::Char('a'));
        assert!(c.mods.contains(KeyModifiers::CONTROL));
        // shift+g folds into the uppercase char, no shift bit.
        let g = parse_chord("shift+g").unwrap();
        assert_eq!(g.code, KeyCode::Char('G'));
        assert!(!g.mods.contains(KeyModifiers::SHIFT));
        // bad specs
        assert_eq!(parse_chord(""), None);
        assert_eq!(parse_chord("Nope"), None);
        assert_eq!(parse_chord("ctrl+"), None);
    }

    #[test]
    fn uppercase_event_matches_uppercase_binding_regardless_of_shift_bit() {
        let km = KeyMap::default();
        // `G` arrives as Char('G') with or without a SHIFT bit depending on the
        // terminal's enhancement flags; both must resolve.
        assert_eq!(km.resolve(&ev(KeyCode::Char('G'), KeyModifiers::NONE)), Some(Action::SelectLast));
        assert_eq!(km.resolve(&ev(KeyCode::Char('G'), KeyModifiers::SHIFT)), Some(Action::SelectLast));
    }

    #[test]
    fn shifted_symbols_resolve_to_their_action() {
        let km = KeyMap::default();
        // With REPORT_ALTERNATE_KEYS the terminal reports the shifted codepoint, so a
        // shifted symbol arrives as its character (with or without a stray SHIFT bit,
        // which normalize() strips) and resolves to its bound action.
        assert_eq!(km.resolve(&ev(KeyCode::Char('?'), KeyModifiers::NONE)), Some(Action::Help));
        assert_eq!(km.resolve(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT)), Some(Action::Help));
        assert_eq!(km.resolve(&ev(KeyCode::Char(':'), KeyModifiers::SHIFT)), Some(Action::Palette));
        assert_eq!(km.resolve(&ev(KeyCode::Char('<'), KeyModifiers::SHIFT)), Some(Action::ShrinkTree));
        assert_eq!(km.resolve(&ev(KeyCode::Char('>'), KeyModifiers::SHIFT)), Some(Action::WidenTree));
        // Failure mode this fix prevents: without REPORT_ALTERNATE_KEYS a Kitty-protocol
        // terminal reports `?` as `/`+SHIFT, which normalize() collapses to `/` → Filter,
        // not Help. That is WHY main.rs must request REPORT_ALTERNATE_KEYS.
        assert_eq!(km.resolve(&ev(KeyCode::Char('/'), KeyModifiers::SHIFT)), Some(Action::Filter));
    }

    #[test]
    fn config_rebinds_and_clears_the_old_key() {
        let mut cfg = HashMap::new();
        cfg.insert("quit".to_string(), vec!["ctrl+q".to_string()]);
        let (km, warns) = KeyMap::build(&cfg);
        assert!(warns.is_empty(), "{warns:?}");
        // New chord quits; the old `q` no longer does.
        let cq = parse_chord("ctrl+q").unwrap();
        assert_eq!(km.resolve(&ev(cq.code, cq.mods)), Some(Action::Quit));
        assert_eq!(km.resolve(&ev(KeyCode::Char('q'), KeyModifiers::NONE)), None);
        // An unmodified action is unaffected by the rebind.
        assert_eq!(km.resolve(&ev(KeyCode::Char('j'), KeyModifiers::NONE)), Some(Action::MoveDown));
    }

    #[test]
    fn config_rebinds_widen_tree() {
        let mut cfg = HashMap::new();
        cfg.insert("widen-tree".to_string(), vec!["ctrl+l".to_string()]);
        let (km, warns) = KeyMap::build(&cfg);
        assert!(warns.is_empty(), "{warns:?}");
        // New chord widens; the old `>` no longer does.
        let cl = parse_chord("ctrl+l").unwrap();
        assert_eq!(km.resolve(&ev(cl.code, cl.mods)), Some(Action::WidenTree));
        assert_eq!(km.resolve(&ev(KeyCode::Char('>'), KeyModifiers::NONE)), None);
        // Shrink keeps its default.
        assert_eq!(km.resolve(&ev(KeyCode::Char('<'), KeyModifiers::NONE)), Some(Action::ShrinkTree));
    }

    #[test]
    fn build_warns_on_unknown_action_bad_spec_reserved_and_reassign() {
        let mut cfg = HashMap::new();
        cfg.insert("nope".to_string(), vec!["x".to_string()]);
        cfg.insert("quit".to_string(), vec!["Nope".to_string()]);
        cfg.insert("open".to_string(), vec!["g".to_string(), "Esc".to_string()]);
        // Reassign: bind delete to `q` (q is quit's default, not rebound here).
        cfg.insert("delete".to_string(), vec!["q".to_string()]);
        let (km, warns) = KeyMap::build(&cfg);
        assert!(warns.iter().any(|w| w.contains("unknown keybinding action 'nope'")));
        assert!(warns.iter().any(|w| w.contains("invalid key 'Nope'")));
        assert!(warns.iter().any(|w| w.contains("reserved")));
        assert!(warns.iter().any(|w| w.contains("reassigned")), "{warns:?}");
        // Config won the chord: `q` now deletes.
        assert_eq!(km.resolve(&ev(KeyCode::Char('q'), KeyModifiers::NONE)), Some(Action::Delete));
    }

    #[test]
    fn help_rows_cover_all_actions_in_order() {
        let km = KeyMap::default();
        let rows = km.help_rows();
        let tree_count = ALL_ACTIONS.iter().filter(|a| a.layers().contains(&Layer::Tree)).count();
        assert_eq!(rows.len(), tree_count);
        assert_eq!(rows[0].1, Action::MoveUp.description());
        // MoveUp shows both its chords.
        assert!(rows[0].0.contains('k') && rows[0].0.contains("Up"));
    }

    #[test]
    fn pane_layer_defaults_resolve_separately_from_the_tree() {
        let km = KeyMap::default();
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('t'), KeyModifiers::NONE)), Some(Action::BackToTree));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Tab, KeyModifiers::NONE)), Some(Action::BackToTree));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Esc, KeyModifiers::NONE)), Some(Action::BackToTree));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('c'), KeyModifiers::NONE)), Some(Action::NewClaudeTab));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char(']'), KeyModifiers::NONE)), Some(Action::NextTab));
        // A Ctrl-modified key is a different chord: `Ctrl+]` (decoded as
        // `]`+CTRL) must not read as the `]` tab command after the prefix.
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char(']'), KeyModifiers::CONTROL)), None);
        // The same key means something else in the tree; tree keys don't leak in.
        assert_eq!(km.resolve(&ev(KeyCode::Char('t'), KeyModifiers::NONE)), Some(Action::SortByAdded));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('j'), KeyModifiers::NONE)), None);
        // Quit is shared: `q` in the tree, `Ctrl+A q` in the pane.
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('q'), KeyModifiers::NONE)), Some(Action::Quit));
    }

    #[test]
    fn config_rebinds_pane_actions_and_the_shared_quit() {
        let mut cfg = HashMap::new();
        cfg.insert("back-to-tree".to_string(), vec!["ctrl+t".to_string()]);
        cfg.insert("quit".to_string(), vec!["ctrl+q".to_string()]);
        let (km, warns) = KeyMap::build(&cfg);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('t'), KeyModifiers::CONTROL)), Some(Action::BackToTree));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('t'), KeyModifiers::NONE)), None);
        assert_eq!(km.resolve_pane(&ev(KeyCode::Esc, KeyModifiers::NONE)), None);
        // The tree's `t` (sort by added) is untouched by a pane rebind.
        assert_eq!(km.resolve(&ev(KeyCode::Char('t'), KeyModifiers::NONE)), Some(Action::SortByAdded));
        // A shared action rebinds in both layers.
        assert_eq!(km.resolve(&ev(KeyCode::Char('q'), KeyModifiers::CONTROL)), Some(Action::Quit));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('q'), KeyModifiers::CONTROL)), Some(Action::Quit));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('q'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn back_to_tree_cannot_be_unbound_but_other_pane_actions_can() {
        let mut cfg = HashMap::new();
        cfg.insert("back-to-tree".to_string(), vec![]);
        let (km, warns) = KeyMap::build(&cfg);
        assert!(warns.iter().any(|w| w.contains("'back-to-tree' cannot be unbound")), "{warns:?}");
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('t'), KeyModifiers::NONE)), Some(Action::BackToTree));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Esc, KeyModifiers::NONE)), Some(Action::BackToTree));

        let mut cfg = HashMap::new();
        cfg.insert("detach".to_string(), vec![]);
        let (km, warns) = KeyMap::build(&cfg);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('d'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn pane_reserved_keys_warn_and_tree_reserved_keys_are_free_in_the_pane() {
        let mut cfg = HashMap::new();
        // Tab digits and the literal prefix are fixed in the pane layer.
        cfg.insert("close-tab".to_string(), vec!["1".to_string(), "ctrl+a".to_string(), "X".to_string()]);
        // Esc is reserved in the tree (clears the filter) but free in the pane.
        cfg.insert("detach".to_string(), vec!["Esc".to_string()]);
        let (km, warns) = KeyMap::build(&cfg);
        assert!(warns.iter().any(|w| w.contains("reserved for jumping to tab N")), "{warns:?}");
        assert!(warns.iter().any(|w| w.contains("reserved for the literal Ctrl+A")), "{warns:?}");
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('X'), KeyModifiers::NONE)), Some(Action::CloseTab));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('1'), KeyModifiers::NONE)), None);
        // Esc moved from back-to-tree to detach (a reassignment, warned).
        assert!(warns.iter().any(|w| w.contains("reassigned from 'back-to-tree' to 'detach'")), "{warns:?}");
        assert_eq!(km.resolve_pane(&ev(KeyCode::Esc, KeyModifiers::NONE)), Some(Action::Detach));
        assert_eq!(km.resolve_pane(&ev(KeyCode::Char('t'), KeyModifiers::NONE)), Some(Action::BackToTree));
    }

    #[test]
    fn pane_help_rows_cover_the_pane_actions_with_the_prefix() {
        let km = KeyMap::default();
        let rows = km.pane_help_rows();
        let pane_count = ALL_ACTIONS.iter().filter(|a| a.layers().contains(&Layer::Pane)).count();
        assert_eq!(rows.len(), pane_count);
        assert_eq!(rows[0], ("Ctrl+A then c".to_string(), Action::NewClaudeTab.description()));
        // Quit closes the section, showing its shared chord.
        assert_eq!(rows.last().unwrap(), &("Ctrl+A then q".to_string(), "Quit"));
        let back = rows.iter().find(|(_, d)| *d == "Back to tree").unwrap();
        assert!(
            back.0.starts_with("Ctrl+A then ") && back.0.contains("Tab") && back.0.contains("Esc"),
            "{back:?}"
        );
    }
}
