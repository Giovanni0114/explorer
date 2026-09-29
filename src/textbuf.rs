//! The text being edited. Lines never contain a newline, columns count grapheme clusters, and
//! every change is one `replace` that can be undone and redone exactly.

use std::fmt;

use unicode_segmentation::UnicodeSegmentation;

pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LINE_BYTES: usize = 100 * 1024;

/// A position with the column counted in grapheme clusters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

/// The same place counted in bytes, which stays exact when characters merge or split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BytePos {
    line: usize,
    byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    TooLarge(usize),
    NotText,
    LineTooLong(usize),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::TooLarge(bytes) => write!(
                f,
                "file is {} MB, more than the {} MB this editor takes",
                bytes / (1024 * 1024),
                MAX_BYTES / (1024 * 1024)
            ),
            LoadError::NotText => f.write_str("not a UTF-8 text file"),
            LoadError::LineTooLong(line) => write!(f, "line {line} is too long to edit here"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Edit {
    at: BytePos,
    removed: String,
    inserted: String,
}

impl Edit {
    fn inserted_end(&self) -> BytePos {
        advance(self.at, &self.inserted)
    }

    fn removed_end(&self) -> BytePos {
        advance(self.at, &self.removed)
    }
}

fn advance(from: BytePos, text: &str) -> BytePos {
    match text.rfind('\n') {
        None => BytePos {
            line: from.line,
            byte: from.byte + text.len(),
        },
        Some(last) => BytePos {
            line: from.line + text.matches('\n').count(),
            byte: text.len() - last - 1,
        },
    }
}

#[derive(Debug, Clone, Default)]
struct Group {
    edits: Vec<Edit>,
    before: Pos,
}

#[derive(Debug, Default)]
struct History {
    done: Vec<Group>,
    undone: Vec<Group>,
    open: Option<Group>,
}

#[derive(Debug)]
pub struct Buffer {
    lines: Vec<String>,
    eol: Eol,
    final_newline: bool,
    /// The file was empty, so the first text written to it gets a final newline as vim does.
    newline_for_new_text: bool,
    /// Every line was deleted, so the file is written as empty instead of as one blank line.
    emptied: bool,
    bom: bool,
    history: History,
    version: u64,
}

impl Buffer {
    pub fn from_bytes(bytes: &[u8]) -> Result<Buffer, LoadError> {
        if bytes.len() > MAX_BYTES {
            return Err(LoadError::TooLarge(bytes.len()));
        }
        let (bom, body) = match bytes.strip_prefix(b"\xef\xbb\xbf") {
            Some(rest) => (true, rest),
            None => (false, bytes),
        };
        if body.contains(&0) {
            return Err(LoadError::NotText);
        }
        let text = std::str::from_utf8(body).map_err(|_| LoadError::NotText)?;
        let crlf = text.matches("\r\n").count();
        let bare_lf = text.matches('\n').count() - crlf;
        let eol = if crlf > 0 && crlf >= bare_lf {
            Eol::Crlf
        } else {
            Eol::Lf
        };
        let final_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut lines: Vec<String> = body
            .split('\n')
            .map(|l| {
                let l = if eol == Eol::Crlf {
                    l.strip_suffix('\r').unwrap_or(l)
                } else {
                    l
                };
                l.to_string()
            })
            .collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        if let Some(bad) = lines.iter().position(|l| l.len() > MAX_LINE_BYTES) {
            return Err(LoadError::LineTooLong(bad + 1));
        }
        Ok(Buffer {
            lines,
            eol,
            final_newline,
            newline_for_new_text: text.is_empty(),
            emptied: false,
            bom,
            history: History::default(),
            version: 0,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        if self.emptied {
            return if self.bom {
                b"\xef\xbb\xbf".to_vec()
            } else {
                Vec::new()
            };
        }
        let blank = self.lines.len() == 1 && self.lines[0].is_empty();
        let final_newline = self.final_newline || (self.newline_for_new_text && !blank);
        let sep = match self.eol {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
        };
        let mut out = Vec::new();
        if self.bom {
            out.extend(b"\xef\xbb\xbf");
        }
        out.extend(self.lines.join(sep).as_bytes());
        if final_newline {
            out.extend(sep.as_bytes());
        }
        out
    }

    /// Declares that the buffer holds no lines at all, if it is down to one blank line. The next change ends that.
    pub fn mark_emptied(&mut self) {
        self.emptied = self.lines.len() == 1 && self.lines[0].is_empty();
    }

    /// Increases with every change, so caches can tell when the text moved on.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, index: usize) -> &str {
        &self.lines[index.min(self.lines.len() - 1)]
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Length of a line in grapheme clusters.
    pub fn line_len(&self, index: usize) -> usize {
        grapheme_len(self.line(index))
    }

    pub fn eol(&self) -> Eol {
        self.eol
    }

    /// The last valid position on a line in normal mode, where the cursor sits on a character.
    pub fn clamp(&self, pos: Pos, past_end: bool) -> Pos {
        let line = pos.line.min(self.lines.len() - 1);
        let len = self.line_len(line);
        let max = if past_end { len } else { len.saturating_sub(1) };
        Pos {
            line,
            col: pos.col.min(max),
        }
    }

    /// The text between two positions, lines joined with `\n`. `end` is exclusive.
    pub fn text_between(&self, start: Pos, end: Pos) -> String {
        self.text_between_bytes(self.to_byte(start), self.to_byte(end))
    }

    fn text_between_bytes(&self, start: BytePos, end: BytePos) -> String {
        if start.line == end.line {
            return self.lines[start.line][start.byte..end.byte.max(start.byte)].to_string();
        }
        let mut out = self.lines[start.line][start.byte..].to_string();
        for line in &self.lines[start.line + 1..end.line] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        out.push_str(&self.lines[end.line][..end.byte]);
        out
    }

    /// Replaces the text from `start` up to `end` and returns where the inserted text ends.
    /// The change joins the open undo group.
    pub fn replace(&mut self, start: Pos, end: Pos, text: &str) -> Pos {
        self.emptied = false;
        let (s, e) = (self.to_byte(start), self.to_byte(end));
        let (s, e) = if s <= e { (s, e) } else { (e, s) };
        let edit = Edit {
            at: s,
            removed: self.text_between_bytes(s, e),
            inserted: text.to_string(),
        };
        self.apply(&edit);
        let after = self.to_pos(edit.inserted_end());
        if let Some(group) = self.history.open.as_mut() {
            group.edits.push(edit);
        }
        after
    }

    fn apply(&mut self, edit: &Edit) {
        self.emptied = false;
        let end = edit.removed_end();
        let prefix = self.lines[edit.at.line][..edit.at.byte].to_string();
        let suffix = self.lines[end.line][end.byte..].to_string();
        let joined = format!("{prefix}{}{suffix}", edit.inserted);
        let pieces: Vec<String> = joined.split('\n').map(str::to_string).collect();
        self.lines.splice(edit.at.line..=end.line, pieces);
        self.version += 1;
    }

    fn unapply(&mut self, edit: &Edit) {
        self.emptied = false;
        let end = edit.inserted_end();
        let prefix = self.lines[edit.at.line][..edit.at.byte].to_string();
        let suffix = self.lines[end.line][end.byte..].to_string();
        let joined = format!("{prefix}{}{suffix}", edit.removed);
        let pieces: Vec<String> = joined.split('\n').map(str::to_string).collect();
        self.lines.splice(edit.at.line..=end.line, pieces);
        self.version += 1;
    }

    /// Starts collecting changes into one undo step. `cursor` is where undo puts the cursor back.
    pub fn begin_group(&mut self, cursor: Pos) {
        self.end_group();
        self.history.open = Some(Group {
            edits: Vec::new(),
            before: cursor,
        });
    }

    pub fn end_group(&mut self) {
        if let Some(group) = self.history.open.take()
            && !group.edits.is_empty()
        {
            self.history.done.push(group);
            self.history.undone.clear();
        }
    }

    /// Whether changes since the last time this returned `false` sit in an open group.
    pub fn has_open_changes(&self) -> bool {
        self.history
            .open
            .as_ref()
            .is_some_and(|g| !g.edits.is_empty())
    }

    pub fn undo(&mut self) -> Option<Pos> {
        self.end_group();
        let group = self.history.done.pop()?;
        for edit in group.edits.iter().rev() {
            self.unapply(edit);
        }
        let cursor = group.before;
        self.history.undone.push(group);
        Some(cursor)
    }

    pub fn redo(&mut self) -> Option<Pos> {
        self.end_group();
        let group = self.history.undone.pop()?;
        let mut cursor = group.before;
        for (i, edit) in group.edits.iter().enumerate() {
            self.apply(edit);
            if i == 0 {
                cursor = self.to_pos(edit.at);
            }
        }
        self.history.done.push(group);
        Some(cursor)
    }

    fn to_byte(&self, pos: Pos) -> BytePos {
        let line = pos.line.min(self.lines.len() - 1);
        BytePos {
            line,
            byte: byte_of_col(&self.lines[line], pos.col),
        }
    }

    fn to_pos(&self, at: BytePos) -> Pos {
        let line = at.line.min(self.lines.len() - 1);
        let text = &self.lines[line];
        let byte = at.byte.min(text.len());
        Pos {
            line,
            col: grapheme_len(&text[..byte]),
        }
    }
}

pub fn grapheme_len(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.graphemes(true).count()
    }
}

/// Byte offset of a grapheme column, or the end of the line when the column is past it.
pub fn byte_of_col(text: &str, col: usize) -> usize {
    if text.is_ascii() {
        return col.min(text.len());
    }
    text.grapheme_indices(true)
        .nth(col)
        .map_or(text.len(), |(i, _)| i)
}

/// The grapheme cluster at a column.
pub fn grapheme_at(text: &str, col: usize) -> Option<&str> {
    if text.is_ascii() {
        return text.get(col..col + 1);
    }
    text.graphemes(true).nth(col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer::from_bytes(text.as_bytes()).unwrap()
    }

    fn text(b: &Buffer) -> String {
        String::from_utf8(b.to_bytes()).unwrap()
    }

    #[test]
    fn plain_text_round_trips_byte_for_byte() {
        for original in [
            "one\ntwo\nthree\n",
            "no final newline",
            "",
            "\n",
            "\n\n\n",
            "trailing spaces  \n  and indent\n",
            "zażółć gęślą jaźń\n",
            "tab\tinside\n",
        ] {
            assert_eq!(text(&buf(original)), original, "{original:?}");
        }
    }

    #[test]
    fn an_empty_file_stays_empty_until_text_is_typed_and_then_gets_a_final_newline() {
        let mut b = buf("");
        assert_eq!(text(&b), "");
        b.replace(Pos::new(0, 0), Pos::new(0, 0), "hi");
        assert_eq!(text(&b), "hi\n");
        let mut one_line = buf("x");
        one_line.replace(Pos::new(0, 1), Pos::new(0, 1), "y");
        assert_eq!(
            text(&one_line),
            "xy",
            "a file without a final newline keeps that"
        );
        assert_eq!(
            text(&buf("\n")),
            "\n",
            "a file of one empty line is not empty"
        );
    }

    #[test]
    fn a_buffer_with_every_line_deleted_is_written_empty_until_it_changes_again() {
        let mut b = buf("one\ntwo\n");
        b.replace(Pos::new(0, 0), Pos::new(1, 3), "");
        assert_eq!(text(&b), "\n", "one blank line is a line");
        b.mark_emptied();
        assert_eq!(text(&b), "");
        b.replace(Pos::new(0, 0), Pos::new(0, 0), "x");
        assert_eq!(text(&b), "x\n");
        b.begin_group(Pos::default());
        b.replace(Pos::new(0, 0), Pos::new(0, 1), "");
        b.mark_emptied();
        b.end_group();
        assert_eq!(text(&b), "");
        b.undo();
        assert_eq!(text(&b), "x\n", "undo ends the emptied state");
    }

    #[test]
    fn crlf_and_bom_are_kept() {
        let original = "\u{feff}a\r\nb\r\nc\r\n";
        let b = buf(original);
        assert_eq!(b.eol(), Eol::Crlf);
        assert_eq!(b.lines(), ["a", "b", "c"]);
        assert_eq!(text(&b), original);
        assert_eq!(
            text(&buf("a\r\nb")),
            "a\r\nb",
            "no final newline stays that way"
        );
    }

    #[test]
    fn mixed_endings_take_the_majority_and_are_normalised() {
        let b = buf("a\r\nb\r\nc\n");
        assert_eq!(b.eol(), Eol::Crlf);
        assert_eq!(text(&b), "a\r\nb\r\nc\r\n");
        assert_eq!(buf("a\nb\r\nc\nd\n").eol(), Eol::Lf);
    }

    #[test]
    fn unsuitable_files_are_refused_with_a_reason() {
        assert_eq!(
            Buffer::from_bytes(b"bin\0ary").unwrap_err(),
            LoadError::NotText
        );
        assert_eq!(
            Buffer::from_bytes(&[0xff, 0xfe, b'a']).unwrap_err(),
            LoadError::NotText
        );
        assert!(matches!(
            Buffer::from_bytes(&vec![b'a'; MAX_BYTES + 1]).unwrap_err(),
            LoadError::TooLarge(_)
        ));
        let long = format!("ok\n{}\n", "x".repeat(MAX_LINE_BYTES + 1));
        assert_eq!(
            Buffer::from_bytes(long.as_bytes()).unwrap_err(),
            LoadError::LineTooLong(2)
        );
        assert!(
            LoadError::TooLarge(3 * 1024 * 1024)
                .to_string()
                .contains("3 MB")
        );
    }

    #[test]
    fn replace_handles_single_line_multi_line_and_joins() {
        let mut b = buf("hello world\nsecond\nthird\n");
        let end = b.replace(Pos::new(0, 5), Pos::new(0, 11), " there");
        assert_eq!(b.line(0), "hello there");
        assert_eq!(end, Pos::new(0, 11));
        let end = b.replace(Pos::new(0, 5), Pos::new(0, 5), "\nnew\nlines\n");
        assert_eq!(
            b.lines(),
            ["hello", "new", "lines", " there", "second", "third"]
        );
        assert_eq!(end, Pos::new(3, 0));
        b.replace(Pos::new(1, 2), Pos::new(4, 3), "");
        assert_eq!(b.lines(), ["hello", "neond", "third"]);
        assert_eq!(
            b.text_between(Pos::new(0, 3), Pos::new(2, 2)),
            "lo\nneond\nth"
        );
    }

    #[test]
    fn columns_count_graphemes_not_bytes() {
        let mut b = buf("a👍🏽é\u{301}b\n");
        assert_eq!(
            b.line_len(0),
            4,
            "the emoji with skin tone and e with accent are one each"
        );
        b.replace(Pos::new(0, 1), Pos::new(0, 3), "-");
        assert_eq!(b.line(0), "a-b");
        assert_eq!(grapheme_at("a👍🏽b", 1), Some("👍🏽"));
        assert_eq!(byte_of_col("añb", 2), 3);
        assert_eq!(byte_of_col("ab", 9), 2);
    }

    #[test]
    fn clamping_keeps_the_cursor_on_a_character_in_normal_mode() {
        let b = buf("abc\n\nxy\n");
        assert_eq!(b.clamp(Pos::new(0, 9), false), Pos::new(0, 2));
        assert_eq!(b.clamp(Pos::new(0, 9), true), Pos::new(0, 3));
        assert_eq!(b.clamp(Pos::new(1, 4), false), Pos::new(1, 0));
        assert_eq!(b.clamp(Pos::new(99, 0), false), Pos::new(2, 0));
    }

    #[test]
    fn undo_and_redo_restore_each_group_and_the_cursor() {
        let mut b = buf("alpha\nbeta\n");
        b.begin_group(Pos::new(0, 1));
        b.replace(Pos::new(0, 0), Pos::new(0, 5), "ALPHA");
        b.replace(Pos::new(0, 5), Pos::new(0, 5), "!");
        b.end_group();
        b.begin_group(Pos::new(1, 0));
        b.replace(Pos::new(1, 0), Pos::new(1, 4), "");
        b.end_group();
        assert_eq!(b.lines(), ["ALPHA!", ""]);
        assert_eq!(b.undo(), Some(Pos::new(1, 0)));
        assert_eq!(b.lines(), ["ALPHA!", "beta"]);
        assert_eq!(b.undo(), Some(Pos::new(0, 1)));
        assert_eq!(b.lines(), ["alpha", "beta"]);
        assert_eq!(b.undo(), None);
        assert_eq!(b.redo(), Some(Pos::new(0, 0)));
        assert_eq!(b.lines(), ["ALPHA!", "beta"]);
        assert!(b.redo().is_some());
        assert_eq!(b.redo(), None);
        assert_eq!(b.lines(), ["ALPHA!", ""]);
    }

    #[test]
    fn a_new_change_after_undo_drops_the_redo_history() {
        let mut b = buf("x\n");
        b.begin_group(Pos::default());
        b.replace(Pos::new(0, 1), Pos::new(0, 1), "y");
        b.end_group();
        b.undo();
        b.begin_group(Pos::default());
        b.replace(Pos::new(0, 1), Pos::new(0, 1), "z");
        b.end_group();
        assert_eq!(b.redo(), None);
        assert_eq!(b.lines(), ["xz"]);
    }

    #[test]
    fn changes_outside_a_group_are_not_undoable_and_empty_groups_vanish() {
        let mut b = buf("x\n");
        b.replace(Pos::new(0, 0), Pos::new(0, 1), "y");
        assert_eq!(b.undo(), None);
        b.begin_group(Pos::default());
        b.end_group();
        assert_eq!(b.undo(), None);
        assert_eq!(b.line(0), "y");
    }

    #[test]
    fn version_moves_on_every_change_including_undo() {
        let mut b = buf("x\n");
        let v0 = b.version();
        b.begin_group(Pos::default());
        b.replace(Pos::new(0, 0), Pos::new(0, 0), "a");
        b.end_group();
        let v1 = b.version();
        assert!(v1 > v0);
        b.undo();
        assert!(b.version() > v1);
    }

    /// A tiny deterministic generator so the randomised test needs no extra crate.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % below
        }
    }

    #[test]
    fn random_edits_always_undo_to_the_start_and_redo_to_the_end() {
        const PIECES: [&str; 9] = [
            "a",
            "b\n",
            "\n",
            "zażółć",
            "👍🏽",
            "e\u{301}",
            "  ",
            "xyz\nabc",
            "",
        ];
        for seed in 0..200 {
            let mut rng = Rng(seed + 1);
            let mut b = buf("first line\nsecond ✓ line\n\nfourth\n");
            let start = text(&b);
            for _ in 0..(1 + rng.next(6)) {
                b.begin_group(Pos::default());
                for _ in 0..(1 + rng.next(4)) {
                    let line = rng.next(b.line_count());
                    let len = b.line_len(line);
                    let a = Pos::new(line, rng.next(len + 1));
                    let end_line = line + rng.next(b.line_count() - line);
                    let e = Pos::new(end_line, rng.next(b.line_len(end_line) + 1));
                    let (a, e) = if a <= e { (a, e) } else { (e, a) };
                    b.replace(a, e, PIECES[rng.next(PIECES.len())]);
                }
                b.end_group();
            }
            let end = b.lines().to_vec();
            while b.undo().is_some() {}
            assert_eq!(text(&b), start, "seed {seed}");
            while b.redo().is_some() {}
            assert_eq!(b.lines(), end, "seed {seed}");
        }
    }
}
