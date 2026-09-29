//! Interaction state on top of the [`Tree`]: modes, key handling and commands.

use std::path::{Path, PathBuf};

use ratatui::crossterm::event::KeyCode;

use crate::{
    excmd::{self, Ex},
    jumps::Jumps,
    keys::{Command, Fed, InputState, Key, Keymap},
    lineedit::LineEditor,
    marks::Marks,
    model::{Effect, Tree},
    motion,
    prompt::{Prompt, PromptEvent, PromptKind},
    search,
};

/// How the session ended. Only `Quit` should carry the shell to the last directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Quit,
    Abort,
}

/// What a key press asks the runtime to do besides redrawing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub effect: Option<Effect>,
    pub exit: Option<Exit>,
}

enum Mode {
    Normal,
    Prompt(Prompt),
    /// A read-only list drawn over the tree, such as the key reference.
    Overlay {
        title: String,
        lines: Vec<String>,
        scroll: usize,
    },
}

pub struct OverlayView<'a> {
    pub title: &'a str,
    pub lines: &'a [String],
    pub scroll: usize,
}

/// Startup choices that are not key bindings.
#[derive(Debug, Default)]
pub struct Settings {
    pub show_hidden: bool,
    pub marks: Marks,
}

pub struct PromptView<'a> {
    pub prefix: char,
    pub text: &'a str,
    pub cursor_col: usize,
}

/// A count this large means "as far as possible", so repeating a step further is pointless.
const MAX_REPEAT: usize = 1000;

pub struct App {
    tree: Tree,
    keymap: Keymap,
    input: InputState,
    mode: Mode,
    last_search: Option<String>,
    last_find: Option<(char, bool)>,
    marks: Marks,
    jumps: Jumps,
    /// Where the last jump started, for `''`.
    previous: Option<PathBuf>,
    /// Rows available to the tree, for page-sized motions.
    viewport: u16,
    home: Option<PathBuf>,
    /// One-line notice shown in the footer until the next key press.
    pub message: Option<String>,
}

impl App {
    pub fn new(root: PathBuf, keymap: Keymap) -> App {
        App::with_settings(root, keymap, Settings::default())
    }

    pub fn with_settings(root: PathBuf, keymap: Keymap, settings: Settings) -> App {
        App {
            tree: Tree::new(root, settings.show_hidden),
            keymap,
            input: InputState::default(),
            mode: Mode::Normal,
            last_search: None,
            last_find: None,
            marks: settings.marks,
            jumps: Jumps::default(),
            previous: None,
            viewport: 24,
            home: std::env::var_os("HOME").map(PathBuf::from),
            message: None,
        }
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn tree_mut(&mut self) -> &mut Tree {
        &mut self.tree
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn set_viewport(&mut self, rows: u16) {
        self.viewport = rows;
    }

    /// The half-typed normal-mode command, such as `12g`.
    pub fn pending(&self) -> String {
        self.input.display()
    }

    pub fn prompt_view(&self) -> Option<PromptView<'_>> {
        match &self.mode {
            Mode::Prompt(p) => Some(PromptView {
                prefix: p.prefix(),
                text: p.editor.text(),
                cursor_col: p.editor.cursor_col(),
            }),
            _ => None,
        }
    }

    pub fn overlay(&self) -> Option<OverlayView<'_>> {
        match &self.mode {
            Mode::Overlay {
                title,
                lines,
                scroll,
            } => Some(OverlayView {
                title,
                lines,
                scroll: *scroll,
            }),
            _ => None,
        }
    }

    /// Lists a directory load that finished. Also surfaces anything the tree wants to say, like a missing bookmark target.
    pub fn finish_load(&mut self, dir: &Path, result: std::io::Result<Vec<crate::model::Entry>>) {
        self.tree.finish_load(dir, result);
        self.pull_notice();
    }

    /// Finishes every pending listing on this thread. For tests.
    #[cfg(test)]
    pub fn settle(&mut self) {
        self.tree.settle();
        self.pull_notice();
    }

    fn pull_notice(&mut self) {
        if let Some(notice) = self.tree.take_notice() {
            self.message = Some(notice);
        }
    }

