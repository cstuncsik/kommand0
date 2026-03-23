use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One visual row in the output pane after wrapping.
#[derive(Debug, Clone, PartialEq)]
pub struct VisualRow {
    /// Index into the logical lines array (pre-wrap)
    pub logical_line: usize,
    /// Byte offset within the logical line where this visual row starts
    pub start_byte: usize,
    /// Byte offset within the logical line where this visual row ends (exclusive)
    pub end_byte: usize,
    /// Display width of this visual row
    pub width: u16,
}

/// Maps between screen coordinates and logical text positions.
pub struct WrapMap {
    rows: Vec<VisualRow>,
    #[allow(dead_code)]
    pane_width: usize,
}

impl WrapMap {
    /// Build from raw text lines at a given pane width.
    /// Replicates ratatui's WordWrapper with trim=false.
    pub fn build(_lines: &[&str], pane_width: usize) -> Self {
        Self {
            rows: Vec::new(),
            pane_width,
        }
    }

    /// Screen (x, y) -> (logical_line, grapheme_offset).
    /// Needs access to original lines to walk grapheme clusters.
    pub fn screen_to_logical(
        &self,
        _x: u16,
        _y: u16,
        _scroll_from_top: usize,
        _lines: &[&str],
    ) -> Option<(usize, usize)> {
        None
    }

    /// (logical_line, grapheme_offset) -> (x_column, y_visual_row).
    pub fn logical_to_screen(
        &self,
        _line: usize,
        _char_offset: usize,
        _scroll_from_top: usize,
        _lines: &[&str],
    ) -> Option<(u16, u16)> {
        None
    }

    /// Total visual row count.
    pub fn total_visual_rows(&self) -> usize {
        self.rows.len()
    }

    /// Extract text between two logical positions.
    /// start and end are (line, grapheme_index) in document order.
    pub fn extract_text(
        &self,
        _lines: &[&str],
        _start: (usize, usize),
        _end: (usize, usize),
    ) -> String {
        String::new()
    }

    /// Access the rows (for testing).
    #[cfg(test)]
    pub fn rows(&self) -> &[VisualRow] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_produces_zero_visual_rows() {
        let lines: &[&str] = &[];
        let wm = WrapMap::build(lines, 80);
        assert_eq!(wm.total_visual_rows(), 0);
    }

    #[test]
    fn single_short_line_produces_one_visual_row() {
        let lines = &["hello"];
        let wm = WrapMap::build(lines, 80);
        assert_eq!(wm.total_visual_rows(), 1);
        assert_eq!(wm.rows()[0].logical_line, 0);
        assert_eq!(wm.rows()[0].start_byte, 0);
        assert_eq!(wm.rows()[0].end_byte, 5);
    }

    #[test]
    fn single_long_line_wraps_at_word_boundary() {
        // "hello world foo" at width 10
        // "hello" fits (5), then " world" would make 11 > 10 -> wrap
        // row 0 = "hello", row 1 = " world foo" (trim=false keeps space)
        let lines = &["hello world foo"];
        let wm = WrapMap::build(lines, 10);
        assert!(
            wm.total_visual_rows() >= 2,
            "Expected at least 2 rows, got {}",
            wm.total_visual_rows()
        );
        for row in wm.rows() {
            assert_eq!(row.logical_line, 0);
        }
    }

    #[test]
    fn long_word_breaks_at_width_boundary() {
        // "abcdefghijklmno" is 15 chars, width 5 -> 3 rows of 5
        let lines = &["abcdefghijklmno"];
        let wm = WrapMap::build(lines, 5);
        assert_eq!(wm.total_visual_rows(), 3);
        assert_eq!(
            &lines[0][wm.rows()[0].start_byte..wm.rows()[0].end_byte],
            "abcde"
        );
        assert_eq!(
            &lines[0][wm.rows()[1].start_byte..wm.rows()[1].end_byte],
            "fghij"
        );
        assert_eq!(
            &lines[0][wm.rows()[2].start_byte..wm.rows()[2].end_byte],
            "klmno"
        );
    }

    #[test]
    fn multiple_logical_lines_produce_correct_visual_rows() {
        let lines = &["hello", "world", "foo"];
        let wm = WrapMap::build(lines, 80);
        assert_eq!(wm.total_visual_rows(), 3);
        assert_eq!(wm.rows()[0].logical_line, 0);
        assert_eq!(wm.rows()[1].logical_line, 1);
        assert_eq!(wm.rows()[2].logical_line, 2);
    }

