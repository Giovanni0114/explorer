use std::ffi::OsString;

use ratatui::crossterm::event::KeyCode;

use crate::{keys::Key, lineedit::LineEditor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Search,
    Ex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptEvent {
    /// The text changed.
    Edited,
    /// Only the cursor moved, or the key did nothing.
    Idle,
    Confirm,
    Cancel,
}

/// A one-line input at the bottom of the screen.
#[derive(Debug)]
pub struct Prompt {
    pub kind: PromptKind,
    pub editor: LineEditor,
    /// Entry the cursor was on when a search opened, so cancelling can go back to it.
    pub origin: Option<OsString>,
}

impl Prompt {
    pub fn prefix(&self) -> char {
        match self.kind {
            PromptKind::Search => '/',
            PromptKind::Ex => ':',
        }
    }

    pub fn handle(&mut self, key: Key) -> PromptEvent {
        let e = &mut self.editor;
        let text = e.text().to_string();
        if let Some(c) = key.typed_char() {
            e.insert(c);
            return PromptEvent::Edited;
        }
        match (key.code, key.ctrl) {
            (KeyCode::Enter, _) => return PromptEvent::Confirm,
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => return PromptEvent::Cancel,
            (KeyCode::Backspace, _) | (KeyCode::Char('h'), true) if text.is_empty() => {
                return PromptEvent::Cancel;
            }
            (KeyCode::Backspace, _) | (KeyCode::Char('h'), true) => e.backspace(),
            (KeyCode::Delete, _) => e.delete(),
            (KeyCode::Left, _) | (KeyCode::Char('b'), true) => e.left(),
            (KeyCode::Right, _) | (KeyCode::Char('f'), true) => e.right(),
            (KeyCode::Home, _) | (KeyCode::Char('a'), true) => e.home(),
            (KeyCode::End, _) | (KeyCode::Char('e'), true) => e.end(),
            (KeyCode::Char('w'), true) => e.kill_word(),
            (KeyCode::Char('u'), true) => e.kill_to_start(),
            _ => return PromptEvent::Idle,
        }
        if e.text() == text {
            PromptEvent::Idle
        } else {
            PromptEvent::Edited
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> Prompt {
        Prompt {
            kind: PromptKind::Search,
            editor: LineEditor::default(),
            origin: None,
        }
    }

    fn press(p: &mut Prompt, keys: &str) -> PromptEvent {
        Key::parse_seq(keys)
            .unwrap()
            .into_iter()
            .map(|k| p.handle(k))
            .last()
            .unwrap()
    }

    #[test]
    fn typing_edits_and_movement_keys_are_idle() {
        let mut p = prompt();
        assert_eq!(press(&mut p, "ab"), PromptEvent::Edited);
        assert_eq!(press(&mut p, "<left>"), PromptEvent::Idle);
        assert_eq!(press(&mut p, "<home>"), PromptEvent::Idle);
        assert_eq!(press(&mut p, "<c-e>"), PromptEvent::Idle);
        assert_eq!(p.editor.text(), "ab");
    }

    #[test]
    fn command_letters_are_query_text() {
        let mut p = prompt();
        press(&mut p, "qjn/");
        assert_eq!(p.editor.text(), "qjn/");
    }

    #[test]
    fn readline_shortcuts_edit_the_line() {
        let mut p = prompt();
        press(&mut p, "cd some/dir");
        assert_eq!(press(&mut p, "<c-w>"), PromptEvent::Edited);
        assert_eq!(p.editor.text(), "cd ");
        assert_eq!(press(&mut p, "<c-u>"), PromptEvent::Edited);
        assert_eq!(p.editor.text(), "");
    }

    #[test]
    fn backspace_on_an_empty_line_cancels_like_vim() {
        let mut p = prompt();
        assert_eq!(press(&mut p, "<bs>"), PromptEvent::Cancel);
        press(&mut p, "a");
        assert_eq!(press(&mut p, "<bs>"), PromptEvent::Edited);
        assert_eq!(press(&mut p, "<bs>"), PromptEvent::Cancel);
    }

    #[test]
    fn enter_confirms_and_escape_or_ctrl_c_cancel() {
        let mut p = prompt();
        assert_eq!(press(&mut p, "<cr>"), PromptEvent::Confirm);
        assert_eq!(press(&mut p, "<esc>"), PromptEvent::Cancel);
        assert_eq!(press(&mut p, "<c-c>"), PromptEvent::Cancel);
    }

    #[test]
    fn deleting_at_the_end_changes_nothing() {
        let mut p = prompt();
        press(&mut p, "a");
        assert_eq!(press(&mut p, "<del>"), PromptEvent::Idle);
    }
}