    pub fn press(&mut self, key: Key) -> Response {
        self.message = None;
        let response = match self.mode {
            Mode::Prompt(_) => self.press_prompt(key),
            Mode::Overlay { .. } => {
                self.press_overlay(key);
                Response::default()
            }
            Mode::Normal => match self.input.feed(key, &self.keymap) {
                Fed::Run {
                    command,
                    count,
                    arg,
                } => self.run(command, count, arg),
                Fed::Pending | Fed::Unbound => Response::default(),
            },
        };
        self.pull_notice();
        response
    }

    fn run(&mut self, command: Command, count: Option<usize>, arg: Option<char>) -> Response {
        let before = self.tree.location();
        let response = self.execute(command, count, arg);
        let jumps = matches!(
            command,
            Command::First
                | Command::Last
                | Command::Enter
                | Command::Leave
                | Command::SearchNext
                | Command::SearchPrev
        );
        if jumps && self.tree.location() != before {
            self.remember_jump(before);
        }
        response
    }

    /// Moves the focused cursor with a motion. Nothing found leaves the cursor and says why.
    fn motion(&mut self, command: Command, count: Option<usize>, arg: Option<char>) {
        if let (Command::Find | Command::FindBack, Some(letter)) = (command, arg) {
            self.last_find = Some((letter, command == Command::Find));
        }
        let level = self.tree.focused();
        let ctx = motion::Ctx {
            entries: &level.entries,
            cursor: level.cursor,
            viewport: self.viewport,
            last_search: self.last_search.as_deref(),
            last_find: self.last_find,
        };
        match motion::target(command, count, arg, &ctx) {
            Ok(index) => self.tree.set_cursor(index),
            Err(message) => self.message = Some(message),
        }
    }

    fn remember_jump(&mut self, from: PathBuf) {
        self.jumps.record(from.clone());
        self.previous = Some(from);
    }

    fn execute(&mut self, command: Command, count: Option<usize>, arg: Option<char>) -> Response {
        let n = count.unwrap_or(1);
        let step = isize::try_from(n).unwrap_or(isize::MAX);
        match command {
            Command::Down
            | Command::Up
            | Command::First
            | Command::Last
            | Command::HalfPageDown
            | Command::HalfPageUp
            | Command::PageDown
            | Command::PageUp
            | Command::SearchNext
            | Command::SearchPrev
            | Command::Find
            | Command::FindBack
            | Command::FindRepeat
            | Command::FindRepeatBack => self.motion(command, count, arg),
            Command::PreviewDown => self.tree.scroll_preview(step, usize::from(self.viewport)),
            Command::PreviewUp => self.tree.scroll_preview(-step, usize::from(self.viewport)),
            Command::Enter => {
                for _ in 0..n.min(MAX_REPEAT) {
                    let before = self.tree.focus();
                    if let Some(effect) = self.tree.enter() {
                        return Response {
                            effect: Some(effect),
                            exit: None,
                        };
                    }
                    if self.tree.focus() == before {
                        break;
                    }
                }
            }
            Command::Leave => (0..n.min(MAX_REPEAT)).for_each(|_| self.tree.leave()),
            Command::Search => self.open_prompt(PromptKind::Search),
            Command::ExPrompt => self.open_prompt(PromptKind::Ex),
            Command::Help => self.open_help(),
            Command::SetMark => {
                if let Some(name) = arg {
                    let here = self.tree.location();
                    if let Err(message) = self.marks.set(name, here) {
                        self.message = Some(message);
                    }
                }
            }
            Command::JumpMark => {
                if let Some(name) = arg {
                    self.jump_to_mark(name);
                }
            }
            Command::JumpBack => self.walk_jumps(n, false),
            Command::JumpForward => self.walk_jumps(n, true),
            Command::ToggleHidden => self.tree.set_show_hidden(!self.tree.show_hidden()),
            Command::Quit => {
                return Response {
                    effect: None,
                    exit: Some(Exit::Quit),
                };
            }
            Command::Abort => {
                return Response {
                    effect: None,
                    exit: Some(Exit::Abort),
                };
            }
        }
        Response::default()
    }