    #[test]
    fn screen_to_logical_first_char() {
        let lines = &["hello world"];
        let wm = WrapMap::build(lines, 80);
        let result = wm.screen_to_logical(0, 0, 0, lines);
        assert_eq!(result, Some((0, 0)));
    }

    #[test]
    fn screen_to_logical_with_scroll_offset() {
        let lines = &["line one", "line two", "line three"];
        let wm = WrapMap::build(lines, 80);
        // With scroll_from_top=1, y=0 maps to visual row 1 -> logical line 1
        let result = wm.screen_to_logical(0, 0, 1, lines);
        assert_eq!(result, Some((1, 0)));
    }

    #[test]
    fn logical_to_screen_round_trip() {
        let lines = &["hello world test"];
        let wm = WrapMap::build(lines, 80);
        // Position (0, 6) = 'w' in "world"
        let screen = wm.logical_to_screen(0, 6, 0, lines);
        assert!(screen.is_some(), "logical_to_screen should return Some");
        let (sx, sy) = screen.unwrap();
        let back = wm.screen_to_logical(sx, sy, 0, lines);
        assert_eq!(back, Some((0, 6)));
    }

    #[test]
    fn cjk_double_width_characters() {
        // U+4E16 and U+754C are 2 display columns each
        let lines = &["\u{4E16}\u{754C}"];
        let wm = WrapMap::build(lines, 80);
        assert_eq!(wm.total_visual_rows(), 1);
        assert_eq!(wm.rows()[0].width, 4);
    }

    #[test]
    fn cjk_wrapping_respects_double_width() {
        // Each CJK char is 2 columns. Width=5 means 2 chars fit (4 cols), 3rd won't fit.
        let lines = &["\u{4E16}\u{754C}\u{4F60}"];
        let wm = WrapMap::build(lines, 5);
        assert_eq!(
            wm.total_visual_rows(),
            2,
            "3 CJK chars (6 cols) at width 5 should produce 2 rows"
        );
    }

    #[test]
    fn emoji_single_codepoint_width() {
        // U+1F600 (grinning face) = 2 display columns
        let lines = &["\u{1F600}"];
        let wm = WrapMap::build(lines, 80);
        assert_eq!(wm.total_visual_rows(), 1);
        assert_eq!(wm.rows()[0].width, 2);
    }

    #[test]
    fn multi_codepoint_emoji_as_single_grapheme() {
        // Family emoji: multiple codepoints joined by ZWJ
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let lines = &[family];
        let wm = WrapMap::build(lines, 80);
        assert_eq!(wm.total_visual_rows(), 1);
        // Should be treated as one grapheme cluster
        let grapheme_count = family.graphemes(true).count();
        assert_eq!(grapheme_count, 1);
    }

    #[test]
    fn extract_text_single_line() {
        let lines = &["hello world"];
        let wm = WrapMap::build(lines, 80);
        let text = wm.extract_text(lines, (0, 0), (0, 4));
        assert_eq!(text, "hello");
    }

    #[test]
    fn extract_text_multi_line() {
        let lines = &["hello world", "foo bar"];
        let wm = WrapMap::build(lines, 80);
        let text = wm.extract_text(lines, (0, 6), (1, 2));
        assert_eq!(text, "world\nfoo");
    }

    #[test]
    fn total_visual_rows_mixed_content() {
        let lines = &[
            "short",
            "this is a longer line that should wrap at width twenty",
            "end",
        ];
        let wm = WrapMap::build(lines, 20);
        assert!(
            wm.total_visual_rows() > 3,
            "Mixed content should produce more than 3 visual rows, got {}",
            wm.total_visual_rows()
        );
    }

    #[test]
    fn trim_false_preserves_leading_whitespace_on_continuation() {
        // With trim=false, continuation lines preserve whitespace
        let lines = &["hello world test"];
        let wm = WrapMap::build(lines, 6);
        assert!(wm.total_visual_rows() >= 2);
        if wm.total_visual_rows() >= 2 {
            let row1_text = &lines[0][wm.rows()[1].start_byte..wm.rows()[1].end_byte];
            assert!(!row1_text.is_empty(), "Continuation row should have content");
        }
    }

    #[test]
    fn empty_line_produces_one_visual_row() {
        let lines = &["hello", "", "world"];
        let wm = WrapMap::build(lines, 80);
        assert_eq!(wm.total_visual_rows(), 3);
        assert_eq!(wm.rows()[1].logical_line, 1);
        assert_eq!(wm.rows()[1].width, 0);
    }
}
