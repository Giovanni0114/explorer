use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime},
};

use crate::preview::Content;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink { target: PathBuf, to_dir: bool },
    Other,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir | Kind::Symlink { to_dir: true, .. })
    }

    /// Something a file opener can act on (regular file, or a symlink that is not a directory).
    pub fn is_openable(&self) -> bool {
        matches!(self.kind, Kind::File | Kind::Symlink { to_dir: false, .. })
    }

    pub fn display_name(&self) -> String {
        self.name.to_string_lossy().into_owned()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Load {
    Loading { since: Instant },
    Ready,
    Failed(io::ErrorKind),
}

#[derive(Debug)]
pub struct Level {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    pub load: Load,
    /// Dotfile name to list even though hidden files are off (the directory we came from).
    keep: Option<OsString>,
    hidden: bool,
    /// Entry to put the cursor on once the first listing arrives.
    anchor: Option<OsString>,
}

impl Level {
    fn pending(dir: PathBuf, cursor: usize, hidden: bool) -> Level {
        Level {
            dir,
            entries: Vec::new(),
            cursor,
            load: Load::Loading {
                since: Instant::now(),
            },
            keep: None,
            hidden,
            anchor: None,
        }
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    fn request(&self) -> LoadRequest {
        LoadRequest {
            dir: self.dir.clone(),
            keep: self.keep.clone(),
            hidden: self.hidden,
        }
    }
}

/// A directory listing the runtime should produce and hand back through [`App::finish_load`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRequest {
    pub dir: PathBuf,
    pub keep: Option<OsString>,
    pub hidden: bool,
}

/// What identifies one version of a file's contents, so a changed file is previewed again.
pub type PreviewKey = (u64, Option<SystemTime>);

#[derive(Debug, Clone)]
pub enum PreviewState {
    Loading { since: Instant },
    Ready(Arc<Content>),
    Failed(String),
}

/// The file under the cursor, shown in a column after the last directory level.
#[derive(Debug)]
pub struct FilePreview {
    pub path: PathBuf,
    key: PreviewKey,
    pub state: PreviewState,
    /// First visible line.
    pub scroll: usize,
}

/// A file the runtime should read and turn into a [`Content`] for [`Tree::finish_preview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRequest {
    pub path: PathBuf,
    key: PreviewKey,
}

const PREVIEW_CACHE_CAP: usize = 64;

#[derive(Default)]
struct PreviewCache {
    entries: HashMap<(PathBuf, PreviewKey), Arc<Content>>,
    order: VecDeque<(PathBuf, PreviewKey)>,
}

impl PreviewCache {
    fn get(&self, path: &Path, key: PreviewKey) -> Option<Arc<Content>> {
        self.entries.get(&(path.to_path_buf(), key)).cloned()
    }

