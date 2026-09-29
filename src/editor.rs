//! A small vim-like editor for quick changes to one file. It edits a [`Buffer`] and asks its
//! owner, through [`EditEvent`], to do anything that touches the disk.

use std::path::{Path, PathBuf};

use ratatui::crossterm::event::KeyCode;
use unicode_width::UnicodeWidthStr;

use crate::{
    editmotion::{self, Find, Kind, Range, Target},
    keys::{Cmd, Fed, InputState, Key, Keymap},
    lineedit::LineEditor,
    model::PreviewKey,
    preview::{self, Line},
    prompt::{Prompt, PromptEvent, PromptKind},
    textbuf::{Buffer, LoadError, Pos, byte_of_col, grapheme_len},
};

const TAB_WIDTH: usize = 4;
const HIGHLIGHT_LINES: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditCommand {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    FirstNonBlank,
    LineEnd,
    WordFwd,
    WordBack,
    WordEnd,
    BigWordFwd,
    BigWordBack,
    BigWordEnd,
    FirstLine,
    LastLine,
    FindChar,
    FindCharBack,
    TillChar,
    TillCharBack,
    RepeatFind,
    RepeatFindBack,
    ParaFwd,
    ParaBack,
    SearchNext,
    SearchPrev,
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
    Delete,
    Change,
    Yank,
    InsertBefore,
    Append,
    InsertAtStart,
    AppendAtEnd,
    OpenBelow,
    OpenAbove,
    DeleteChar,
    DeleteCharBack,
    DeleteToEnd,
    ChangeToEnd,
    SubstituteChar,
    SubstituteLine,
    ReplaceChar,
    Join,
    ToggleCase,
    PasteAfter,
    PasteBefore,
    Undo,
    Redo,
    SearchForward,
    SearchBackward,
    ExPrompt,
    WriteQuit,
    QuitDiscard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOp {
    Delete,
    Change,
    Yank,
}

impl Cmd for EditCommand {
    type Op = EditOp;

    fn operator(self) -> Option<EditOp> {
        match self {
            EditCommand::Delete => Some(EditOp::Delete),
            EditCommand::Change => Some(EditOp::Change),
            EditCommand::Yank => Some(EditOp::Yank),
            _ => None,
        }
    }

    fn is_motion(self) -> bool {
        use EditCommand::*;
        matches!(
            self,
            Left | Right
                | Up
                | Down
                | LineStart
                | FirstNonBlank
                | LineEnd
                | WordFwd
                | WordBack
                | WordEnd
                | BigWordFwd
                | BigWordBack
                | BigWordEnd
                | FirstLine
                | LastLine
                | FindChar
                | FindCharBack
                | TillChar
                | TillCharBack
                | RepeatFind
                | RepeatFindBack
                | ParaFwd
                | ParaBack
                | SearchNext
                | SearchPrev
                | HalfDown
                | HalfUp
                | PageDown
                | PageUp
        )
    }

    fn wants_char(self) -> bool {
        use EditCommand::*;
        matches!(
            self,
            FindChar | FindCharBack | TillChar | TillCharBack | ReplaceChar
        )
    }
}

const DEFAULT_KEYS: &[(&str, EditCommand)] = &[
    ("h", EditCommand::Left),
    ("<left>", EditCommand::Left),
    ("l", EditCommand::Right),
    ("<right>", EditCommand::Right),
    ("k", EditCommand::Up),
    ("<up>", EditCommand::Up),
    ("j", EditCommand::Down),
    ("<down>", EditCommand::Down),
    ("0", EditCommand::LineStart),
    ("<home>", EditCommand::LineStart),
    ("^", EditCommand::FirstNonBlank),
    ("$", EditCommand::LineEnd),
    ("<end>", EditCommand::LineEnd),
    ("w", EditCommand::WordFwd),
    ("b", EditCommand::WordBack),
    ("e", EditCommand::WordEnd),
    ("W", EditCommand::BigWordFwd),
    ("B", EditCommand::BigWordBack),
    ("E", EditCommand::BigWordEnd),
    ("gg", EditCommand::FirstLine),
    ("G", EditCommand::LastLine),
    ("f", EditCommand::FindChar),
    ("F", EditCommand::FindCharBack),
    ("t", EditCommand::TillChar),
    ("T", EditCommand::TillCharBack),
    (";", EditCommand::RepeatFind),
    (",", EditCommand::RepeatFindBack),
    ("}", EditCommand::ParaFwd),
    ("{", EditCommand::ParaBack),
    ("n", EditCommand::SearchNext),
    ("N", EditCommand::SearchPrev),
    ("<c-d>", EditCommand::HalfDown),
    ("<c-u>", EditCommand::HalfUp),
    ("<c-f>", EditCommand::PageDown),
    ("<pagedown>", EditCommand::PageDown),
    ("<c-b>", EditCommand::PageUp),
    ("<pageup>", EditCommand::PageUp),
    ("d", EditCommand::Delete),
    ("c", EditCommand::Change),
    ("y", EditCommand::Yank),
    ("i", EditCommand::InsertBefore),
    ("a", EditCommand::Append),
    ("I", EditCommand::InsertAtStart),
    ("A", EditCommand::AppendAtEnd),
    ("o", EditCommand::OpenBelow),
    ("O", EditCommand::OpenAbove),
    ("x", EditCommand::DeleteChar),
    ("<del>", EditCommand::DeleteChar),
    ("X", EditCommand::DeleteCharBack),
    ("D", EditCommand::DeleteToEnd),
    ("C", EditCommand::ChangeToEnd),
    ("s", EditCommand::SubstituteChar),
    ("S", EditCommand::SubstituteLine),
    ("r", EditCommand::ReplaceChar),
    ("J", EditCommand::Join),
    ("~", EditCommand::ToggleCase),
    ("p", EditCommand::PasteAfter),
    ("P", EditCommand::PasteBefore),
    ("u", EditCommand::Undo),
    ("<c-r>", EditCommand::Redo),
    ("/", EditCommand::SearchForward),
    ("?", EditCommand::SearchBackward),
    (":", EditCommand::ExPrompt),
    ("ZZ", EditCommand::WriteQuit),
    ("ZQ", EditCommand::QuitDiscard),
];

fn edit_keymap() -> Keymap<EditCommand> {
    let bindings = DEFAULT_KEYS
        .iter()
        .map(|(keys, command)| (Key::parse_seq(keys).expect("valid default key"), *command))
        .collect();
    Keymap::from_bindings(bindings).expect("default editor keys do not shadow each other")
}

/// What the editor needs its owner to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditEvent {
    /// Write the text to the file. `then_close` leaves the editor once that has worked.
    Save { force: bool, then_close: bool },
    /// Leave the editor now.
    Close,
    /// Throw the edits away and read the file again.
    Reload,
}

enum EditEx {
    Write { force: bool },
    Quit { force: bool },
    WriteQuit { force: bool },
    Reload,
    Goto(usize),
}

fn parse_ex(text: &str) -> Result<EditEx, String> {
    let text = text.trim();
    if let Ok(line) = text.parse::<usize>() {
        return Ok(EditEx::Goto(line));
    }
    match text {
        "$" => Ok(EditEx::Goto(usize::MAX)),
        "w" | "write" => Ok(EditEx::Write { force: false }),
        "w!" | "write!" => Ok(EditEx::Write { force: true }),
        "q" | "quit" => Ok(EditEx::Quit { force: false }),
        "q!" | "quit!" => Ok(EditEx::Quit { force: true }),
        "wq" | "x" | "xit" => Ok(EditEx::WriteQuit { force: false }),
        "wq!" | "x!" => Ok(EditEx::WriteQuit { force: true }),
        "e!" | "edit!" => Ok(EditEx::Reload),
        "" => Err(String::new()),
        other => Err(format!("not an editor command: {other}")),
    }
}

