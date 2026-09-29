use unicode_width::UnicodeWidthStr;

/// Single-line text buffer with a cursor, shared by every prompt.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineEditor {
    text: String,
    /// Byte offset, always on a char boundary.
    cursor: usize,
}

impl LineEditor {
    pub fn with_text(text: &str) -> LineEditor {
        LineEditor {
            text: text.to_string(),
            cursor: text.len(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Terminal columns between the start of the text and the cursor.
    pub fn cursor_col(&self) -> usize {
        self.text[..self.cursor].width()
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Deletes the word before the cursor, like Ctrl-w in vim and readline.
    pub fn kill_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| {
            i + trimmed[i..].chars().next().map_or(1, char::len_utf8)
        });
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn kill_to_start(&mut self) {
        self.text.replace_range(..self.cursor, "");
        self.cursor = 0;
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> LineEditor {
        let mut e = LineEditor::default();
        s.chars().for_each(|c| e.insert(c));
        e
    }

    #[test]
    fn typing_and_editing_in_the_middle() {
        let mut e = typed("helo");
        e.left();
        e.insert('l');
        assert_eq!(e.text(), "hello");
        e.home();
        e.delete();
        assert_eq!(e.text(), "ello");
        e.end();
        e.backspace();
        assert_eq!(e.text(), "ell");
    }

    #[test]
    fn multibyte_characters_are_never_split() {
        let mut e = typed("zażółć");
        e.left();
        e.left();
        e.backspace();
        assert_eq!(e.text(), "zażłć");
        e.home();
        e.delete();
        assert_eq!(e.text(), "ażłć");
        e.end();
        e.right();
        e.backspace();
        assert_eq!(e.text(), "ażł");
    }

    #[test]
    fn cursor_column_counts_display_width() {
        let mut e = typed("a漢b");
        assert_eq!(e.cursor_col(), 4);
        e.left();
        assert_eq!(e.cursor_col(), 3);
        e.left();
        assert_eq!(e.cursor_col(), 1);
    }

    #[test]
    fn kill_word_removes_the_word_before_the_cursor() {
        let mut e = typed("cd  some/dir ");
        e.kill_word();
        assert_eq!(e.text(), "cd  ");
        e.kill_word();
        assert_eq!(e.text(), "");
        e.kill_word();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn kill_to_start_keeps_the_text_after_the_cursor() {
        let mut e = typed("abcdef");
        e.left();
        e.left();
        e.kill_to_start();
        assert_eq!(e.text(), "ef");
        assert_eq!(e.cursor_col(), 0);
    }

    #[test]
    fn editing_an_empty_buffer_is_a_no_op() {
        let mut e = LineEditor::default();
        e.backspace();
        e.delete();
        e.left();
        e.right();
        assert!(e.is_empty());
    }
}