    fn insert(&mut self, path: PathBuf, key: PreviewKey, content: Arc<Content>) {
        if self.entries.insert((path.clone(), key), content).is_none() {
            self.order.push_back((path, key));
        }
        while self.order.len() > PREVIEW_CACHE_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

/// Something the runtime must do on the model's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Open(PathBuf),
    /// Put these files on the desktop clipboard.
    Clipboard {
        paths: Vec<PathBuf>,
        cut: bool,
    },
}

/// `levels[..=focus]` is the path; a directory under the cursor adds one preview level after it.
/// Once every level has loaded: `levels[i + 1].dir == levels[i].dir.join(selected name)`.
pub struct Tree {
    levels: Vec<Level>,
    focus: usize,
    memory: HashMap<PathBuf, usize>,
    requests: Vec<LoadRequest>,
    preview: Option<FilePreview>,
    preview_cache: PreviewCache,
    preview_requests: Vec<PreviewRequest>,
    show_hidden: bool,
    /// Path components still to walk after a [`Tree::reveal`], one per level as listings arrive.
    reveal: VecDeque<OsString>,
    notice: Option<String>,
}

impl Tree {
    pub fn new(root: PathBuf, show_hidden: bool) -> Tree {
        let mut tree = Tree {
            levels: Vec::new(),
            focus: 0,
            memory: HashMap::new(),
            requests: Vec::new(),
            preview: None,
            preview_cache: PreviewCache::default(),
            preview_requests: Vec::new(),
            show_hidden,
            reveal: VecDeque::new(),
            notice: None,
        };
        tree.push_level(Level::pending(root, 0, show_hidden));
        tree
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Shows or hides dotfiles in every level. Cursors stay on the same names.
    pub fn set_show_hidden(&mut self, on: bool) {
        if self.show_hidden == on {
            return;
        }
        self.show_hidden = on;
        for level in &mut self.levels {
            level.hidden = on;
            self.requests.push(level.request());
        }
    }

    /// Where the cursor is: the selected entry, or the directory itself when it is empty.
    pub fn location(&self) -> PathBuf {
        let level = self.focused();
        match level.selected() {
            Some(entry) => level.dir.join(&entry.name),
            None => level.dir.clone(),
        }
    }

    /// A one-line message about something that went wrong outside a key press, such as a bookmark whose target is gone.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Navigates to `path`, keeping the current root when the path is under it. Levels that are
    /// not listed yet are walked as their listings arrive.
    pub fn reveal(&mut self, path: &Path) {
        let root = self.levels[0].dir.clone();
        let (root, relative) = match path.strip_prefix(&root) {
            Ok(relative) if !relative.as_os_str().is_empty() => (root, relative.to_path_buf()),
            _ => match (path.parent(), path.file_name()) {
                (Some(parent), Some(name)) => (parent.to_path_buf(), PathBuf::from(name)),
                _ => (path.to_path_buf(), PathBuf::new()),
            },
        };
        if self.levels[0].dir == root {
            self.levels.truncate(1);
        } else {
            self.levels.clear();
            self.push_level(Level::pending(root, 0, self.show_hidden));
        }
        self.focus = 0;
        self.preview = None;
        self.reveal = relative
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        self.sync_preview();
        self.advance_reveal();
    }

    fn advance_reveal(&mut self) {
        while let Some(name) = self.reveal.front().cloned() {
            let level = &mut self.levels[self.focus];
            match level.load {
                Load::Ready => {}
                Load::Failed(_) => {
                    self.reveal.clear();
                    return;
                }
                Load::Loading { .. } => return,
            }
            let Some(index) = level.entries.iter().position(|e| e.name == name) else {
                let dotfile = name.as_encoded_bytes().first() == Some(&b'.');
                if dotfile && !level.hidden && level.keep.as_ref() != Some(&name) {
                    level.keep = Some(name);
                    level.load = Load::Loading {
                        since: Instant::now(),
                    };
                    self.requests.push(level.request());
                    return;
                }
                self.notice = Some(format!(
                    "no longer exists: {}",
                    level.dir.join(&name).display()
                ));
                self.reveal.clear();
                return;
            };
            self.set_cursor(index);
            self.reveal.pop_front();
            if !self.reveal.is_empty() && self.enter().is_some() {
                self.reveal.clear();
            }
        }
    }

    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    pub fn focus(&self) -> usize {
        self.focus
    }

    pub fn focused(&self) -> &Level {
        &self.levels[self.focus]
    }

    /// Directory the shell should return to: the one holding the cursor.
    pub fn current_dir(&self) -> &Path {
        &self.focused().dir
    }

    pub fn is_loading(&self) -> bool {
        self.levels
            .iter()
            .any(|l| matches!(l.load, Load::Loading { .. }))
            || self
                .preview
                .as_ref()
                .is_some_and(|p| matches!(p.state, PreviewState::Loading { .. }))
    }

    /// The file under the cursor, when there is one.
    pub fn preview(&self) -> Option<&FilePreview> {
        self.preview.as_ref()
    }

    pub fn take_preview_requests(&mut self) -> Vec<PreviewRequest> {
        std::mem::take(&mut self.preview_requests)
    }

    pub fn finish_preview(&mut self, request: &PreviewRequest, result: io::Result<Content>) {
        let outcome = result.map(Arc::new).map_err(|e| e.to_string());
        if let Ok(content) = &outcome {
            self.preview_cache
                .insert(request.path.clone(), request.key, Arc::clone(content));
        }
        if let Some(p) = self.preview.as_mut()
            && p.path == request.path
            && p.key == request.key
        {
            p.state = match outcome {
                Ok(content) => PreviewState::Ready(content),
                Err(message) => PreviewState::Failed(message),
            };
        }
    }

    /// Scrolls the preview by `delta` lines, keeping the last page full. `rows` is the height it is drawn in.
    pub fn scroll_preview(&mut self, delta: isize, rows: usize) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let PreviewState::Ready(content) = &p.state else {
            return;
        };
        let max = content.lines.len().saturating_sub(rows.max(1));
        p.scroll = p.scroll.saturating_add_signed(delta).min(max);
    }