    fn open_help(&mut self) {
        let rows = self.keymap.describe();
        let key_width = rows
            .iter()
            .map(|(k, _)| unicode_width::UnicodeWidthStr::width(k.as_str()))
            .max()
            .unwrap_or(0);
        let lines = rows
            .iter()
            .map(|(keys, help)| {
                let pad =
                    " ".repeat(key_width - unicode_width::UnicodeWidthStr::width(keys.as_str()));
                format!("{keys}{pad}   {help}")
            })
            .collect();
        self.mode = Mode::Overlay {
            title: "keys".into(),
            lines,
            scroll: 0,
        };
    }

    fn open_marks(&mut self) {
        let mut lines: Vec<String> = self
            .marks
            .list()
            .into_iter()
            .map(|(name, path)| format!("{name}   {}", path.display()))
            .collect();
        if lines.is_empty() {
            lines.push("no marks yet. m{a-z} sets one, m{A-Z} saves a bookmark".into());
        }
        self.mode = Mode::Overlay {
            title: "marks".into(),
            lines,
            scroll: 0,
        };
    }

    fn jump_to_mark(&mut self, name: char) {
        let here = self.tree.location();
        let target = if name == '\'' || name == '`' {
            match self.previous.replace(here.clone()) {
                Some(previous) => previous,
                None => {
                    self.previous = None;
                    self.message = Some("no previous position".into());
                    return;
                }
            }
        } else {
            match self.marks.get(name) {
                Some(path) => path.to_path_buf(),
                None => {
                    self.message = Some(format!("mark {name} is not set"));
                    return;
                }
            }
        };
        if name != '\'' && name != '`' && target != here {
            self.remember_jump(here);
        }
        self.tree.reveal(&target);
    }

    fn walk_jumps(&mut self, times: usize, forward: bool) {
        let mut here = self.tree.location();
        let mut target = None;
        for _ in 0..times.min(MAX_REPEAT) {
            let next = if forward {
                self.jumps.forward()
            } else {
                self.jumps.back(&here)
            };
            match next {
                Some(path) => {
                    here = path.clone();
                    target = Some(path);
                }
                None => break,
            }
        }
        match target {
            Some(path) => self.tree.reveal(&path),
            None => {
                let edge = if forward { "newest" } else { "oldest" };
                self.message = Some(format!("already at the {edge} jump"));
            }
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        let origin = match kind {
            PromptKind::Search => self.tree.focused().selected().map(|e| e.name.clone()),
            PromptKind::Ex => None,
        };
        self.mode = Mode::Prompt(Prompt {
            kind,
            editor: LineEditor::default(),
            origin,
        });
    }

    fn press_prompt(&mut self, key: Key) -> Response {
        let Mode::Prompt(mut prompt) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return Response::default();
        };
        match prompt.handle(key) {
            PromptEvent::Idle => self.mode = Mode::Prompt(prompt),
            PromptEvent::Edited => {
                if prompt.kind == PromptKind::Search {
                    self.incremental_search(&prompt);
                }
                self.mode = Mode::Prompt(prompt);
            }
            PromptEvent::Cancel => {
                if prompt.kind == PromptKind::Search {
                    self.tree.set_cursor(self.search_origin(&prompt));
                }
            }
            PromptEvent::Confirm => {
                return match prompt.kind {
                    PromptKind::Search => {
                        let origin = self
                            .tree
                            .focused()
                            .dir
                            .join(prompt.origin.as_deref().unwrap_or_default());
                        self.confirm_search(prompt.editor.text(), origin);
                        Response::default()
                    }
                    PromptKind::Ex => self.run_ex(prompt.editor.text()),
                };
            }
        }
        Response::default()
    }

    /// Index of the entry the search started on, which incremental matching measures from.
    fn search_origin(&self, prompt: &Prompt) -> usize {
        let level = self.tree.focused();
        prompt
            .origin
            .as_ref()
            .and_then(|name| level.entries.iter().position(|e| &e.name == name))
            .unwrap_or(level.cursor)
    }

