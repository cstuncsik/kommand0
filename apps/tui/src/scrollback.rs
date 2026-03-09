use std::collections::VecDeque;

#[allow(dead_code)]
pub struct ScrollbackBuffer {
    lines: VecDeque<String>,
    capacity: usize,
    scroll_offset: usize,
    new_lines_since_scroll: usize,
}

#[allow(dead_code)]
impl ScrollbackBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(10_000)),
            capacity,
            scroll_offset: 0,
            new_lines_since_scroll: 0,
        }
    }

    pub fn push_line(&mut self, line: String) {
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
            // Adjust scroll offset if we dropped a line above viewport
            if self.scroll_offset > 0 {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
        }
        self.lines.push_back(line);
        if self.scroll_offset > 0 {
            self.new_lines_since_scroll += 1;
        }
    }

    /// Append text to the last line in the buffer (for streaming accumulation).
    /// If the buffer is empty, pushes a new line.
    pub fn append_to_last_line(&mut self, text: &str) {
        if let Some(last) = self.lines.back_mut() {
            last.push_str(text);
        } else {
            self.push_line(text.to_string());
        }
    }

    pub fn push_lines(&mut self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            self.push_line(line);
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    /// Scroll up by `n` lines. Capped so at least `viewport_height` lines remain visible.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset += n;
        // Will be clamped in visible_lines based on actual viewport height
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
        self.new_lines_since_scroll = 0;
    }

    pub fn new_lines_count(&self) -> usize {
        self.new_lines_since_scroll
    }

    pub fn visible_lines(&self, height: usize) -> Vec<&str> {
        if self.lines.is_empty() || height == 0 {
            return Vec::new();
        }
        // Clamp scroll_offset so viewport always shows `height` lines (if available)
        let max_offset = self.lines.len().saturating_sub(height);
        let clamped_offset = self.scroll_offset.min(max_offset);
        let end = self.lines.len().saturating_sub(clamped_offset);
        let start = end.saturating_sub(height);
        self.lines
            .iter()
            .skip(start)
            .take(end - start)
            .map(|s| s.as_str())
            .collect()
    }

    /// Return all lines as string slices (for Paragraph::scroll-based rendering).
    pub fn all_lines(&self) -> Vec<&str> {
        self.lines.iter().map(|s| s.as_str()).collect()
    }

    /// Return the raw scroll_offset (not clamped).
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Clamp scroll_offset to a maximum value. Called by the renderer
    /// after computing the actual max based on visual (wrapped) line count.
    pub fn clamp_scroll_offset(&mut self, max_offset: usize) {
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.scroll_offset = 0;
        self.new_lines_since_scroll = 0;
    }

    /// Jump to the top of the buffer by setting scroll_offset to max.
    /// The offset will be clamped in `visible_lines()` based on viewport height.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = self.lines.len();
    }

    /// Return the total number of lines in the buffer.
    pub fn total_lines(&self) -> usize {
        self.lines.len()
    }

    /// Return scroll_offset clamped so the viewport stays within bounds.
    pub fn clamped_offset(&self, viewport_height: usize) -> usize {
        self.scroll_offset
            .min(self.lines.len().saturating_sub(viewport_height))
    }

    /// Default page size for scrolling. Callers should prefer passing
    /// actual viewport height, but this provides a backward-compatible default.
    pub fn page_size(&self) -> usize {
        20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_empty_and_at_bottom() {
        let buf = ScrollbackBuffer::new(100);
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert!(buf.is_at_bottom());
    }

    #[test]
    fn push_line_increases_len() {
        let mut buf = ScrollbackBuffer::new(100);
        buf.push_line("hello".to_string());
        assert_eq!(buf.len(), 1);
        assert!(!buf.is_empty());
    }

    #[test]
    fn push_line_beyond_capacity_drops_oldest() {
        let mut buf = ScrollbackBuffer::new(3);
        buf.push_line("a".to_string());
        buf.push_line("b".to_string());
        buf.push_line("c".to_string());
        buf.push_line("d".to_string());
        assert_eq!(buf.len(), 3);
        let visible = buf.visible_lines(10);
        assert_eq!(visible, vec!["b", "c", "d"]);
    }

    #[test]
    fn scroll_up_increases_offset() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..20 {
            buf.push_line(format!("line {}", i));
        }
        buf.scroll_up(5);
        assert!(!buf.is_at_bottom());
    }

    #[test]
    fn scroll_down_decreases_offset_to_bottom() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..20 {
            buf.push_line(format!("line {}", i));
        }
        buf.scroll_up(5);
        buf.scroll_down(5);
        assert!(buf.is_at_bottom());
    }

    #[test]
    fn push_line_while_scrolled_increments_new_lines() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..10 {
            buf.push_line(format!("line {}", i));
        }
        buf.scroll_up(3);
        assert_eq!(buf.new_lines_count(), 0);
        buf.push_line("new1".to_string());
        buf.push_line("new2".to_string());
        assert_eq!(buf.new_lines_count(), 2);
    }

    #[test]
    fn push_line_at_bottom_does_not_increment_new_lines() {
        let mut buf = ScrollbackBuffer::new(100);
        buf.push_line("a".to_string());
        buf.push_line("b".to_string());
        assert_eq!(buf.new_lines_count(), 0);
    }

    #[test]
    fn reset_scroll_clears_offset_and_new_lines() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..10 {
            buf.push_line(format!("line {}", i));
        }
        buf.scroll_up(3);
        buf.push_line("extra".to_string());
        buf.reset_scroll();
        assert!(buf.is_at_bottom());
        assert_eq!(buf.new_lines_count(), 0);
    }

    #[test]
    fn visible_lines_returns_correct_slice() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..10 {
            buf.push_line(format!("line {}", i));
        }
        let visible = buf.visible_lines(3);
        assert_eq!(visible, vec!["line 7", "line 8", "line 9"]);
    }

    #[test]
    fn visible_lines_with_scroll_offset() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..10 {
            buf.push_line(format!("line {}", i));
        }
        buf.scroll_up(5);
        let visible = buf.visible_lines(3);
        // With 10 lines, offset 5, viewport 3: end=5, start=2 -> lines 2,3,4
        assert_eq!(visible, vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn scroll_offset_clamped_to_keep_viewport_full() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..10 {
            buf.push_line(format!("line {}", i));
        }
        // Scroll way past the top
        buf.scroll_up(100);
        let visible = buf.visible_lines(3);
        // Should clamp: still shows 3 lines (the first 3)
        assert_eq!(visible, vec!["line 0", "line 1", "line 2"]);
        assert_eq!(visible.len(), 3);
    }

    #[test]
    fn scroll_to_top_shows_first_lines() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..20 {
            buf.push_line(format!("line {}", i));
        }
        buf.scroll_to_top();
        let visible = buf.visible_lines(5);
        assert_eq!(visible, vec!["line 0", "line 1", "line 2", "line 3", "line 4"]);
    }

    #[test]
    fn total_lines_returns_count() {
        let mut buf = ScrollbackBuffer::new(100);
        assert_eq!(buf.total_lines(), 0);
        for i in 0..7 {
            buf.push_line(format!("line {}", i));
        }
        assert_eq!(buf.total_lines(), 7);
    }

    #[test]
    fn clamped_offset_returns_min_of_offset_and_max() {
        let mut buf = ScrollbackBuffer::new(100);
        for i in 0..10 {
            buf.push_line(format!("line {}", i));
        }
        // No scroll -- offset is 0
        assert_eq!(buf.clamped_offset(5), 0);
        // Scroll up 3 -- within bounds
        buf.scroll_up(3);
        assert_eq!(buf.clamped_offset(5), 3);
        // Scroll way past top -- should clamp to max_offset (10-5=5)
        buf.scroll_up(100);
        assert_eq!(buf.clamped_offset(5), 5);
    }

    #[test]
    fn page_size_returns_default() {
        let buf = ScrollbackBuffer::new(100);
        assert_eq!(buf.page_size(), 20);
    }

    #[test]
    fn capacity_50000_works() {
        let mut buf = ScrollbackBuffer::new(50_000);
        for i in 0..50_000 {
            buf.push_line(format!("line {}", i));
        }
        assert_eq!(buf.len(), 50_000);
        // Adding one more should drop oldest
        buf.push_line("overflow".to_string());
        assert_eq!(buf.len(), 50_000);
    }
}