    /// Directories currently on screen, for the runtime to watch.
    pub fn dirs(&self) -> impl Iterator<Item = &Path> {
        self.levels.iter().map(|l| l.dir.as_path())
    }

    pub fn take_requests(&mut self) -> Vec<LoadRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Re-list a directory that changed on disk. The cursor stays on the same entry name.
    pub fn reload(&mut self, dir: &Path) {
        if let Some(level) = self.levels.iter().find(|l| l.dir == dir) {
            self.requests.push(level.request());
        }
    }

    pub fn finish_load(&mut self, dir: &Path, result: io::Result<Vec<Entry>>) {
        let Some(i) = self.levels.iter().position(|l| l.dir == dir) else {
            return;
        };
        let level = &mut self.levels[i];
        let anchor = level
            .anchor
            .take()
            .or_else(|| level.selected().map(|e| e.name.clone()));
        match result {
            Ok(entries) => {
                level.entries = entries;
                level.load = Load::Ready;
            }
            Err(e) => {
                level.entries.clear();
                level.load = Load::Failed(e.kind());
            }
        }
        let found = anchor.and_then(|name| level.entries.iter().position(|e| e.name == name));
        level.cursor = found
            .unwrap_or(level.cursor)
            .min(level.entries.len().saturating_sub(1));
        self.memory.insert(level.dir.clone(), level.cursor);
        self.reconcile(i);
        self.advance_reveal();
    }

    pub fn move_by(&mut self, delta: isize) {
        self.set_cursor(self.focused().cursor.saturating_add_signed(delta));
    }

    /// Moves the focused cursor to `index`, clamped to the list.
    pub fn set_cursor(&mut self, index: usize) {
        let level = &mut self.levels[self.focus];
        let Some(max) = level.entries.len().checked_sub(1) else {
            return;
        };
        level.cursor = index.min(max);
        self.memory.insert(level.dir.clone(), level.cursor);
        self.sync_preview();
    }

    pub fn enter(&mut self) -> Option<Effect> {
        let level = &self.levels[self.focus];
        if let Some(entry) = level.selected()
            && entry.is_openable()
        {
            return Some(Effect::Open(level.dir.join(&entry.name)));
        }
        if self.levels.len() > self.focus + 1 {
            self.focus += 1;
            self.sync_preview();
        }
        None
    }

    pub fn leave(&mut self) {
        if self.focus > 0 {
            self.focus -= 1;
            self.sync_preview();
            return;
        }
        let child = self.levels[0].dir.clone();
        let (Some(parent), Some(name)) = (child.parent(), child.file_name()) else {
            return;
        };
        let mut level = Level::pending(parent.to_path_buf(), 0, self.show_hidden);
        level.keep = Some(name.to_os_string());
        level.anchor = Some(name.to_os_string());
        // The old levels stay behind the parent until it loads; `sync_preview` then reuses them.
        self.requests.push(level.request());
        self.levels.insert(0, level);
    }

    fn push_level(&mut self, level: Level) {
        self.requests.push(level.request());
        self.levels.push(level);
    }

    /// Makes the level after `focus` match the directory under the cursor, reusing it when it already does.
    fn sync_preview(&mut self) {
        let level = &self.levels[self.focus];
        let file = level
            .selected()
            .filter(|e| e.is_openable())
            .map(|e| (level.dir.join(&e.name), (e.size, e.mtime)));
        self.sync_file_preview(file);
        let level = &self.levels[self.focus];
        let wanted = level
            .selected()
            .filter(|e| e.is_dir())
            .map(|e| level.dir.join(&e.name));
        let Some(dir) = wanted else {
            self.levels.truncate(self.focus + 1);
            return;
        };
        if self
            .levels
            .get(self.focus + 1)
            .is_some_and(|l| l.dir == dir)
        {
            self.levels.truncate(self.focus + 2);
            return;
        }
        self.levels.truncate(self.focus + 1);
        let hint = self.memory.get(&dir).copied().unwrap_or(0);
        self.push_level(Level::pending(dir, hint, self.show_hidden));
    }