#[derive(Debug, Clone)]
struct Register {
    text: String,
    linewise: bool,
}

enum Mode {
    Normal,
    Insert,
    Prompt(Prompt),
}

pub struct Editor {
    path: PathBuf,
    buf: Buffer,
    cursor: Pos,
    /// The column `j` and `k` aim for, or `usize::MAX` after `$`.
    want_col: usize,
    mode: Mode,
    input: InputState<EditCommand>,
    keymap: Keymap<EditCommand>,
    register: Option<Register>,
    last_find: Option<Find>,
    last_search: Option<String>,
    search_forward: bool,
    top: usize,
    rows: usize,
    saved_version: u64,
    disk_key: PreviewKey,
    tab_text: &'static str,
    highlight: Option<(u64, Vec<Line>)>,
    /// One-line notice for the footer, cleared by the next key.
    pub message: Option<String>,
}

impl Editor {
    pub fn open(path: PathBuf, bytes: &[u8], disk_key: PreviewKey) -> Result<Editor, LoadError> {
        let buf = Buffer::from_bytes(bytes)?;
        let tab_indented = buf.lines().iter().filter(|l| l.starts_with('\t')).count();
        let space_indented = buf.lines().iter().filter(|l| l.starts_with("  ")).count();
        Ok(Editor {
            path,
            saved_version: buf.version(),
            buf,
            cursor: Pos::default(),
            want_col: 0,
            mode: Mode::Normal,
            input: InputState::default(),
            keymap: edit_keymap(),
            register: None,
            last_find: None,
            last_search: None,
            search_forward: true,
            top: 0,
            rows: 24,
            disk_key,
            tab_text: if tab_indented > space_indented {
                "\t"
            } else {
                "    "
            },
            highlight: None,
            message: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dirty(&self) -> bool {
        self.buf.version() != self.saved_version
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.buf.to_bytes()
    }

    pub fn disk_key(&self) -> PreviewKey {
        self.disk_key
    }

    pub fn line_count(&self) -> usize {
        self.buf.line_count()
    }

    /// Records that the text now matches the file, whose new fingerprint is `key`.
    pub fn mark_saved(&mut self, key: PreviewKey) {
        self.saved_version = self.buf.version();
        self.disk_key = key;
        let bytes = self.buf.to_bytes().len();
        self.message = Some(format!(
            "written, {} line{}, {bytes} bytes",
            self.buf.line_count(),
            if self.buf.line_count() == 1 { "" } else { "s" }
        ));
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn top(&self) -> usize {
        self.top
    }

    pub fn is_insert(&self) -> bool {
        matches!(self.mode, Mode::Insert)
    }

    /// The half-typed command, such as `2d`.
    pub fn pending(&self) -> String {
        self.input.display()
    }

    /// The prompt at the bottom, as its label, text and cursor column.
    pub fn prompt_view(&self) -> Option<(&'static str, &str, usize)> {
        let Mode::Prompt(prompt) = &self.mode else {
            return None;
        };
        let label = match prompt.kind {
            PromptKind::Search if !self.search_forward => "?",
            _ => prompt.label(),
        };
        Some((label, prompt.editor.text(), prompt.editor.cursor_col()))
    }

    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows.max(1);
        self.scroll_to_cursor();
    }

    pub fn line_text(&self, index: usize) -> &str {
        self.buf.line(index)
    }

    /// Syntax colors for a line. A line changed since the colors were made shows plain until the next refresh.
    pub fn styled_line(&self, index: usize) -> Line {
        let text = self.buf.line(index);
        if let Some((_, lines)) = &self.highlight
            && let Some(line) = lines.get(index)
            && line.text() == text
        {
            return line.clone();
        }
        Line(vec![preview::Span {
            text: text.to_string(),
            color: crate::theme::FG,
            bold: false,
            italic: false,
        }])
    }

    /// Whether the colors are older than the text.
    pub fn highlight_stale(&self) -> bool {
        self.highlight
            .as_ref()
            .is_none_or(|(version, _)| *version != self.buf.version())
    }

    /// Recomputes syntax colors down to the bottom of the view.
    pub fn refresh_highlight(&mut self) {
        if !self.highlight_stale() {
            return;
        }
        let shown = (self.top + self.rows)
            .max(self.rows * 2)
            .min(self.buf.line_count())
            .min(HIGHLIGHT_LINES);
        let mut lines = preview::highlight(&self.buf.lines()[..shown], &self.path);
        lines.truncate(shown);
        self.highlight = Some((self.buf.version(), lines));
    }

    /// Takes the text of the file again after `:e!`, keeping the cursor where it can stay.
    pub fn reload(&mut self, bytes: &[u8], disk_key: PreviewKey) -> Result<(), LoadError> {
        let buf = Buffer::from_bytes(bytes)?;
        self.saved_version = buf.version();
        self.buf = buf;
        self.disk_key = disk_key;
        self.highlight = None;
        self.mode = Mode::Normal;
        self.input.reset();
        self.cursor = self.buf.clamp(self.cursor, false);
        self.scroll_to_cursor();
        Ok(())
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Terminal column of the cursor within its line, with tabs expanded.
    pub fn cursor_display_col(&self) -> usize {
        display_col(self.buf.line(self.cursor.line), self.cursor.col)
    }

    pub fn press(&mut self, key: Key) -> Option<EditEvent> {
        // A terminal sends Esc and a fast next key as one Alt chord. Nothing here is bound to Alt, so split it.
        if key.alt {
            self.press(Key::plain(KeyCode::Esc));
            return self.press(Key { alt: false, ..key });
        }
        self.message = None;
        let event = match self.mode {
            Mode::Prompt(_) => self.press_prompt(key),
            Mode::Insert => {
                self.press_insert(key);
                None
            }
            Mode::Normal => self.press_normal(key),
        };
        self.settle_cursor();
        event
    }

    fn settle_cursor(&mut self) {
        self.cursor = self.buf.clamp(self.cursor, self.is_insert());
        self.scroll_to_cursor();
    }

    fn scroll_to_cursor(&mut self) {
        if self.cursor.line < self.top {
            self.top = self.cursor.line;
        } else if self.cursor.line >= self.top + self.rows {
            self.top = self.cursor.line + 1 - self.rows;
        }
        self.top = self.top.min(self.buf.line_count().saturating_sub(1));
    }

    fn press_normal(&mut self, key: Key) -> Option<EditEvent> {
        match self.input.feed(key, &self.keymap, false) {
            Fed::Run {
                command,
                count,
                arg,
            } => {
                self.buf.begin_group(self.cursor);
                let event = self.run(command, count, arg);
                if !self.is_insert() {
                    self.buf.end_group();
                }
                event
            }
            Fed::Operate {
                operator,
                motion,
                count,
            } => {
                self.buf.begin_group(self.cursor);
                self.operate(operator, motion, count);
                if !self.is_insert() {
                    self.buf.end_group();
                }
                None
            }
            Fed::Pending | Fed::Unbound => None,
        }
    }

    fn run(
        &mut self,
        command: EditCommand,
        count: Option<usize>,
        arg: Option<char>,
    ) -> Option<EditEvent> {
        use EditCommand::*;
        let n = count.unwrap_or(1);
        let line = self.cursor.line;
        match command {
            c if c.is_motion() => self.move_cursor(c, count, arg),
            InsertBefore => self.mode = Mode::Insert,
            Append => {
                if self.buf.line_len(line) > 0 {
                    self.cursor.col += 1;
                }
                self.mode = Mode::Insert;
            }
            InsertAtStart => {
                self.cursor.col = editmotion::first_non_blank(&self.buf, line);
                if self.buf.line(line).trim().is_empty() {
                    self.cursor.col = 0;
                }
                self.mode = Mode::Insert;
            }
            AppendAtEnd => {
                self.cursor.col = self.buf.line_len(line);
                self.mode = Mode::Insert;
            }
            OpenBelow => self.open_line(true),
            OpenAbove => self.open_line(false),
            DeleteChar => self.operate(EditOp::Delete, Some((Right, None)), count),
            DeleteCharBack => self.operate(EditOp::Delete, Some((Left, None)), count),
            DeleteToEnd => self.operate(EditOp::Delete, Some((LineEnd, None)), count),
            ChangeToEnd => self.operate(EditOp::Change, Some((LineEnd, None)), count),
            SubstituteChar => self.operate(EditOp::Change, Some((Right, None)), count),
            SubstituteLine => self.operate(EditOp::Change, None, count),
            ReplaceChar => self.replace_chars(arg?, n),
            Join => self.join_lines(n.max(2)),
            ToggleCase => self.toggle_case(n),
            PasteAfter => self.put(true, n),
            PasteBefore => self.put(false, n),
            Undo => match self.buf.undo() {
                Some(cursor) => self.cursor = cursor,
                None => self.message = Some("nothing to undo".into()),
            },
            Redo => match self.buf.redo() {
                Some(cursor) => self.cursor = cursor,
                None => self.message = Some("nothing to redo".into()),
            },
            SearchForward => self.open_prompt(true),
            SearchBackward => self.open_prompt(false),
            ExPrompt => {
                self.mode = Mode::Prompt(Prompt {
                    kind: PromptKind::Ex,
                    editor: LineEditor::default(),
                    origin: None,
                    subject: None,
                });
            }
            WriteQuit => {
                return Some(EditEvent::Save {
                    force: false,
                    then_close: true,
                });
            }
            QuitDiscard => return Some(EditEvent::Close),
            _ => {}
        }
        None
    }

    fn note_find(&mut self, command: EditCommand, arg: Option<char>) {
        use EditCommand::*;
        let Some(ch) = arg else { return };
        let (forward, till) = match command {
            FindChar => (true, false),
            FindCharBack => (false, false),
            TillChar => (true, true),
            TillCharBack => (false, true),
            _ => return,
        };
        self.last_find = Some(Find { ch, forward, till });
    }

    /// Where a motion leads, without moving. `Err` carries the message to show, empty when there is nothing to say.
    fn motion_target(
        &self,
        command: EditCommand,
        count: Option<usize>,
        arg: Option<char>,
    ) -> Result<Target, String> {
        use EditCommand::*;
        let n = count.unwrap_or(1);
        let (buf, c) = (&self.buf, self.cursor);
        let last = buf.line_count() - 1;
        let silent = || Err(String::new());
        let target = |pos, kind| Ok(Target { pos, kind });
        let repeat = |from: Pos, step: &dyn Fn(Pos) -> Pos| (0..n).fold(from, |p, _| step(p));
        let vertical = |delta: isize| -> Result<Target, String> {
            let line = c.line.saturating_add_signed(delta).min(last);
            if line == c.line {
                return silent();
            }
            let col = self.want_col.min(buf.line_len(line).saturating_sub(1));
            target(Pos::new(line, col), Kind::Linewise)
        };
        let screen = isize::try_from(self.rows).unwrap_or(1);
        let half = (screen / 2).max(1);
        let page = (screen - 2).max(1);
        let step = isize::try_from(n).unwrap_or(isize::MAX);
        match command {
            Left => {
                if c.col == 0 {
                    return silent();
                }
                target(Pos::new(c.line, c.col.saturating_sub(n)), Kind::Exclusive)
            }
            Right => {
                let len = buf.line_len(c.line);
                if c.col >= len {
                    return silent();
                }
                target(Pos::new(c.line, (c.col + n).min(len)), Kind::Exclusive)
            }
            Up => vertical(-step),
            Down => vertical(step),
            HalfUp => vertical(-half.saturating_mul(step)),
            HalfDown => vertical(half.saturating_mul(step)),
            PageUp => vertical(-page.saturating_mul(step)),
            PageDown => vertical(page.saturating_mul(step)),
            LineStart => target(Pos::new(c.line, 0), Kind::Exclusive),
            FirstNonBlank => target(
                Pos::new(c.line, editmotion::first_non_blank(buf, c.line)),
                Kind::Exclusive,
            ),
            LineEnd => {
                let line = (c.line + n - 1).min(last);
                target(
                    Pos::new(line, buf.line_len(line).saturating_sub(1)),
                    Kind::Inclusive,
                )
            }
            WordFwd | BigWordFwd => target(
                repeat(c, &|p| {
                    editmotion::word_forward(buf, p, command == BigWordFwd)
                }),
                Kind::Exclusive,
            ),
            WordBack | BigWordBack => target(
                repeat(c, &|p| {
                    editmotion::word_back(buf, p, command == BigWordBack)
                }),
                Kind::Exclusive,
            ),
            WordEnd | BigWordEnd => target(
                repeat(c, &|p| editmotion::word_end(buf, p, command == BigWordEnd)),
                Kind::Inclusive,
            ),
            FirstLine | LastLine => {
                let line = match (command, count) {
                    (_, Some(count)) => count.saturating_sub(1).min(last),
                    (FirstLine, None) => 0,
                    _ => last,
                };
                target(
                    Pos::new(line, editmotion::first_non_blank(buf, line)),
                    Kind::Linewise,
                )
            }
            FindChar | FindCharBack | TillChar | TillCharBack | RepeatFind | RepeatFindBack => {
                let (find, is_repeat) = match (command, arg) {
                    (RepeatFind, _) => (self.last_find, true),
                    (RepeatFindBack, _) => (
                        self.last_find.map(|f| Find {
                            forward: !f.forward,
                            ..f
                        }),
                        true,
                    ),
                    (_, Some(ch)) => (
                        Some(Find {
                            ch,
                            forward: matches!(command, FindChar | TillChar),
                            till: matches!(command, TillChar | TillCharBack),
                        }),
                        false,
                    ),
                    _ => (None, false),
                };
                let Some(find) = find else {
                    return Err("no character to repeat".into());
                };
                match editmotion::find_char(buf, c, find, n, is_repeat) {
                    Some(pos) => target(
                        pos,
                        if find.forward {
                            Kind::Inclusive
                        } else {
                            Kind::Exclusive
                        },
                    ),
                    None => silent(),
                }
            }
            ParaFwd => target(
                repeat(c, &|p| editmotion::paragraph_forward(buf, p)),
                Kind::Exclusive,
            ),
            ParaBack => target(
                repeat(c, &|p| editmotion::paragraph_back(buf, p)),
                Kind::Exclusive,
            ),
            SearchNext | SearchPrev => {
                let Some(query) = &self.last_search else {
                    return Err("no previous search".into());
                };
                let forward = self.search_forward == (command == SearchNext);
                let mut at = c;
                for _ in 0..n {
                    at = editmotion::search(buf, query, at, forward)
                        .ok_or_else(|| format!("pattern not found: {query}"))?;
                }
                target(at, Kind::Exclusive)
            }
            _ => Err("not a motion".into()),
        }
    }

    fn move_cursor(&mut self, command: EditCommand, count: Option<usize>, arg: Option<char>) {
        use EditCommand::*;
        self.note_find(command, arg);
        match self.motion_target(command, count, arg) {
            Ok(target) => {
                self.cursor = self.buf.clamp(target.pos, false);
                let vertical = matches!(command, Up | Down | HalfUp | HalfDown | PageUp | PageDown);
                if command == LineEnd {
                    self.want_col = usize::MAX;
                } else if !vertical {
                    self.want_col = self.cursor.col;
                }
            }
            Err(message) if !message.is_empty() => self.message = Some(message),
            Err(_) => {}
        }
    }

    fn operate(
        &mut self,
        operator: EditOp,
        motion: Option<(EditCommand, Option<char>)>,
        count: Option<usize>,
    ) {
        use EditCommand::*;
        let cursor = self.cursor;
        let range = match motion {
            None => {
                let last = self.buf.line_count() - 1;
                let end = (cursor.line + count.unwrap_or(1).max(1) - 1).min(last);
                Range {
                    start: Pos::new(cursor.line, 0),
                    end: Pos::new(end, self.buf.line_len(end)),
                    linewise: true,
                }
            }
            Some((command, arg)) => {
                self.note_find(command, arg);
                let word = matches!(command, WordFwd | BigWordFwd);
                let on_word = self.buf.line_len(cursor.line) > 0 && !is_blank_at(&self.buf, cursor);
                let target = if operator == EditOp::Change && word && on_word {
                    let big = command == BigWordFwd;
                    let mut end = editmotion::end_of_current_word(&self.buf, cursor, big);
                    for _ in 1..count.unwrap_or(1) {
                        end = editmotion::word_end(&self.buf, end, big);
                    }
                    Target {
                        pos: end,
                        kind: Kind::Inclusive,
                    }
                } else {
                    match self.motion_target(command, count, arg) {
                        Ok(target) => target,
                        Err(message) => {
                            if !message.is_empty() {
                                self.message = Some(message);
                            }
                            return;
                        }
                    }
                };
                editmotion::range_for(&self.buf, cursor, target, word)
            }
        };
        self.apply_operator(operator, range);
    }

    fn range_text(&self, range: Range) -> String {
        let text = self.buf.text_between(range.start, range.end);
        if range.linewise {
            format!("{text}\n")
        } else {
            text
        }
    }

    fn apply_operator(&mut self, operator: EditOp, range: Range) {
        let text = self.range_text(range);
        let lines = text.matches('\n').count() + usize::from(!range.linewise);
        match operator {
            EditOp::Yank => {
                self.register = Some(Register {
                    text,
                    linewise: range.linewise,
                });
                if range.linewise {
                    self.cursor.line = self.cursor.line.min(range.start.line);
                } else {
                    self.cursor = range.start;
                }
                if lines >= 3 {
                    self.message = Some(format!("{lines} lines yanked"));
                }
            }
            EditOp::Delete => {
                self.register = Some(Register {
                    text,
                    linewise: range.linewise,
                });
                self.delete(range);
                if lines >= 3 {
                    self.message = Some(format!("{lines} lines deleted"));
                }
            }
            EditOp::Change => {
                self.register = Some(Register {
                    text,
                    linewise: range.linewise,
                });
                self.buf.replace(range.start, range.end, "");
                self.cursor = range.start;
                self.mode = Mode::Insert;
            }
        }
    }

    fn delete(&mut self, range: Range) {
        if !range.linewise {
            self.buf.replace(range.start, range.end, "");
            self.cursor = self.buf.clamp(range.start, false);
            return;
        }
        let (first, last) = (range.start.line, range.end.line);
        let count = self.buf.line_count();
        if first == 0 && last + 1 >= count {
            self.buf.replace(Pos::new(0, 0), range.end, "");
            self.buf.mark_emptied();
        } else if last + 1 < count {
            self.buf
                .replace(Pos::new(first, 0), Pos::new(last + 1, 0), "");
        } else {
            let above = first - 1;
            self.buf.replace(
                Pos::new(above, self.buf.line_len(above)),
                Pos::new(last, self.buf.line_len(last)),
                "",
            );
        }
        let line = first.min(self.buf.line_count() - 1);
        self.cursor = Pos::new(line, editmotion::first_non_blank(&self.buf, line));
        self.want_col = self.cursor.col;
    }

    fn put(&mut self, after: bool, times: usize) {
        let Some(register) = self.register.clone() else {
            self.message = Some("nothing to paste".into());
            return;
        };
        let line = self.cursor.line;
        if register.linewise {
            let body = register.text.trim_end_matches('\n');
            let block = vec![body; times].join("\n");
            let (at, text, first) = if after {
                let at = Pos::new(line, self.buf.line_len(line));
                (at, format!("\n{block}"), line + 1)
            } else {
                (Pos::new(line, 0), format!("{block}\n"), line)
            };
            self.buf.replace(at, at, &text);
            self.cursor = Pos::new(first, editmotion::first_non_blank(&self.buf, first));
            return;
        }
        let text = register.text.repeat(times);
        let at = if after && self.buf.line_len(line) > 0 {
            Pos::new(line, self.cursor.col + 1)
        } else {
            self.cursor
        };
        let end = self.buf.replace(at, at, &text);
        self.cursor = if text.contains('\n') {
            at
        } else {
            Pos::new(end.line, end.col.saturating_sub(1))
        };
    }

    fn open_line(&mut self, below: bool) {
        let line = self.cursor.line;
        let text = self.buf.line(line);
        let indent: String = text.chars().take_while(|c| c.is_whitespace()).collect();
        let indent_cols = grapheme_len(&indent);
        if below {
            let at = Pos::new(line, self.buf.line_len(line));
            self.buf.replace(at, at, &format!("\n{indent}"));
            self.cursor = Pos::new(line + 1, indent_cols);
        } else {
            let at = Pos::new(line, 0);
            self.buf.replace(at, at, &format!("{indent}\n"));
            self.cursor = Pos::new(line, indent_cols);
        }
        self.mode = Mode::Insert;
    }

    fn replace_chars(&mut self, ch: char, n: usize) {
        let c = self.cursor;
        if c.col + n > self.buf.line_len(c.line) {
            return;
        }
        let end = Pos::new(c.line, c.col + n);
        self.buf.replace(c, end, &ch.to_string().repeat(n));
        self.cursor = Pos::new(c.line, c.col + n - 1);
    }

    fn join_lines(&mut self, count: usize) {
        let line = self.cursor.line;
        for _ in 1..count {
            if line + 1 >= self.buf.line_count() {
                break;
            }
            let current = self.buf.line(line).to_string();
            let next = self.buf.line(line + 1);
            let trimmed = next.trim_start();
            let skipped = grapheme_len(&next[..next.len() - trimmed.len()]);
            let glue = if current.is_empty()
                || trimmed.is_empty()
                || current.ends_with(char::is_whitespace)
            {
                ""
            } else {
                " "
            };
            let len = grapheme_len(&current);
            self.buf
                .replace(Pos::new(line, len), Pos::new(line + 1, skipped), glue);
            self.cursor = Pos::new(line, len);
        }
    }

    fn toggle_case(&mut self, n: usize) {
        let c = self.cursor;
        let len = self.buf.line_len(c.line);
        if len == 0 {
            return;
        }
        let end = Pos::new(c.line, (c.col + n).min(len));
        let swapped: String = self
            .buf
            .text_between(c, end)
            .chars()
            .map(|ch| {
                if ch.is_lowercase() {
                    ch.to_uppercase().collect::<String>()
                } else {
                    ch.to_lowercase().collect::<String>()
                }
            })
            .collect();
        self.buf.replace(c, end, &swapped);
        self.cursor = Pos::new(c.line, end.col);
    }

    fn open_prompt(&mut self, forward: bool) {
        self.search_forward = forward;
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Search,
            editor: LineEditor::default(),
            origin: None,
            subject: None,
        });
    }

    fn press_prompt(&mut self, key: Key) -> Option<EditEvent> {
        let Mode::Prompt(mut prompt) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return None;
        };
        match prompt.handle(key) {
            PromptEvent::Idle | PromptEvent::Edited => {
                self.mode = Mode::Prompt(prompt);
                None
            }
            PromptEvent::Cancel => None,
            PromptEvent::Confirm => match prompt.kind {
                PromptKind::Search => {
                    self.confirm_search(prompt.editor.text());
                    None
                }
                _ => self.confirm_ex(prompt.editor.text()),
            },
        }
    }

    fn confirm_search(&mut self, query: &str) {
        if !query.is_empty() {
            self.last_search = Some(query.to_string());
        }
        // `n` follows the direction the search was typed in, so the first jump is just `n`.
        self.move_cursor(EditCommand::SearchNext, None, None);
    }

    fn confirm_ex(&mut self, text: &str) -> Option<EditEvent> {
        match parse_ex(text) {
            Ok(EditEx::Write { force }) => Some(EditEvent::Save {
                force,
                then_close: false,
            }),
            Ok(EditEx::WriteQuit { force }) => Some(EditEvent::Save {
                force,
                then_close: true,
            }),
            Ok(EditEx::Quit { force }) => {
                if self.dirty() && !force {
                    self.message = Some("unsaved changes (:w saves, :q! discards)".into());
                    None
                } else {
                    Some(EditEvent::Close)
                }
            }
            Ok(EditEx::Reload) => Some(EditEvent::Reload),
            Ok(EditEx::Goto(line)) => {
                let line = line.saturating_sub(1).min(self.buf.line_count() - 1);
                self.cursor = Pos::new(line, editmotion::first_non_blank(&self.buf, line));
                self.want_col = self.cursor.col;
                None
            }
            Err(message) => {
                if !message.is_empty() {
                    self.message = Some(message);
                }
                None
            }
        }
    }

    fn press_insert(&mut self, key: Key) {
        let c = self.cursor;
        let ctrl = key.ctrl;
        match (key.code, ctrl) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => self.leave_insert(),
            (KeyCode::Enter, _) => {
                let line = self.buf.line(c.line);
                let before = &line[..byte_of_col(line, c.col)];
                let indent: String = before.chars().take_while(|ch| ch.is_whitespace()).collect();
                self.insert(&format!("\n{indent}"));
            }
            (KeyCode::Tab, _) => self.insert(self.tab_text),
            (KeyCode::Backspace, _) | (KeyCode::Char('h'), true) => self.backspace(),
            (KeyCode::Delete, _) => {
                let len = self.buf.line_len(c.line);
                if c.col < len {
                    self.buf.replace(c, Pos::new(c.line, c.col + 1), "");
                } else if c.line + 1 < self.buf.line_count() {
                    self.buf.replace(c, Pos::new(c.line + 1, 0), "");
                }
            }
            (KeyCode::Char('w'), true) => {
                if c.col == 0 {
                    self.backspace();
                } else {
                    let start = editmotion::word_back(&self.buf, c, false);
                    let start = if start.line == c.line {
                        start
                    } else {
                        Pos::new(c.line, 0)
                    };
                    self.buf.replace(start, c, "");
                    self.cursor = start;
                }
            }
            (KeyCode::Char('u'), true) => {
                self.buf.replace(Pos::new(c.line, 0), c, "");
                self.cursor = Pos::new(c.line, 0);
            }
            (KeyCode::Left, _) => self.cursor.col = c.col.saturating_sub(1),
            (KeyCode::Right, _) => self.cursor.col = (c.col + 1).min(self.buf.line_len(c.line)),
            (KeyCode::Home, _) => self.cursor.col = 0,
            (KeyCode::End, _) => self.cursor.col = self.buf.line_len(c.line),
            (KeyCode::Up | KeyCode::Down, _) => {
                let line = if key.code == KeyCode::Up {
                    c.line.saturating_sub(1)
                } else {
                    (c.line + 1).min(self.buf.line_count() - 1)
                };
                self.cursor = Pos::new(line, self.want_col.min(self.buf.line_len(line)));
            }
            _ => {
                if let Some(ch) = key.typed_char() {
                    self.insert(&ch.to_string());
                }
            }
        }
        if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            self.want_col = self.cursor.col;
        }
    }

    fn insert(&mut self, text: &str) {
        self.cursor = self.buf.replace(self.cursor, self.cursor, text);
    }

    fn backspace(&mut self) {
        let c = self.cursor;
        if c.col > 0 {
            let start = Pos::new(c.line, c.col - 1);
            self.buf.replace(start, c, "");
            self.cursor = start;
        } else if c.line > 0 {
            let above = Pos::new(c.line - 1, self.buf.line_len(c.line - 1));
            self.buf.replace(above, c, "");
            self.cursor = above;
        }
    }

    fn leave_insert(&mut self) {
        self.buf.end_group();
        self.mode = Mode::Normal;
        self.cursor.col = self.cursor.col.saturating_sub(1);
        self.cursor = self.buf.clamp(self.cursor, false);
        self.want_col = self.cursor.col;
    }
}