    fn incremental_search(&mut self, prompt: &Prompt) {
        let origin = self.search_origin(prompt);
        let entries = &self.tree.focused().entries;
        let target =
            search::find(entries, prompt.editor.text(), origin, true, true).unwrap_or(origin);
        self.tree.set_cursor(target);
    }

    fn confirm_search(&mut self, query: &str, origin: PathBuf) {
        if self.tree.location() != origin {
            self.remember_jump(origin);
        }
        if query.is_empty() {
            if self.last_search.is_some() {
                self.motion(Command::SearchNext, None, None);
            }
            return;
        }
        let level = self.tree.focused();
        if search::find(&level.entries, query, level.cursor, true, true).is_none() {
            self.message = Some(format!("pattern not found: {query}"));
        }
        self.last_search = Some(query.to_string());
    }

    fn run_ex(&mut self, line: &str) -> Response {
        let parsed = excmd::parse(line, self.tree.current_dir(), self.home.as_deref());
        let exit = |exit| Response {
            effect: None,
            exit: Some(exit),
        };
        match parsed {
            Ok(Ex::Quit) => return exit(Exit::Quit),
            Ok(Ex::Abort) => return exit(Exit::Abort),
            Ok(Ex::Help) => self.open_help(),
            Ok(Ex::Marks) => self.open_marks(),
            Ok(Ex::SetHidden(setting)) => {
                let on = setting.unwrap_or(!self.tree.show_hidden());
                self.tree.set_show_hidden(on);
            }
            Ok(Ex::Cd(path)) => self.change_root(&path),
            Err(message) => self.message = Some(message),
        }
        Response::default()
    }

    fn change_root(&mut self, path: &Path) {
        match path.canonicalize() {
            Ok(dir) if dir.is_dir() => {
                let before = self.tree.location();
                self.tree = Tree::new(dir, self.tree.show_hidden());
                self.remember_jump(before);
            }
            Ok(_) => self.message = Some(format!("not a directory: {}", path.display())),
            Err(e) => self.message = Some(format!("{}: {e}", path.display())),
        }
    }