    fn sync_file_preview(&mut self, wanted: Option<(PathBuf, PreviewKey)>) {
        let Some((path, key)) = wanted else {
            self.preview = None;
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|p| p.path == path && p.key == key)
        {
            return;
        }
        let scroll = self
            .preview
            .as_ref()
            .filter(|p| p.path == path)
            .map_or(0, |p| p.scroll);
        let state = match self.preview_cache.get(&path, key) {
            Some(content) => PreviewState::Ready(content),
            None => {
                self.preview_requests.push(PreviewRequest {
                    path: path.clone(),
                    key,
                });
                PreviewState::Loading {
                    since: Instant::now(),
                }
            }
        };
        self.preview = Some(FilePreview {
            path,
            key,
            state,
            scroll,
        });
    }

    /// After level `i` changed, drop the levels to its right that no longer follow from its cursor.
    fn reconcile(&mut self, i: usize) {
        if i < self.focus {
            let level = &self.levels[i];
            let follows = level
                .selected()
                .is_some_and(|e| e.is_dir() && self.levels[i + 1].dir == level.dir.join(&e.name));
            if !follows {
                self.levels.truncate(i + 1);
                self.focus = i;
            }
        }
        self.sync_preview();
    }

    /// Runs every pending listing on this thread. For tests.
    #[cfg(test)]
    pub fn settle(&mut self) {
        loop {
            let requests = self.take_requests();
            if requests.is_empty() {
                break;
            }
            for r in requests {
                let result = crate::fsread::read_dir(&r.dir, r.keep.as_deref(), r.hidden);
                self.finish_load(&r.dir, result);
            }
        }
        for r in self.take_preview_requests() {
            let result = crate::preview::build(&r.path);
            self.finish_preview(&r, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        fs::create_dir_all(r.join("alpha/inner")).unwrap();
        fs::create_dir_all(r.join("beta")).unwrap();
        fs::write(r.join("alpha/one.txt"), "x").unwrap();
        fs::write(r.join("alpha/two.txt"), "xx").unwrap();
        fs::write(r.join("file.md"), "hello").unwrap();
        fs::write(r.join(".hidden"), "").unwrap();
        tmp
    }

    fn open(path: &Path) -> Tree {
        let mut tree = Tree::new(path.to_path_buf(), false);
        tree.settle();
        tree
    }

    fn names(level: &Level) -> Vec<String> {
        level.entries.iter().map(Entry::display_name).collect()
    }

    fn step<T>(tree: &mut Tree, f: impl FnOnce(&mut Tree) -> T) -> T {
        let out = f(tree);
        tree.settle();
        out
    }

    #[test]
    fn lists_dirs_first_and_hides_dotfiles() {
        let tmp = fixture();
        let tree = open(tmp.path());
        assert_eq!(names(&tree.levels()[0]), ["alpha", "beta", "file.md"]);
    }

    #[test]
    fn preview_follows_cursor_and_vanishes_on_files() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        assert_eq!(names(&tree.levels()[1]), ["inner", "one.txt", "two.txt"]);
        step(&mut tree, |t| t.move_by(1));
        assert_eq!(tree.levels()[1].dir, tmp.path().join("beta"));
        step(&mut tree, |t| t.move_by(1));
        assert_eq!(tree.levels().len(), 1);
    }

    #[test]
    fn enter_and_leave_walk_the_stack_and_remember_cursors() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.enter());
        assert_eq!(tree.focus(), 1);
        step(&mut tree, |t| t.move_by(2));
        step(&mut tree, |t| t.leave());
        assert_eq!(tree.focus(), 0);
        step(&mut tree, |t| t.enter());
        assert_eq!(tree.levels()[1].cursor, 2);
    }

    #[test]
    fn leave_at_root_loads_the_parent_with_cursor_on_the_child() {
        let tmp = fixture();
        let mut tree = open(&tmp.path().join("alpha"));
        step(&mut tree, |t| t.move_by(1));
        step(&mut tree, |t| t.leave());
        assert_eq!(tree.levels()[0].dir, tmp.path());
        assert_eq!(tree.levels()[0].selected().unwrap().display_name(), "alpha");
        assert_eq!(tree.levels()[1].dir, tmp.path().join("alpha"));
        assert_eq!(tree.levels().len(), 2);
    }