fn is_blank_at(buf: &Buffer, pos: Pos) -> bool {
    let line = buf.line(pos.line);
    line[byte_of_col(line, pos.col)..]
        .chars()
        .next()
        .is_none_or(char::is_whitespace)
}

/// Terminal column of a grapheme column, with tabs expanded to the next tab stop.
pub fn display_col(line: &str, col: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let mut width = 0;
    for g in line.graphemes(true).take(col) {
        width += if g == "\t" {
            TAB_WIDTH - width % TAB_WIDTH
        } else {
            g.width().max(usize::from(!g.is_empty() && g.width() == 0))
        };
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::open(PathBuf::from("t.txt"), text.as_bytes(), (0, None)).unwrap()
    }

    fn keys(e: &mut Editor, sequence: &str) -> Vec<EditEvent> {
        Key::parse_seq(sequence)
            .unwrap()
            .into_iter()
            .filter_map(|key| e.press(key))
            .collect()
    }

    fn text(e: &Editor) -> String {
        String::from_utf8(e.bytes()).unwrap()
    }

    fn at(e: &Editor) -> (usize, usize) {
        (e.cursor().line, e.cursor().col)
    }

    #[test]
    fn hjkl_move_and_stay_on_characters() {
        let mut e = ed("abc\n\nlonger line\n");
        keys(&mut e, "lll");
        assert_eq!(at(&e), (0, 2), "stops on the last character");
        keys(&mut e, "j");
        assert_eq!(at(&e), (1, 0), "an empty line");
        keys(&mut e, "j");
        assert_eq!(
            at(&e),
            (2, 2),
            "the column is remembered across the empty line"
        );
        keys(&mut e, "05l");
        assert_eq!(at(&e), (2, 5));
        keys(&mut e, "kk");
        assert_eq!(at(&e), (0, 2));
        keys(&mut e, "hk");
        assert_eq!(at(&e), (0, 1), "k on the first line does not move");
    }

    #[test]
    fn j_and_k_remember_the_column_across_short_lines() {
        let mut e = ed("long line here\nab\nanother long one\n");
        keys(&mut e, "8l");
        keys(&mut e, "j");
        assert_eq!(at(&e), (1, 1));
        keys(&mut e, "j");
        assert_eq!(at(&e), (2, 8));
        keys(&mut e, "$k");
        assert_eq!(at(&e), (1, 1));
        keys(&mut e, "j");
        assert_eq!(at(&e), (2, 15), "after $ the cursor sticks to the line end");
    }

    #[test]
    fn line_and_word_motions() {
        let mut e = ed("  foo.bar baz\nsecond\n");
        keys(&mut e, "$");
        assert_eq!(at(&e), (0, 12));
        keys(&mut e, "0");
        assert_eq!(at(&e), (0, 0));
        keys(&mut e, "^");
        assert_eq!(at(&e), (0, 2));
        keys(&mut e, "w");
        assert_eq!(at(&e), (0, 5));
        keys(&mut e, "2w");
        assert_eq!(at(&e), (0, 10));
        keys(&mut e, "b");
        assert_eq!(at(&e), (0, 6));
        keys(&mut e, "e");
        assert_eq!(at(&e), (0, 8));
        keys(&mut e, "W");
        assert_eq!(at(&e), (0, 10));
        keys(&mut e, "B");
        assert_eq!(at(&e), (0, 2));
    }

    #[test]
    fn gg_g_and_numbered_lines() {
        let mut e = ed("one\n  two\nthree\nfour\n");
        keys(&mut e, "G");
        assert_eq!(at(&e), (3, 0));
        keys(&mut e, "gg");
        assert_eq!(at(&e), (0, 0));
        keys(&mut e, "2G");
        assert_eq!(at(&e), (1, 2), "lands on the first non-blank");
        keys(&mut e, "3gg");
        assert_eq!(at(&e), (2, 0));
        keys(&mut e, "99G");
        assert_eq!(at(&e), (3, 0));
    }

    #[test]
    fn f_t_semicolon_and_comma() {
        let mut e = ed("a,b,c,d\n");
        keys(&mut e, "f,");
        assert_eq!(at(&e), (0, 1));
        keys(&mut e, ";");
        assert_eq!(at(&e), (0, 3));
        keys(&mut e, ",");
        assert_eq!(at(&e), (0, 1));
        keys(&mut e, "0t,");
        assert_eq!(
            at(&e),
            (0, 0),
            "till stops before the comma, which is where we are"
        );
        keys(&mut e, "2f,");
        assert_eq!(at(&e), (0, 3));
        keys(&mut e, "F,");
        assert_eq!(at(&e), (0, 1));
        keys(&mut e, "fz");
        assert_eq!(at(&e), (0, 1), "a miss does not move");
    }

    #[test]
    fn insert_typing_and_leaving_step_back_one_column() {
        let mut e = ed("hello\n");
        keys(&mut e, "ixy<esc>");
        assert_eq!(text(&e), "xyhello\n");
        assert_eq!(at(&e), (0, 1));
        keys(&mut e, "Aend<esc>");
        assert_eq!(text(&e), "xyhelloend\n");
        assert_eq!(at(&e), (0, 9));
        keys(&mut e, "0aZ<esc>");
        assert_eq!(text(&e), "xZyhelloend\n");
        keys(&mut e, "Iq<esc>");
        assert_eq!(text(&e), "qxZyhelloend\n");
    }

    #[test]
    fn o_and_capital_o_open_lines_and_copy_the_indent() {
        let mut e = ed("    indented\nplain\n");
        keys(&mut e, "onext<esc>");
        assert_eq!(text(&e), "    indented\n    next\nplain\n");
        assert_eq!(at(&e), (1, 7));
        keys(&mut e, "Oabove<esc>");
        assert_eq!(text(&e), "    indented\n    above\n    next\nplain\n");
        keys(&mut e, "Gonew<esc>");
        assert_eq!(text(&e), "    indented\n    above\n    next\nplain\nnew\n");
    }

    #[test]
    fn enter_backspace_delete_and_word_kill_edit_while_inserting() {
        let mut e = ed("ab\ncd\n");
        keys(&mut e, "a<cr>X<esc>");
        assert_eq!(text(&e), "a\nXb\ncd\n");
        keys(&mut e, "0i<bs><esc>");
        assert_eq!(text(&e), "aXb\ncd\n", "backspace at column 0 joins lines");
        keys(&mut e, "0i<del><esc>");
        assert_eq!(text(&e), "Xb\ncd\n");
        let mut e = ed("one two three\n");
        keys(&mut e, "A<c-w><c-w><esc>");
        assert_eq!(text(&e), "one \n");
        let mut e = ed("some text\n");
        keys(&mut e, "wi<c-u><esc>");
        assert_eq!(text(&e), "text\n");
    }

    #[test]
    fn tab_uses_the_files_own_indent_style() {
        let mut spaces = ed("  two\n  spaces\n");
        keys(&mut spaces, "i<tab><esc>");
        assert!(text(&spaces).starts_with("    "), "{:?}", text(&spaces));
        let mut tabs = ed("\tone\n\ttwo\n");
        keys(&mut tabs, "i<tab><esc>");
        assert!(text(&tabs).starts_with("\t\tone"), "{:?}", text(&tabs));
    }

    #[test]
    fn x_and_capital_x_delete_characters_with_counts() {
        let mut e = ed("abcdef\n");
        keys(&mut e, "x");
        assert_eq!(text(&e), "bcdef\n");
        keys(&mut e, "2x");
        assert_eq!(text(&e), "def\n");
        keys(&mut e, "$X");
        assert_eq!(text(&e), "df\n");
        keys(&mut e, "9x");
        assert_eq!(text(&e), "d\n", "a count past the end takes what is there");
        keys(&mut e, "9x");
        assert_eq!(text(&e), "\n");
        keys(&mut e, "x");
        assert_eq!(text(&e), "\n");
    }

    #[test]
    fn dw_deletes_words_without_joining_lines() {
        let mut e = ed("foo bar\nbaz\n");
        keys(&mut e, "dw");
        assert_eq!(text(&e), "bar\nbaz\n");
        keys(&mut e, "dw");
        assert_eq!(
            text(&e),
            "\nbaz\n",
            "the last word of a line leaves the line break"
        );
        let mut e = ed("a b c d e\n");
        keys(&mut e, "d3w");
        assert_eq!(text(&e), "d e\n");
        keys(&mut e, "$db");
        assert_eq!(text(&e), "e\n");
    }

    #[test]
    fn dd_deletes_lines_with_counts_and_keeps_the_cursor_valid() {
        let mut e = ed("1\n2\n3\n4\n5\n");
        keys(&mut e, "dd");
        assert_eq!(text(&e), "2\n3\n4\n5\n");
        keys(&mut e, "2dd");
        assert_eq!(text(&e), "4\n5\n");
        keys(&mut e, "Gdd");
        assert_eq!(text(&e), "4\n");
        assert_eq!(at(&e), (0, 0));
        keys(&mut e, "dd");
        assert_eq!(text(&e), "", "the last line goes and the file is empty");
        keys(&mut e, "dd");
        assert_eq!(text(&e), "");
    }

    #[test]
    fn operators_with_vertical_and_file_motions_are_linewise() {
        let mut e = ed("1\n2\n3\n4\n5\n");
        keys(&mut e, "jdj");
        assert_eq!(text(&e), "1\n4\n5\n");
        keys(&mut e, "dk");
        assert_eq!(text(&e), "5\n");
        let mut e = ed("1\n2\n3\n4\n5\n");
        keys(&mut e, "2jdG");
        assert_eq!(text(&e), "1\n2\n");
        let mut e = ed("1\n2\n3\n4\n5\n");
        keys(&mut e, "2jdgg");
        assert_eq!(text(&e), "4\n5\n");
        let mut e = ed("1\n2\n3\n4\n5\n");
        keys(&mut e, "d2j");
        assert_eq!(text(&e), "4\n5\n");
    }

    #[test]
    fn d_with_end_of_line_and_find_motions() {
        let mut e = ed("hello, world\n");
        keys(&mut e, "wD");
        assert_eq!(text(&e), "hello\n");
        let mut e = ed("a,b,c\n");
        keys(&mut e, "df,");
        assert_eq!(text(&e), "b,c\n");
        keys(&mut e, "dt,");
        assert_eq!(text(&e), ",c\n");
        let mut e = ed("one two three\n");
        keys(&mut e, "d$");
        assert_eq!(text(&e), "\n");
    }

    #[test]
    fn cw_keeps_the_space_and_cc_changes_the_line() {
        let mut e = ed("foo bar baz\n");
        keys(&mut e, "cwX<esc>");
        assert_eq!(text(&e), "X bar baz\n");
        keys(&mut e, "wc2wY<esc>");
        assert_eq!(text(&e), "X Y\n");
        let mut e = ed("first\nsecond\nthird\n");
        keys(&mut e, "jccnew<esc>");
        assert_eq!(text(&e), "first\nnew\nthird\n");
        keys(&mut e, "ggcjdone<esc>");
        assert_eq!(text(&e), "done\nthird\n");
    }

    #[test]
    fn capital_c_capital_s_and_s_substitute() {
        let mut e = ed("hello world\n");
        keys(&mut e, "wCthere<esc>");
        assert_eq!(text(&e), "hello there\n");
        keys(&mut e, "0sJ<esc>");
        assert_eq!(text(&e), "Jello there\n");
        keys(&mut e, "Sall new<esc>");
        assert_eq!(text(&e), "all new\n");
    }

    #[test]
    fn yank_and_put_work_for_characters_and_lines() {
        let mut e = ed("alpha\nbeta\n");
        keys(&mut e, "yyp");
        assert_eq!(text(&e), "alpha\nalpha\nbeta\n");
        assert_eq!(at(&e), (1, 0));
        keys(&mut e, "jP");
        assert_eq!(text(&e), "alpha\nalpha\nalpha\nbeta\n");
        keys(&mut e, "Gyw");
        keys(&mut e, "$p");
        assert_eq!(text(&e), "alpha\nalpha\nalpha\nbetabeta\n");
        keys(&mut e, "3p");
        assert!(
            text(&e).ends_with("betabetabetabetabeta\n"),
            "{:?}",
            text(&e)
        );
        keys(&mut e, "ggy2jGp");
        assert!(
            text(&e).ends_with("betabetabetabetabeta\nalpha\nalpha\nalpha\n"),
            "{:?}",
            text(&e)
        );
    }

    #[test]
    fn deleted_text_can_be_put_back_elsewhere() {
        let mut e = ed("one\ntwo\nthree\n");
        keys(&mut e, "ddjp");
        assert_eq!(text(&e), "two\nthree\none\n");
        keys(&mut e, "ggdwP");
        assert_eq!(text(&e), "two\nthree\none\n");
        let mut e = ed("no register\n");
        keys(&mut e, "p");
        assert_eq!(e.message.as_deref(), Some("nothing to paste"));
    }

    #[test]
    fn r_j_and_tilde() {
        let mut e = ed("hello\n");
        keys(&mut e, "rH");
        assert_eq!(text(&e), "Hello\n");
        keys(&mut e, "l3rx");
        assert_eq!(text(&e), "Hxxxo\n");
        keys(&mut e, "9rz");
        assert_eq!(
            text(&e),
            "Hxxxo\n",
            "too few characters left, nothing changes"
        );
        let mut e = ed("one\n   two\nthree\n");
        keys(&mut e, "J");
        assert_eq!(text(&e), "one two\nthree\n");
        keys(&mut e, "J");
        assert_eq!(text(&e), "one two three\n");
        let mut e = ed("aBc dE\n");
        keys(&mut e, "~");
        assert_eq!(text(&e), "ABc dE\n");
        keys(&mut e, "0$~");
        assert_eq!(text(&e), "ABc de\n");
        keys(&mut e, "0~");
        assert_eq!(text(&e), "aBc de\n");
    }

    #[test]
    fn one_undo_reverts_a_whole_insert_or_a_whole_change() {
        let mut e = ed("hello world\n");
        keys(&mut e, "Aone two three<esc>");
        keys(&mut e, "u");
        assert_eq!(text(&e), "hello world\n");
        keys(&mut e, "cwbye<esc>");
        assert_eq!(text(&e), "bye world\n");
        keys(&mut e, "u");
        assert_eq!(text(&e), "hello world\n");
        assert_eq!(at(&e), (0, 0));
        keys(&mut e, "<c-r>");
        assert_eq!(text(&e), "bye world\n");
        keys(&mut e, "ddu");
        assert_eq!(text(&e), "bye world\n");
        keys(&mut e, "uu");
        assert_eq!(e.message.as_deref(), Some("nothing to undo"));
    }

    #[test]
    fn undo_and_redo_report_when_there_is_nothing_to_do() {
        let mut e = ed("x\n");
        keys(&mut e, "u");
        assert_eq!(e.message.as_deref(), Some("nothing to undo"));
        keys(&mut e, "<c-r>");
        assert_eq!(e.message.as_deref(), Some("nothing to redo"));
    }

    #[test]
    fn search_moves_repeats_and_reverses() {
        let mut e = ed("alpha\nbeta\ngamma beta\ndelta\n");
        keys(&mut e, "/beta<cr>");
        assert_eq!(at(&e), (1, 0));
        keys(&mut e, "n");
        assert_eq!(at(&e), (2, 6));
        keys(&mut e, "n");
        assert_eq!(at(&e), (1, 0), "wraps");
        keys(&mut e, "N");
        assert_eq!(at(&e), (2, 6));
        keys(&mut e, "?alpha<cr>");
        assert_eq!(at(&e), (0, 0), "backward search");
        keys(&mut e, "n");
        assert_eq!(
            at(&e),
            (0, 0),
            "n keeps going backwards after ?, and there is only one match"
        );
        keys(&mut e, "/zzz<cr>");
        assert_eq!(e.message.as_deref(), Some("pattern not found: zzz"));
    }

    #[test]
    fn search_can_be_an_operator_motion() {
        let mut e = ed("keep this and drop that\n");
        keys(&mut e, "/and<cr>");
        assert_eq!(at(&e), (0, 10));
        keys(&mut e, "0dn");
        assert_eq!(text(&e), "and drop that\n");
    }

    #[test]
    fn ex_commands_report_events_and_guard_unsaved_changes() {
        let mut e = ed("a\nb\nc\n");
        assert_eq!(
            keys(&mut e, ":w<cr>"),
            [EditEvent::Save {
                force: false,
                then_close: false
            }]
        );
        assert_eq!(
            keys(&mut e, ":w!<cr>"),
            [EditEvent::Save {
                force: true,
                then_close: false
            }]
        );
        assert_eq!(
            keys(&mut e, ":wq<cr>"),
            [EditEvent::Save {
                force: false,
                then_close: true
            }]
        );
        assert_eq!(
            keys(&mut e, "ZZ"),
            [EditEvent::Save {
                force: false,
                then_close: true
            }]
        );
        assert_eq!(
            keys(&mut e, ":q<cr>"),
            [EditEvent::Close],
            "clean buffers just close"
        );
        keys(&mut e, "x");
        assert!(keys(&mut e, ":q<cr>").is_empty());
        assert_eq!(
            e.message.as_deref(),
            Some("unsaved changes (:w saves, :q! discards)")
        );
        assert_eq!(keys(&mut e, ":q!<cr>"), [EditEvent::Close]);
        assert_eq!(keys(&mut e, "ZQ"), [EditEvent::Close]);
        assert_eq!(keys(&mut e, ":e!<cr>"), [EditEvent::Reload]);
        keys(&mut e, ":nonsense<cr>");
        assert_eq!(
            e.message.as_deref(),
            Some("not an editor command: nonsense")
        );
    }

    #[test]
    fn colon_number_jumps_to_a_line() {
        let mut e = ed("a\n  b\nc\n");
        keys(&mut e, ":2<cr>");
        assert_eq!(at(&e), (1, 2));
        keys(&mut e, ":99<cr>");
        assert_eq!(at(&e), (2, 0));
        keys(&mut e, ":$<cr>gg:1<cr>");
        assert_eq!(at(&e), (0, 0));
    }

    #[test]
    fn an_alt_chord_is_escape_followed_by_the_key() {
        let mut e = ed("x\n");
        keys(&mut e, "Aab");
        let events = keys(&mut e, "<a-:>wq<cr>");
        assert_eq!(
            events,
            [EditEvent::Save {
                force: false,
                then_close: true
            }]
        );
        assert_eq!(text(&e), "xab\n");
        assert!(!e.is_insert());
    }

    #[test]
    fn dirty_tracks_edits_and_saving() {
        let mut e = ed("x\n");
        assert!(!e.dirty());
        keys(&mut e, "ihi<esc>");
        assert!(e.dirty());
        e.mark_saved((3, None));
        assert!(!e.dirty());
        assert_eq!(e.disk_key(), (3, None));
        assert!(e.message.as_deref().unwrap().starts_with("written, 1 line"));
        keys(&mut e, "x");
        assert!(e.dirty());
    }

    #[test]
    fn escape_cancels_a_half_typed_command_and_a_prompt() {
        let mut e = ed("abc def\n");
        keys(&mut e, "d<esc>x");
        assert_eq!(
            text(&e),
            "bc def\n",
            "the d was cancelled, so x deleted a character"
        );
        keys(&mut e, ":w<esc>");
        assert!(e.prompt_view().is_none());
        keys(&mut e, "/x<esc>");
        assert!(e.prompt_view().is_none());
        keys(&mut e, "/");
        assert_eq!(e.prompt_view().unwrap().0, "/");
        keys(&mut e, "<esc>?");
        assert_eq!(e.prompt_view().unwrap().0, "?");
    }

    #[test]
    fn unicode_text_is_edited_by_grapheme() {
        let mut e = ed("zażółć 👍🏽 ok\n");
        keys(&mut e, "5lx");
        assert_eq!(text(&e), "zażół 👍🏽 ok\n");
        keys(&mut e, "fox");
        assert_eq!(text(&e), "zażół 👍🏽 k\n");
        let mut e = ed("zażółć 👍🏽 ok\n");
        keys(&mut e, "7lx");
        assert_eq!(
            text(&e),
            "zażółć  ok\n",
            "the emoji and its skin tone go together"
        );
        let mut e = ed("e\u{301}x\n");
        keys(&mut e, "x");
        assert_eq!(text(&e), "x\n", "the accent goes with its letter");
    }

    #[test]
    fn crlf_files_stay_crlf_after_editing() {
        let mut e = ed("one\r\ntwo\r\n");
        keys(&mut e, "ddoinserted<esc>");
        assert_eq!(text(&e), "two\r\ninserted\r\n");
    }

    #[test]
    fn the_view_follows_the_cursor() {
        let body: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut e = ed(&body);
        e.set_rows(10);
        keys(&mut e, "G");
        assert_eq!(e.top(), 90);
        keys(&mut e, "gg");
        assert_eq!(e.top(), 0);
        keys(&mut e, "15j");
        assert_eq!(e.top(), 6);
        keys(&mut e, "<c-d>");
        assert_eq!(at(&e).0, 20);
        keys(&mut e, "<c-u>");
        assert_eq!(at(&e).0, 15);
        keys(&mut e, "<c-f>");
        assert_eq!(at(&e).0, 23);
        keys(&mut e, "<c-b>");
        assert_eq!(at(&e).0, 15);
    }

    #[test]
    fn deleting_lines_pulls_the_view_back() {
        let body: String = (0..30).map(|i| format!("l{i}\n")).collect();
        let mut e = ed(&body);
        e.set_rows(5);
        keys(&mut e, "G");
        keys(&mut e, "25dk");
        assert!(e.top() < e.line_count());
        assert!(e.cursor().line < e.line_count());
    }

    #[test]
    fn display_columns_expand_tabs_and_count_wide_characters() {
        assert_eq!(display_col("\tx", 1), 4);
        assert_eq!(display_col("ab\tx", 3), 4);
        assert_eq!(display_col("a漢b", 2), 3);
        assert_eq!(display_col("abc", 9), 3);
    }

    #[test]
    fn styled_lines_use_syntax_colors_once_refreshed_and_plain_text_before() {
        let mut e = Editor::open("a.rs".into(), b"fn main() {}\n", (0, None)).unwrap();
        assert_eq!(
            e.styled_line(0).0.len(),
            1,
            "plain before the first refresh"
        );
        e.refresh_highlight();
        assert!(
            e.styled_line(0).0.len() > 1,
            "keywords and names split into spans"
        );
        keys(&mut e, "x");
        assert_eq!(
            e.styled_line(0).0.len(),
            1,
            "a changed line is plain until the refresh"
        );
        assert!(e.highlight_stale());
        e.refresh_highlight();
        assert!(e.styled_line(0).0.len() > 1);
    }

    /// Feeds thousands of random keys and checks that nothing panics and the cursor stays on the text.
    #[test]
    fn random_key_storms_never_panic_or_strand_the_cursor() {
        const ORIGINAL: &str = "first line\n\n  indented\tline ✓\nzażółć 👍🏽 gęślą\nlast";
        const KEYS: &[&str] = &[
            "h", "j", "k", "l", "w", "b", "e", "W", "B", "E", "0", "$", "^", "gg", "G", "x", "X",
            "dd", "dw", "d$", "D", "cw", "cc", "C", "s", "S", "yy", "yw", "p", "P", "u", "<c-r>",
            "J", "~", "r", "i", "a", "A", "I", "o", "O", "<esc>", "<cr>", "<bs>", "<del>", "<tab>",
            "x", "z", "é", "👍", " ", "f", "t", ";", ",", "}", "{", "n", "N", "/a<cr>", "?b<cr>",
            ":3<cr>", "2", "3", "d2j", "dk", "dG", "dgg", "<c-w>", "<c-u>", "<left>", "<right>",
            "<up>", "<down>", "<c-d>", "<c-u>", "<c-f>", "<c-b>",
        ];
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
        for seed in 1..=40 {
            let mut rng = Rng(seed);
            let mut e = ed(ORIGINAL);
            e.set_rows(6);
            for _ in 0..400 {
                let key = KEYS[rng.next(KEYS.len())];
                keys(&mut e, key);
                let c = e.cursor();
                assert!(c.line < e.line_count(), "seed {seed} after {key}");
                let len = grapheme_len(e.line_text(c.line));
                let limit = if e.is_insert() {
                    len
                } else {
                    len.saturating_sub(1)
                };
                assert!(
                    c.col <= limit,
                    "seed {seed} after {key}: {c:?} on a line of {len}"
                );
                assert!(
                    e.top() <= c.line && c.line < e.top() + 6,
                    "seed {seed} after {key}"
                );
                assert!(String::from_utf8(e.bytes()).is_ok());
            }
            keys(&mut e, "<esc>");
            let mut undone = 0;
            while e.buf.undo().is_some() {
                undone += 1;
                assert!(undone < 100_000);
            }
            assert_eq!(
                String::from_utf8(e.bytes()).unwrap(),
                ORIGINAL,
                "seed {seed}: undoing everything restores the file"
            );
        }
    }
}