    fn press_overlay(&mut self, key: Key) {
        let Mode::Overlay { lines, scroll, .. } = &mut self.mode else {
            return;
        };
        let last = lines.len().saturating_sub(1);
        let typed = key.typed_char();
        match (key.code, typed) {
            (KeyCode::Down, _) | (_, Some('j')) => *scroll = (*scroll + 1).min(last),
            (KeyCode::Up, _) | (_, Some('k')) => *scroll = scroll.saturating_sub(1),
            (KeyCode::PageDown, _) => *scroll = (*scroll + 10).min(last),
            (KeyCode::PageUp, _) => *scroll = scroll.saturating_sub(10),
            (KeyCode::Esc | KeyCode::Enter, _) | (_, Some('q' | '?')) => self.mode = Mode::Normal,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::marks::Marks;
    use std::{collections::BTreeMap, fs};

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for d in ["docs", "src", "target"] {
            fs::create_dir(tmp.path().join(d)).unwrap();
        }
        for f in ["Cargo.toml", "README.md", "notes.txt", "tsconfig.json"] {
            fs::write(tmp.path().join(f), "").unwrap();
        }
        fs::write(tmp.path().join("src/main.rs"), "").unwrap();
        fs::create_dir(tmp.path().join("src/deep")).unwrap();
        fs::write(tmp.path().join("src/deep/leaf.txt"), "").unwrap();
        tmp
    }

    fn open(path: &Path) -> App {
        let mut app = App::new(path.to_path_buf(), Keymap::default());
        app.tree_mut().settle();
        app
    }

    /// Types vim-notation keys, letting listings finish after each one. Returns the last non-empty response.
    fn keys(app: &mut App, text: &str) -> Response {
        let mut last = Response::default();
        for key in Key::parse_seq(text).unwrap() {
            let response = app.press(key);
            app.settle();
            if response != Response::default() {
                last = response;
            }
        }
        last
    }

    fn selected(app: &App) -> String {
        app.tree().focused().selected().unwrap().display_name()
    }

    #[test]
    fn counts_repeat_motions_and_clamp() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "3j");
        assert_eq!(selected(&app), "Cargo.toml");
        keys(&mut app, "2k");
        assert_eq!(selected(&app), "src");
        keys(&mut app, "99j");
        assert_eq!(selected(&app), "tsconfig.json");
    }

    #[test]
    fn gg_and_capital_g_jump_to_the_ends_or_to_a_numbered_entry() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "G");
        assert_eq!(selected(&app), "tsconfig.json");
        keys(&mut app, "gg");
        assert_eq!(selected(&app), "docs");
        keys(&mut app, "4G");
        assert_eq!(selected(&app), "Cargo.toml");
        keys(&mut app, "2gg");
        assert_eq!(selected(&app), "src");
        keys(&mut app, "500G");
        assert_eq!(selected(&app), "tsconfig.json");
    }

    #[test]
    fn page_motions_scale_with_the_viewport_and_count() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..100 {
            fs::write(tmp.path().join(format!("f{i:03}")), "").unwrap();
        }
        let mut app = open(tmp.path());
        app.set_viewport(12);
        keys(&mut app, "<c-d>");
        assert_eq!(app.tree().focused().cursor, 6);
        keys(&mut app, "<c-f>");
        assert_eq!(app.tree().focused().cursor, 16);
        keys(&mut app, "2<c-d>");
        assert_eq!(app.tree().focused().cursor, 28);
        keys(&mut app, "<c-u><c-b>");
        assert_eq!(app.tree().focused().cursor, 12);
    }

    #[test]
    fn f_jumps_to_names_by_first_letter_and_semicolon_repeats() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "ft");
        assert_eq!(selected(&app), "target");
        keys(&mut app, ";");
        assert_eq!(selected(&app), "tsconfig.json");
        keys(&mut app, ";");
        assert_eq!(selected(&app), "target", "wraps");
        keys(&mut app, ",");
        assert_eq!(selected(&app), "tsconfig.json");
        keys(&mut app, "Fc");
        assert_eq!(selected(&app), "Cargo.toml");
        keys(&mut app, "fz");
        assert_eq!(app.message.as_deref(), Some("no name starts with z"));
    }

    #[test]
    fn a_letter_that_is_a_command_can_still_be_jumped_to() {
        let tmp = tempfile::tempdir().unwrap();
        for f in ["alpha", "quokka", "zulu"] {
            fs::write(tmp.path().join(f), "").unwrap();
        }
        let mut app = open(tmp.path());
        keys(&mut app, "fq");
        assert_eq!(selected(&app), "quokka");
    }

    #[test]
    fn l_and_h_walk_levels_and_counts_repeat_them() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "j2l");
        assert_eq!(app.tree().focus(), 2, "src then deep");
        assert_eq!(app.tree().current_dir(), tmp.path().join("src/deep"));
        keys(&mut app, "2h");
        assert_eq!(app.tree().focus(), 0);
    }

    #[test]
    fn opening_a_file_returns_the_open_effect() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "3j");
        let response = keys(&mut app, "l");
        assert_eq!(
            response.effect,
            Some(Effect::Open(tmp.path().join("Cargo.toml")))
        );
    }

    #[test]
    fn quit_abort_and_escape_semantics() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        assert_eq!(keys(&mut app, "q").exit, Some(Exit::Quit));
        assert_eq!(keys(&mut app, "<c-c>").exit, Some(Exit::Abort));
        assert_eq!(
            keys(&mut app, "3<esc>").exit,
            None,
            "escape cancels the count"
        );
        assert_eq!(keys(&mut app, "<esc>").exit, Some(Exit::Quit));
    }

    #[test]
    fn pending_shows_the_half_typed_command() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "12g");
        assert_eq!(app.pending(), "12g");
        keys(&mut app, "g");
        assert_eq!(app.pending(), "");
        keys(&mut app, "f");
        assert_eq!(app.pending(), "f");
    }

    #[test]
    fn typing_a_search_moves_to_the_first_match_at_or_after_the_start() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "3j/t");
        assert_eq!(selected(&app), "Cargo.toml", "the current entry matches");
        keys(&mut app, "s");
        assert_eq!(selected(&app), "tsconfig.json");
        keys(&mut app, "<bs>");
        assert_eq!(
            selected(&app),
            "Cargo.toml",
            "backspace re-measures from the origin"
        );
    }

    #[test]
    fn matching_a_directory_shows_its_contents_in_the_preview() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/src");
        let preview = &app.tree().levels()[1];
        assert_eq!(preview.dir, tmp.path().join("src"));
        assert!(
            preview
                .entries
                .iter()
                .any(|e| e.display_name() == "main.rs")
        );
    }

    #[test]
    fn escape_restores_the_cursor_and_enter_keeps_it() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/notes<esc>");
        assert_eq!(selected(&app), "docs");
        assert!(app.prompt_view().is_none());
        keys(&mut app, "/notes<cr>");
        assert_eq!(selected(&app), "notes.txt");
        assert!(app.prompt_view().is_none());
    }

    #[test]
    fn backspace_on_an_empty_prompt_closes_it() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/<bs>");
        assert!(app.prompt_view().is_none());
    }

    #[test]
    fn motion_keys_are_query_text_while_the_prompt_is_open() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/jjq");
        assert_eq!(app.prompt_view().unwrap().text, "jjq");
        assert_eq!(app.tree().focus(), 0);
    }

    #[test]
    fn prompt_supports_cursor_editing() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/rdme<left><left><left>ea");
        assert_eq!(app.prompt_view().unwrap().text, "readme");
        assert_eq!(app.prompt_view().unwrap().cursor_col, 3);
        assert_eq!(selected(&app), "README.md");
    }

    #[test]
    fn n_and_capital_n_walk_the_matches_and_wrap() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/t<cr>");
        let mut seen = vec![selected(&app)];
        for _ in 0..4 {
            keys(&mut app, "n");
            seen.push(selected(&app));
        }
        assert_eq!(
            seen,
            [
                "target",
                "Cargo.toml",
                "notes.txt",
                "tsconfig.json",
                "target"
            ]
        );
        keys(&mut app, "N");
        assert_eq!(selected(&app), "tsconfig.json");
        keys(&mut app, "2n");
        assert_eq!(selected(&app), "Cargo.toml");
    }

    #[test]
    fn searching_uses_smartcase() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/readme");
        assert_eq!(selected(&app), "README.md");
        keys(&mut app, "<esc>/CARGO");
        assert_eq!(
            selected(&app),
            "docs",
            "an uppercase query is case sensitive and finds nothing"
        );
    }

    #[test]
    fn an_empty_search_repeats_the_previous_one() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/src<cr>gg/<cr>");
        assert_eq!(selected(&app), "src");
    }

    #[test]
    fn a_missing_match_leaves_the_cursor_and_says_so() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "2j/zzz");
        assert_eq!(selected(&app), "target");
        keys(&mut app, "<cr>");
        assert_eq!(app.message.as_deref(), Some("pattern not found: zzz"));
        keys(&mut app, "j");
        assert_eq!(app.message, None, "the notice clears on the next key");
        keys(&mut app, "n");
        assert_eq!(app.message.as_deref(), Some("pattern not found: zzz"));
    }

    #[test]
    fn n_without_a_previous_search_says_so() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "n");
        assert_eq!(app.message.as_deref(), Some("no previous search"));
    }

    #[test]
    fn colon_q_quits_and_colon_q_bang_aborts() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        assert_eq!(keys(&mut app, ":q<cr>").exit, Some(Exit::Quit));
        assert_eq!(keys(&mut app, ":q!<cr>").exit, Some(Exit::Abort));
    }

    #[test]
    fn colon_cd_restarts_the_tree_in_another_directory() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, ":cd src<cr>");
        assert_eq!(
            app.tree().current_dir(),
            tmp.path().join("src").canonicalize().unwrap()
        );
        assert_eq!(selected(&app), "deep");
        keys(&mut app, ":cd nowhere<cr>");
        assert!(
            app.message.as_deref().unwrap().contains("nowhere"),
            "{:?}",
            app.message
        );
        keys(&mut app, ":cd main.rs<cr>");
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .starts_with("not a directory"),
            "{:?}",
            app.message
        );
    }

    #[test]
    fn unknown_commands_are_reported_and_close_the_prompt() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, ":frobnicate<cr>");
        assert_eq!(app.message.as_deref(), Some("unknown command: frobnicate"));
        assert!(app.prompt_view().is_none());
    }

    #[test]
    fn help_opens_scrolls_stays_in_range_and_closes() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "?");
        assert_eq!(app.overlay().map(|o| o.scroll), Some(0));
        keys(&mut app, "jj");
        assert_eq!(app.overlay().map(|o| o.scroll), Some(2));
        keys(&mut app, &"j".repeat(500));
        assert_eq!(
            app.overlay().map(|o| o.scroll),
            Some(app.keymap().describe().len() - 1)
        );
        keys(&mut app, "k");
        assert_eq!(
            selected(&app),
            "docs",
            "keys do not move the tree behind the overlay"
        );
        keys(&mut app, "q");
        assert_eq!(app.overlay().map(|o| o.scroll), None);
        keys(&mut app, ":help<cr>");
        assert_eq!(app.overlay().map(|o| o.scroll), Some(0));
    }

    #[test]
    fn user_keymaps_change_what_keys_do() {
        let tmp = fixture();
        let keymap = Keymap::with_overrides(&BTreeMap::from([
            ("<space>".to_string(), "down".to_string()),
            ("q".to_string(), "none".to_string()),
        ]))
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf(), keymap);
        app.tree_mut().settle();
        keys(&mut app, "<space><space>");
        assert_eq!(selected(&app), "target");
        assert_eq!(keys(&mut app, "q").exit, None);
    }

    fn here(app: &App) -> PathBuf {
        app.tree().location()
    }

    #[test]
    fn a_mark_jumps_back_to_the_entry_from_anywhere() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "jllma");
        assert_eq!(here(&app), tmp.path().join("src/deep/leaf.txt"));
        keys(&mut app, "hhgg");
        assert_eq!(here(&app), tmp.path().join("docs"));
        keys(&mut app, "'a");
        assert_eq!(here(&app), tmp.path().join("src/deep/leaf.txt"));
        assert_eq!(app.tree().focus(), 2);
    }

    #[test]
    fn quote_quote_swaps_with_the_previous_position() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "G");
        assert_eq!(here(&app), tmp.path().join("tsconfig.json"));
        keys(&mut app, "''");
        assert_eq!(here(&app), tmp.path().join("docs"));
        keys(&mut app, "''");
        assert_eq!(here(&app), tmp.path().join("tsconfig.json"));
    }

    #[test]
    fn quote_quote_with_no_history_says_so() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "''");
        assert_eq!(app.message.as_deref(), Some("no previous position"));
        assert_eq!(here(&app), tmp.path().join("docs"));
    }

    #[test]
    fn unset_marks_and_invalid_names_are_reported() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "'z");
        assert_eq!(app.message.as_deref(), Some("mark z is not set"));
        keys(&mut app, "m1");
        assert_eq!(app.message.as_deref(), Some("marks are a-z and A-Z, not 1"));
    }

    #[test]
    fn a_mark_whose_target_was_deleted_reports_it() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "jllmahhgg");
        fs::remove_file(tmp.path().join("src/deep/leaf.txt")).unwrap();
        app.tree_mut().reload(&tmp.path().join("src/deep"));
        app.tree_mut().settle();
        keys(&mut app, "'a");
        let message = app.message.clone().unwrap_or_default();
        assert!(message.contains("no longer exists"), "{message:?}");
    }

    #[test]
    fn uppercase_marks_survive_a_restart_and_lowercase_ones_do_not() {
        let tmp = fixture();
        let state = tempfile::tempdir().unwrap();
        let file = state.path().join("marks");
        let mut app = App::with_settings(
            tmp.path().to_path_buf(),
            Keymap::default(),
            Settings {
                marks: Marks::open(Some(file.clone())),
                ..Settings::default()
            },
        );
        app.tree_mut().settle();
        keys(&mut app, "jmSmq");
        let mut again = App::with_settings(
            tmp.path().to_path_buf(),
            Keymap::default(),
            Settings {
                marks: Marks::open(Some(file)),
                ..Settings::default()
            },
        );
        again.tree_mut().settle();
        keys(&mut again, "'S");
        assert_eq!(here(&again), tmp.path().join("src"));
        keys(&mut again, "'q");
        assert_eq!(again.message.as_deref(), Some("mark q is not set"));
    }

    #[test]
    fn a_mark_outside_the_current_root_re_roots_the_tree() {
        let tmp = fixture();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("far.txt"), "x").unwrap();
        let mut app = open(tmp.path());
        app.marks.set('F', other.path().join("far.txt")).unwrap();
        keys(&mut app, "'F");
        assert_eq!(here(&app), other.path().join("far.txt"));
    }

    #[test]
    fn colon_marks_lists_them_in_an_overlay() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, ":marks<cr>");
        let view = app.overlay().unwrap();
        assert_eq!(view.title, "marks");
        assert!(view.lines[0].contains("no marks yet"));
        keys(&mut app, "q");
        keys(&mut app, "jma:marks<cr>");
        let view = app.overlay().unwrap();
        assert_eq!(
            view.lines,
            [format!("a   {}", tmp.path().join("src").display())]
        );
    }

    #[test]
    fn ctrl_o_and_tab_walk_the_places_you_jumped_from() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "jl");
        assert_eq!(here(&app), tmp.path().join("src/deep"));
        keys(&mut app, "<c-o>");
        assert_eq!(here(&app), tmp.path().join("src"));
        keys(&mut app, "<c-o>");
        assert_eq!(app.message.as_deref(), Some("already at the oldest jump"));
        keys(&mut app, "<tab>");
        assert_eq!(here(&app), tmp.path().join("src/deep"));
        keys(&mut app, "<tab>");
        assert_eq!(app.message.as_deref(), Some("already at the newest jump"));
    }

    #[test]
    fn plain_line_motions_do_not_pollute_the_jumplist() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "jjjkk<c-o>");
        assert_eq!(app.message.as_deref(), Some("already at the oldest jump"));
    }

    #[test]
    fn a_count_walks_several_jumps_at_once() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "GggG4G");
        keys(&mut app, "3<c-o>");
        assert_eq!(here(&app), tmp.path().join("docs"));
    }

    #[test]
    fn zh_and_set_hidden_toggle_dotfiles_and_keep_the_cursor() {
        let tmp = fixture();
        fs::write(tmp.path().join(".env"), "").unwrap();
        let mut app = open(tmp.path());
        keys(&mut app, "3j");
        assert_eq!(selected(&app), "Cargo.toml");
        keys(&mut app, "zh");
        let names: Vec<_> = app.tree().levels()[0]
            .entries
            .iter()
            .map(|e| e.display_name())
            .collect();
        assert!(names.contains(&".env".to_string()));
        assert_eq!(selected(&app), "Cargo.toml");
        keys(&mut app, "zh");
        assert!(!app.tree().show_hidden());
        keys(&mut app, ":set hidden<cr>");
        assert!(app.tree().show_hidden());
        keys(&mut app, ":set hidden!<cr>");
        assert!(!app.tree().show_hidden());
        keys(&mut app, ":set nohidden<cr>");
        assert!(!app.tree().show_hidden());
    }

    #[test]
    fn hidden_setting_survives_colon_cd() {
        let tmp = fixture();
        let mut app = App::with_settings(
            tmp.path().to_path_buf(),
            Keymap::default(),
            Settings {
                show_hidden: true,
                ..Settings::default()
            },
        );
        app.tree_mut().settle();
        keys(&mut app, ":cd src<cr>");
        assert!(app.tree().show_hidden());
    }

    #[test]
    fn colon_cd_is_a_jump_you_can_undo() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, ":cd src<cr>");
        keys(&mut app, "<c-o>");
        assert_eq!(here(&app), tmp.path().join("docs"));
    }

    #[test]
    fn a_search_that_moves_the_cursor_is_a_jump() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/notes<cr>");
        assert_eq!(selected(&app), "notes.txt");
        keys(&mut app, "<c-o>");
        assert_eq!(here(&app), tmp.path().join("docs"));
    }
}
