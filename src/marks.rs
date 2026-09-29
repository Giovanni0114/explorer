//! Vim-style marks. `a`-`z` last for the session. `A`-`Z` are bookmarks that are saved.

use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Default)]
pub struct Marks {
    session: BTreeMap<char, PathBuf>,
    saved: BTreeMap<char, PathBuf>,
    file: Option<PathBuf>,
}

impl Marks {
    /// Loads saved bookmarks from `file`. A missing or unreadable file means none.
    pub fn open(file: Option<PathBuf>) -> Marks {
        let saved = file
            .as_deref()
            .and_then(|f| fs::read_to_string(f).ok())
            .map(|text| parse(&text))
            .unwrap_or_default();
        Marks {
            session: BTreeMap::new(),
            saved,
            file,
        }
    }

    /// `$XDG_STATE_HOME/tx/marks`, or `~/.local/state/tx/marks`.
    pub fn default_file() -> Option<PathBuf> {
        let base = match env::var_os("XDG_STATE_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => PathBuf::from(env::var_os("HOME")?).join(".local/state"),
        };
        Some(base.join("tx").join("marks"))
    }

    pub fn set(&mut self, name: char, path: PathBuf) -> Result<(), String> {
        match name {
            'a'..='z' => {
                self.session.insert(name, path);
                Ok(())
            }
            'A'..='Z' => {
                self.saved.insert(name, path);
                self.save()
                    .map_err(|e| format!("could not save bookmark {name}: {e}"))
            }
            _ => Err(format!("marks are a-z and A-Z, not {name}")),
        }
    }

    pub fn get(&self, name: char) -> Option<&Path> {
        self.session
            .get(&name)
            .or_else(|| self.saved.get(&name))
            .map(PathBuf::as_path)
    }

    /// Every mark, `a`-`z` then `A`-`Z`.
    pub fn list(&self) -> Vec<(char, &Path)> {
        self.session
            .iter()
            .chain(&self.saved)
            .map(|(&c, p)| (c, p.as_path()))
            .collect()
    }

    fn save(&self) -> io::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let text: String = self
            .saved
            .iter()
            .filter_map(|(c, p)| Some(format!("{c}\t{}\n", p.to_str()?)))
            .collect();
        let temp = file.with_extension("tmp");
        fs::write(&temp, text)?;
        fs::rename(temp, file)
    }
}

fn parse(text: &str) -> BTreeMap<char, PathBuf> {
    text.lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t')?;
            let mut chars = name.chars();
            let (c, None) = (chars.next()?, chars.next()) else {
                return None;
            };
            (c.is_ascii_uppercase() && path.starts_with('/')).then(|| (c, PathBuf::from(path)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_marks_last_for_the_session_only() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("marks");
        let mut marks = Marks::open(Some(file.clone()));
        marks.set('a', "/one".into()).unwrap();
        assert_eq!(marks.get('a'), Some(Path::new("/one")));
        assert!(!file.exists(), "nothing is written for a session mark");
        assert_eq!(Marks::open(Some(file)).get('a'), None);
    }

    #[test]
    fn uppercase_marks_are_saved_and_reloaded() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("state/tx/marks");
        let mut marks = Marks::open(Some(file.clone()));
        marks.set('H', "/home/me".into()).unwrap();
        marks.set('P', "/srv/with space/x".into()).unwrap();
        let again = Marks::open(Some(file));
        assert_eq!(again.get('H'), Some(Path::new("/home/me")));
        assert_eq!(again.get('P'), Some(Path::new("/srv/with space/x")));
    }

    #[test]
    fn setting_a_mark_again_replaces_it() {
        let mut marks = Marks::default();
        marks.set('a', "/one".into()).unwrap();
        marks.set('a', "/two".into()).unwrap();
        assert_eq!(marks.get('a'), Some(Path::new("/two")));
        assert_eq!(marks.list().len(), 1);
    }

    #[test]
    fn only_letters_can_name_a_mark() {
        let mut marks = Marks::default();
        assert!(marks.set('1', "/x".into()).is_err());
        assert!(marks.set('é', "/x".into()).is_err());
        assert!(marks.set(' ', "/x".into()).is_err());
    }

    #[test]
    fn list_is_sorted_lowercase_first() {
        let mut marks = Marks::default();
        marks.set('B', "/b".into()).unwrap();
        marks.set('z', "/z".into()).unwrap();
        marks.set('a', "/a".into()).unwrap();
        let names: String = marks.list().into_iter().map(|(c, _)| c).collect();
        assert_eq!(names, "azB");
    }

    #[test]
    fn a_damaged_file_loses_only_the_bad_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("marks");
        fs::write(
            &file,
            "H\t/home\ngarbage\nx\t/lower\nAB\t/two\nQ\trelative\nZ\t/ok\n",
        )
        .unwrap();
        let marks = Marks::open(Some(file));
        let names: String = marks.list().into_iter().map(|(c, _)| c).collect();
        assert_eq!(names, "HZ");
    }

    #[test]
    fn saving_fails_softly_when_the_state_directory_is_unwritable() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        let mut marks = Marks::open(Some(blocker.join("marks")));
        let err = marks.set('A', "/x".into()).unwrap_err();
        assert!(err.contains("could not save bookmark A"), "{err}");
        assert_eq!(
            marks.get('A'),
            Some(Path::new("/x")),
            "the mark still works this session"
        );
    }

    #[test]
    fn without_a_state_file_uppercase_marks_still_work() {
        let mut marks = Marks::open(None);
        marks.set('A', "/x".into()).unwrap();
        assert_eq!(marks.get('A'), Some(Path::new("/x")));
    }
}