    #[test]
    fn leaving_a_hidden_directory_still_lists_it_in_the_parent() {
        let tmp = fixture();
        fs::create_dir_all(tmp.path().join(".dot/sub")).unwrap();
        let mut tree = open(&tmp.path().join(".dot"));
        step(&mut tree, |t| t.leave());
        assert_eq!(tree.levels()[0].selected().unwrap().display_name(), ".dot");
    }

    #[test]
    fn move_clamps_at_both_ends() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(isize::MAX));
        assert_eq!(tree.focused().cursor, 2);
        step(&mut tree, |t| t.move_by(isize::MIN));
        assert_eq!(tree.focused().cursor, 0);
    }

    #[test]
    fn unreadable_directory_becomes_a_failed_level() {
        let mut tree = open(Path::new("/definitely/not/here"));
        assert!(matches!(
            tree.levels()[0].load,
            Load::Failed(io::ErrorKind::NotFound)
        ));
        assert!(tree.levels()[0].entries.is_empty());
        assert_eq!(step(&mut tree, |t| t.enter()), None);
    }

    #[test]
    fn enter_on_a_file_asks_the_runtime_to_open_it() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(2));
        assert_eq!(
            step(&mut tree, |t| t.enter()),
            Some(Effect::Open(tmp.path().join("file.md")))
        );
        assert_eq!(tree.focus(), 0);
    }

    #[test]
    fn the_ui_never_waits_for_a_listing() {
        let tmp = fixture();
        let mut tree = Tree::new(tmp.path().to_path_buf(), false);
        assert!(tree.is_loading());
        assert!(tree.levels()[0].entries.is_empty());
        tree.move_by(1);
        assert_eq!(tree.take_requests().len(), 1);
    }

    #[test]
    fn results_for_levels_that_are_gone_are_ignored() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(1));
        let stale = tmp.path().join("alpha");
        tree.finish_load(&stale, Ok(Vec::new()));
        assert_eq!(tree.levels().len(), 2);
        assert_eq!(tree.levels()[1].dir, tmp.path().join("beta"));
    }

    #[test]
    fn reload_keeps_the_cursor_on_the_same_name_when_entries_appear_above() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(1));
        fs::create_dir(tmp.path().join("aaa")).unwrap();
        tree.reload(tmp.path());
        tree.settle();
        let level = &tree.levels()[0];
        assert_eq!(level.selected().unwrap().display_name(), "beta");
        assert_eq!(level.cursor, 2);
    }

    #[test]
    fn reload_shows_new_files_in_the_preview_level() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        fs::write(tmp.path().join("alpha/new.txt"), "").unwrap();
        tree.reload(&tmp.path().join("alpha"));
        tree.settle();
        assert!(names(&tree.levels()[1]).contains(&"new.txt".to_string()));
    }

    #[test]
    fn deleting_the_opened_directory_pulls_focus_back_to_its_parent() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.enter());
        fs::remove_dir_all(tmp.path().join("alpha")).unwrap();
        tree.reload(tmp.path());
        tree.settle();
        assert_eq!(tree.focus(), 0);
        assert_eq!(tree.levels()[0].selected().unwrap().display_name(), "beta");
        assert_eq!(tree.levels()[1].dir, tmp.path().join("beta"));
    }

    #[test]
    fn current_dir_is_the_directory_holding_the_cursor() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        assert_eq!(tree.current_dir(), tmp.path());
        step(&mut tree, |t| t.enter());
        assert_eq!(tree.current_dir(), tmp.path().join("alpha"));
    }

    fn preview_lines(tree: &Tree) -> Vec<String> {
        match &tree.preview().expect("a preview").state {
            PreviewState::Ready(c) => c.lines.iter().map(|l| l.text()).collect(),
            other => panic!("not ready: {other:?}"),
        }
    }

    #[test]
    fn a_file_under_the_cursor_gets_a_preview_and_directories_do_not() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        assert!(tree.preview().is_none(), "cursor starts on a directory");
        step(&mut tree, |t| t.move_by(2));
        assert_eq!(tree.preview().unwrap().path, tmp.path().join("file.md"));
        assert_eq!(preview_lines(&tree), ["hello"]);
        step(&mut tree, |t| t.move_by(-1));
        assert!(tree.preview().is_none());
    }

    #[test]
    fn previews_load_in_the_background_and_ignore_stale_results() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        tree.move_by(2);
        let request = tree.take_preview_requests().pop().unwrap();
        assert!(matches!(
            tree.preview().unwrap().state,
            PreviewState::Loading { .. }
        ));
        assert!(tree.is_loading());
        tree.move_by(-2);
        tree.finish_preview(&request, Ok(crate::preview::build(&request.path).unwrap()));
        assert!(
            tree.preview().is_none(),
            "the cursor left that file before the result arrived"
        );
        tree.move_by(2);
        assert!(
            tree.take_preview_requests().is_empty(),
            "the finished result was cached"
        );
        assert_eq!(preview_lines(&tree), ["hello"]);
    }

    #[test]
    fn changing_a_file_invalidates_its_preview() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(2));
        assert_eq!(preview_lines(&tree), ["hello"]);
        fs::write(tmp.path().join("file.md"), "changed on disk").unwrap();
        tree.reload(tmp.path());
        tree.settle();
        assert_eq!(preview_lines(&tree), ["changed on disk"]);
    }

    #[test]
    fn a_failed_preview_is_reported_and_retried_after_reselecting() {
        let tmp = fixture();
        fs::write(tmp.path().join("broken.zip"), "nope").unwrap();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(2));
        assert!(matches!(
            tree.preview().unwrap().state,
            PreviewState::Failed(_)
        ));
        step(&mut tree, |t| t.move_by(-1));
        step(&mut tree, |t| t.move_by(1));
        assert!(matches!(
            tree.preview().unwrap().state,
            PreviewState::Failed(_)
        ));
    }

    #[test]
    fn scrolling_stops_so_that_the_last_page_stays_full_and_survives_a_reload() {
        let tmp = fixture();
        let body: String = (0..100).map(|i| format!("l{i}\n")).collect();
        fs::write(tmp.path().join("file.md"), &body).unwrap();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(2));
        tree.scroll_preview(10, 20);
        assert_eq!(tree.preview().unwrap().scroll, 10);
        tree.scroll_preview(1000, 20);
        assert_eq!(tree.preview().unwrap().scroll, 80);
        tree.scroll_preview(-1000, 20);
        assert_eq!(tree.preview().unwrap().scroll, 0);
        tree.scroll_preview(5, 20);
        fs::write(tmp.path().join("file.md"), format!("{body}more\n")).unwrap();
        tree.reload(tmp.path());
        tree.settle();
        assert_eq!(
            tree.preview().unwrap().scroll,
            5,
            "same file keeps its scroll position"
        );
        step(&mut tree, |t| t.move_by(-1));
        step(&mut tree, |t| t.move_by(1));
        assert_eq!(
            tree.preview().unwrap().scroll,
            0,
            "a fresh selection starts at the top"
        );
    }

    #[test]
    fn the_preview_cache_evicts_the_oldest_entries() {
        let mut cache = PreviewCache::default();
        let content = Arc::new(Content {
            lines: Vec::new(),
            numbered: false,
        });
        for i in 0..PREVIEW_CACHE_CAP + 5 {
            cache.insert(
                PathBuf::from(format!("/f{i}")),
                (0, None),
                Arc::clone(&content),
            );
        }
        assert!(cache.get(Path::new("/f0"), (0, None)).is_none());
        assert!(cache.get(Path::new("/f4"), (0, None)).is_none());
        assert!(cache.get(Path::new("/f5"), (0, None)).is_some());
        assert_eq!(cache.entries.len(), PREVIEW_CACHE_CAP);
    }

    #[test]
    fn toggling_hidden_files_relists_every_level_and_keeps_the_cursor_name() {
        let tmp = fixture();
        fs::write(tmp.path().join("alpha/.secret"), "").unwrap();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.enter());
        assert!(!names(&tree.levels()[1]).contains(&".secret".to_string()));
        step(&mut tree, |t| t.set_show_hidden(true));
        assert!(names(&tree.levels()[0]).contains(&".hidden".to_string()));
        assert!(names(&tree.levels()[1]).contains(&".secret".to_string()));
        assert_eq!(tree.focused().selected().unwrap().display_name(), "inner");
        step(&mut tree, |t| t.set_show_hidden(false));
        assert!(!names(&tree.levels()[0]).contains(&".hidden".to_string()));
        assert!(tree.take_requests().is_empty());
    }

    #[test]
    fn hidden_files_setting_applies_to_levels_opened_later() {
        let tmp = fixture();
        fs::write(tmp.path().join("beta/.dot"), "").unwrap();
        let mut tree = Tree::new(tmp.path().to_path_buf(), true);
        tree.settle();
        step(&mut tree, |t| t.move_by(1));
        assert_eq!(names(&tree.levels()[1]), [".dot"]);
    }

    #[test]
    fn a_hidden_cursor_entry_that_disappears_leaves_a_valid_selection() {
        let tmp = fixture();
        let mut tree = Tree::new(tmp.path().to_path_buf(), true);
        tree.settle();
        let hidden_index = tree
            .focused()
            .entries
            .iter()
            .position(|e| e.display_name() == ".hidden")
            .unwrap();
        step(&mut tree, |t| t.set_cursor(hidden_index));
        step(&mut tree, |t| t.set_show_hidden(false));
        assert!(tree.focused().selected().is_some());
    }

    #[test]
    fn reveal_walks_down_to_a_deep_file_as_listings_arrive() {
        let tmp = fixture();
        fs::create_dir_all(tmp.path().join("alpha/inner/deep")).unwrap();
        fs::write(tmp.path().join("alpha/inner/deep/target.txt"), "found").unwrap();
        let mut tree = open(tmp.path());
        tree.reveal(&tmp.path().join("alpha/inner/deep/target.txt"));
        tree.settle();
        assert_eq!(
            tree.location(),
            tmp.path().join("alpha/inner/deep/target.txt")
        );
        assert_eq!(tree.focus(), 3);
        assert_eq!(preview_lines(&tree), ["found"]);
    }

    #[test]
    fn reveal_of_a_directory_selects_it_in_its_parent() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        tree.reveal(&tmp.path().join("alpha/inner"));
        tree.settle();
        assert_eq!(tree.location(), tmp.path().join("alpha/inner"));
        assert_eq!(tree.focus(), 1);
    }

    #[test]
    fn reveal_outside_the_root_re_roots_at_the_parent() {
        let tmp = fixture();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("x.txt"), "x").unwrap();
        let mut tree = open(tmp.path());
        tree.reveal(&other.path().join("x.txt"));
        tree.settle();
        assert_eq!(tree.levels()[0].dir, other.path());
        assert_eq!(tree.location(), other.path().join("x.txt"));
    }

    #[test]
    fn reveal_of_something_that_is_gone_stops_and_leaves_a_notice() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        tree.reveal(&tmp.path().join("alpha/missing/file.txt"));
        tree.settle();
        assert!(tree.take_notice().unwrap().contains("no longer exists"));
        assert_eq!(tree.take_notice(), None);
        assert!(tree.location().starts_with(tmp.path().join("alpha")));
    }

    #[test]
    fn reveal_of_the_current_root_or_its_parent_still_works() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        tree.reveal(tmp.path());
        tree.settle();
        assert_eq!(tree.levels()[0].dir, tmp.path().parent().unwrap());
        assert_eq!(tree.location(), tmp.path());
    }

    #[test]
    fn location_falls_back_to_the_directory_when_it_is_empty() {
        let tmp = fixture();
        let mut tree = open(tmp.path());
        step(&mut tree, |t| t.move_by(1));
        step(&mut tree, |t| t.enter());
        assert_eq!(tree.location(), tmp.path().join("beta"));
    }

    #[test]
    fn reveal_reaches_hidden_directories_even_when_dotfiles_are_off() {
        let tmp = fixture();
        fs::create_dir_all(tmp.path().join(".config/app")).unwrap();
        fs::write(tmp.path().join(".config/app/settings.toml"), "k = 1").unwrap();
        let mut tree = open(tmp.path());
        tree.reveal(&tmp.path().join(".config/app/settings.toml"));
        tree.settle();
        assert_eq!(
            tree.location(),
            tmp.path().join(".config/app/settings.toml")
        );
        assert_eq!(tree.take_notice(), None);
    }
}
