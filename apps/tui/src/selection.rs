/// Represents the selection state in the output pane.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SelectionState {
    #[default]
    None,
    /// Cursor visible but no range selected.
    Cursor {
        line: usize,
        char_offset: usize,
    },
    /// Active selection range.
    Range {
        anchor_line: usize,
        anchor_char: usize,
        cursor_line: usize,
        cursor_char: usize,
    },
}

impl SelectionState {
    /// Returns true if the selection state is None.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Returns true if there is an active range selection.
    pub fn has_range(&self) -> bool {
        matches!(self, Self::Range { .. })
    }

    /// Returns (start, end) in document order regardless of anchor/cursor direction.
    /// Returns None for None and Cursor states.
    pub fn ordered_range(&self) -> Option<((usize, usize), (usize, usize))> {
        match self {
            Self::Range {
                anchor_line,
                anchor_char,
                cursor_line,
                cursor_char,
            } => {
                let anchor = (*anchor_line, *anchor_char);
                let cursor = (*cursor_line, *cursor_char);
                if anchor.0 < cursor.0 || (anchor.0 == cursor.0 && anchor.1 <= cursor.1) {
                    Some((anchor, cursor))
                } else {
                    Some((cursor, anchor))
                }
            }
            _ => None,
        }
    }

    /// Resets to None.
    pub fn clear(&mut self) {
        *self = Self::None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_none() {
        let state = SelectionState::default();
        assert_eq!(state, SelectionState::None);
    }

    #[test]
    fn is_none_returns_true_for_none() {
        let state = SelectionState::None;
        assert!(state.is_none());
    }

    #[test]
    fn is_none_returns_false_for_cursor() {
        let state = SelectionState::Cursor {
            line: 0,
            char_offset: 0,
        };
        assert!(!state.is_none());
    }

    #[test]
    fn is_none_returns_false_for_range() {
        let state = SelectionState::Range {
            anchor_line: 0,
            anchor_char: 0,
            cursor_line: 1,
            cursor_char: 5,
        };
        assert!(!state.is_none());
    }

    #[test]
    fn has_range_returns_true_only_for_range() {
        assert!(!SelectionState::None.has_range());
        assert!(
            !SelectionState::Cursor {
                line: 0,
                char_offset: 0
            }
            .has_range()
        );
        assert!(SelectionState::Range {
            anchor_line: 0,
            anchor_char: 0,
            cursor_line: 1,
            cursor_char: 5,
        }
        .has_range());
    }

    #[test]
    fn ordered_range_returns_none_for_none_and_cursor() {
        assert_eq!(SelectionState::None.ordered_range(), None);
        assert_eq!(
            SelectionState::Cursor {
                line: 0,
                char_offset: 0
            }
            .ordered_range(),
            None
        );
    }

    #[test]
    fn ordered_range_anchor_before_cursor() {
        let state = SelectionState::Range {
            anchor_line: 0,
            anchor_char: 3,
            cursor_line: 2,
            cursor_char: 7,
        };
        assert_eq!(state.ordered_range(), Some(((0, 3), (2, 7))));
    }

    #[test]
    fn ordered_range_anchor_after_cursor_reversed() {
        let state = SelectionState::Range {
            anchor_line: 2,
            anchor_char: 7,
            cursor_line: 0,
            cursor_char: 3,
        };
        assert_eq!(state.ordered_range(), Some(((0, 3), (2, 7))));
    }

    #[test]
    fn ordered_range_same_line_chars_in_order() {
        let state = SelectionState::Range {
            anchor_line: 1,
            anchor_char: 10,
            cursor_line: 1,
            cursor_char: 3,
        };
        assert_eq!(state.ordered_range(), Some(((1, 3), (1, 10))));
    }

    #[test]
    fn clear_resets_to_none_from_cursor() {
        let mut state = SelectionState::Cursor {
            line: 5,
            char_offset: 10,
        };
        state.clear();
        assert_eq!(state, SelectionState::None);
    }

    #[test]
    fn clear_resets_to_none_from_range() {
        let mut state = SelectionState::Range {
            anchor_line: 0,
            anchor_char: 0,
            cursor_line: 5,
            cursor_char: 10,
        };
        state.clear();
        assert_eq!(state, SelectionState::None);
    }
}
