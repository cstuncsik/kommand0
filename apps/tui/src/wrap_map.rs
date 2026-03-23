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
    pub fn build(lines: &[&str], pane_width: usize) -> Self {
        let mut rows = Vec::new();

        if lines.is_empty() {
            return Self { rows, pane_width };
        }

        for (line_idx, line) in lines.iter().enumerate() {
            if line.is_empty() {
                rows.push(VisualRow {
                    logical_line: line_idx,
                    start_byte: 0,
                    end_byte: 0,
                    width: 0,
                });
                continue;
            }

            Self::wrap_line(line, line_idx, pane_width, &mut rows);
        }

        Self { rows, pane_width }
    }

    /// Wrap a single logical line into visual rows.
    /// Replicates ratatui WordWrapper with trim=false.
    fn wrap_line(line: &str, line_idx: usize, pane_width: usize, rows: &mut Vec<VisualRow>) {
        // Collect grapheme info: (byte_start, byte_end, display_width, is_whitespace)
        let graphemes: Vec<(usize, usize, usize, bool)> = {
            let indices: Vec<(usize, &str)> = line.grapheme_indices(true).collect();
            indices
                .iter()
                .enumerate()
                .map(|(i, &(byte_start, g))| {
                    let byte_end = if i + 1 < indices.len() {
                        indices[i + 1].0
                    } else {
                        line.len()
                    };
                    let w = UnicodeWidthStr::width(g);
                    let is_ws = g.chars().all(|c| c.is_whitespace());
                    (byte_start, byte_end, w, is_ws)
                })
                .collect()
        };

        // State for word-wrapping
        let mut current_row_start: usize = 0;
        let mut current_row_width: usize = 0;
        let mut current_row_end: usize = 0; // byte end of last content on current row

        // Pending whitespace and word buffers
        // Each entry: (byte_start, byte_end, display_width)
        let mut pending_ws: Vec<(usize, usize, usize)> = Vec::new();
        let mut pending_word: Vec<(usize, usize, usize)> = Vec::new();

        for &(byte_start, byte_end, g_width, is_ws) in &graphemes {
            // Skip graphemes wider than pane_width
            if g_width > pane_width {
                continue;
            }

            if is_ws {
                // Whitespace: flush any pending word first
                if !pending_word.is_empty() {
                    Self::flush_word(
                        line_idx,
                        pane_width,
                        rows,
                        &mut current_row_start,
                        &mut current_row_width,
                        &mut current_row_end,
                        &mut pending_ws,
                        &mut pending_word,
                    );
                }
                pending_ws.push((byte_start, byte_end, g_width));
            } else {
                pending_word.push((byte_start, byte_end, g_width));
            }
        }

        // Flush remaining content
        if !pending_word.is_empty() {
            Self::flush_word(
                line_idx,
                pane_width,
                rows,
                &mut current_row_start,
                &mut current_row_width,
                &mut current_row_end,
                &mut pending_ws,
                &mut pending_word,
            );
        } else if !pending_ws.is_empty() {
            // Trailing whitespace only
            let ws_width: usize = pending_ws.iter().map(|e| e.2).sum();
            if current_row_width + ws_width <= pane_width {
                current_row_width += ws_width;
                current_row_end = pending_ws.last().unwrap().1;
            } else {
                // Emit current row, start new with trailing ws
                if current_row_width > 0 || current_row_end > current_row_start {
                    rows.push(VisualRow {
                        logical_line: line_idx,
                        start_byte: current_row_start,
                        end_byte: current_row_end,
                        width: current_row_width as u16,
                    });
                }
                current_row_start = pending_ws[0].0;
                current_row_width = ws_width;
                current_row_end = pending_ws.last().unwrap().1;
            }
            pending_ws.clear();
        }

        // Emit final row
        rows.push(VisualRow {
            logical_line: line_idx,
            start_byte: current_row_start,
            end_byte: current_row_end,
            width: current_row_width as u16,
        });
    }

    /// Flush pending whitespace + word onto current row, wrapping if needed.
    /// Handles overlong words (exceeding pane_width) with character-level breaks.
    fn flush_word(
        line_idx: usize,
        pane_width: usize,
        rows: &mut Vec<VisualRow>,
        current_row_start: &mut usize,
        current_row_width: &mut usize,
        current_row_end: &mut usize,
        pending_ws: &mut Vec<(usize, usize, usize)>,
        pending_word: &mut Vec<(usize, usize, usize)>,
    ) {
        let ws_width: usize = pending_ws.iter().map(|e| e.2).sum();
        let word_width: usize = pending_word.iter().map(|e| e.2).sum();

        if *current_row_width + ws_width + word_width <= pane_width {
            // Fits on current row
            *current_row_width += ws_width + word_width;
            *current_row_end = pending_word.last().unwrap().1;
        } else if ws_width + word_width <= pane_width {
            // Word fits on a new row (with whitespace), but not on current row
            // Emit current row if it has content
            if *current_row_width > 0 || *current_row_end > *current_row_start {
                rows.push(VisualRow {
                    logical_line: line_idx,
                    start_byte: *current_row_start,
                    end_byte: *current_row_end,
                    width: *current_row_width as u16,
                });
            }
            // trim=false: new row starts with the whitespace
            let new_start = if !pending_ws.is_empty() {
                pending_ws[0].0
            } else {
                pending_word[0].0
            };
            *current_row_start = new_start;
            *current_row_width = ws_width + word_width;
            *current_row_end = pending_word.last().unwrap().1;
        } else {
            // Word itself exceeds pane_width: character-level break needed
            // Emit current row if it has content
            if *current_row_width > 0 || *current_row_end > *current_row_start {
                rows.push(VisualRow {
                    logical_line: line_idx,
                    start_byte: *current_row_start,
                    end_byte: *current_row_end,
                    width: *current_row_width as u16,
                });
            }

            // Include whitespace at start of first break line (trim=false)
            let ws_start = pending_ws.first().map(|e| e.0);
            let word_start = pending_word[0].0;
            let effective_start = ws_start.unwrap_or(word_start);

            let mut partial_start = effective_start;
            let mut partial_width: usize = ws_width;
            let mut partial_end = if !pending_ws.is_empty() {
                pending_ws.last().unwrap().1
            } else {
                word_start
            };

            for &(bs, be, gw) in pending_word.iter() {
                if partial_width + gw > pane_width && partial_width > 0 {
                    rows.push(VisualRow {
                        logical_line: line_idx,
                        start_byte: partial_start,
                        end_byte: partial_end,
                        width: partial_width as u16,
                    });
                    partial_start = bs;
                    partial_width = 0;
                }
                partial_width += gw;
                partial_end = be;
            }

            *current_row_start = partial_start;
            *current_row_width = partial_width;
            *current_row_end = partial_end;
        }

        pending_ws.clear();
        pending_word.clear();
    }

    /// Screen (x, y) -> (logical_line, grapheme_offset).
    /// Needs access to original lines to walk grapheme clusters.
    pub fn screen_to_logical(
        &self,
        x: u16,
        y: u16,
        scroll_from_top: usize,
        lines: &[&str],
    ) -> Option<(usize, usize)> {
        let visual_row_idx = y as usize + scroll_from_top;
        let row = self.rows.get(visual_row_idx)?;
        let line = lines.get(row.logical_line)?;
        let row_text = &line[row.start_byte..row.end_byte];

        // Count graphemes before this row's start to get base offset
        let base_grapheme_offset = line[..row.start_byte].graphemes(true).count();

        let mut col = 0u16;
        let mut grapheme_idx = 0usize;

        for grapheme in row_text.graphemes(true) {
            let g_width = UnicodeWidthStr::width(grapheme) as u16;
            if col + g_width > x {
                return Some((row.logical_line, base_grapheme_offset + grapheme_idx));
            }
            col += g_width;
            grapheme_idx += 1;
        }

        // Past end of row content: clamp to last position
        if grapheme_idx > 0 {
            Some((
                row.logical_line,
                base_grapheme_offset + grapheme_idx.saturating_sub(1),
            ))
        } else {
            Some((row.logical_line, base_grapheme_offset))
        }
    }

    /// (logical_line, grapheme_offset) -> (x_column, y_visual_row).
    pub fn logical_to_screen(
        &self,
        line: usize,
        char_offset: usize,
        scroll_from_top: usize,
        lines: &[&str],
    ) -> Option<(u16, u16)> {
        let source_line = lines.get(line)?;

        // Find the byte offset for the given grapheme index
        let mut target_byte_offset = source_line.len();
        for (i, (byte_idx, _)) in source_line.grapheme_indices(true).enumerate() {
            if i == char_offset {
                target_byte_offset = byte_idx;
                break;
            }
        }

        // Find which visual row contains this byte offset
        for (row_idx, row) in self.rows.iter().enumerate() {
            if row.logical_line != line {
                continue;
            }
            let in_range = if row.start_byte == row.end_byte {
                // Empty row
                target_byte_offset == row.start_byte
            } else {
                target_byte_offset >= row.start_byte && target_byte_offset < row.end_byte
            };

            if in_range {
                let row_text = &source_line[row.start_byte..row.end_byte];
                let mut x = 0u16;
                for (byte_idx, grapheme) in row_text.grapheme_indices(true) {
                    if row.start_byte + byte_idx >= target_byte_offset {
                        break;
                    }
                    x += UnicodeWidthStr::width(grapheme) as u16;
                }

                if row_idx < scroll_from_top {
                    return None;
                }
                let y = (row_idx - scroll_from_top) as u16;
                return Some((x, y));
            }
        }

        // char_offset at end of line: use last row for this line
        for (row_idx, row) in self.rows.iter().enumerate().rev() {
            if row.logical_line == line {
                let row_text = &source_line[row.start_byte..row.end_byte];
                let x: u16 = row_text
                    .graphemes(true)
                    .map(|g| UnicodeWidthStr::width(g) as u16)
                    .sum();
                if row_idx < scroll_from_top {
                    return None;
                }
                let y = (row_idx - scroll_from_top) as u16;
                return Some((x, y));
            }
        }

        None
    }

    /// Total visual row count.
    pub fn total_visual_rows(&self) -> usize {
        self.rows.len()
    }

    /// Extract text between two logical positions.
    /// start and end are (line, grapheme_index) in document order (inclusive).
    pub fn extract_text(
        &self,
        lines: &[&str],
        start: (usize, usize),
        end: (usize, usize),
    ) -> String {
        let (start_line, start_char) = start;
        let (end_line, end_char) = end;

        if start_line == end_line {
            let line = match lines.get(start_line) {
                Some(l) => l,
                None => return String::new(),
            };
            let graphemes: Vec<&str> = line.graphemes(true).collect();
            let end_idx = (end_char + 1).min(graphemes.len());
            if start_char >= graphemes.len() {
                return String::new();
            }
            graphemes[start_char..end_idx].join("")
        } else {
            let mut result = Vec::new();

            // First line: from start_char to end
            if let Some(line) = lines.get(start_line) {
                let graphemes: Vec<&str> = line.graphemes(true).collect();
                if start_char < graphemes.len() {
                    result.push(graphemes[start_char..].join(""));
                }
            }

            // Middle lines: full
            for line_idx in (start_line + 1)..end_line {
                if let Some(line) = lines.get(line_idx) {
                    result.push(line.to_string());
                }
            }

            // Last line: from start to end_char
            if let Some(line) = lines.get(end_line) {
                let graphemes: Vec<&str> = line.graphemes(true).collect();
                let end_idx = (end_char + 1).min(graphemes.len());
                result.push(graphemes[..end_idx].join(""));
            }

            result.join("\n")
        }
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
