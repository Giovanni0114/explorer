//! The jumplist behind Ctrl-o and Tab, like vim's: recording a jump drops the "forward" side.

use std::path::{Path, PathBuf};

const CAP: usize = 100;

#[derive(Debug, Default)]
pub struct Jumps {
    list: Vec<PathBuf>,
    /// Index of the current position while walking, or `list.len()` when not walking.
    pos: usize,
}

impl Jumps {
    /// Remembers `from` as a place to come back to.
    pub fn record(&mut self, from: PathBuf) {
        self.list.truncate(self.pos.min(self.list.len()));
        self.list.retain(|p| *p != from);
        self.list.push(from);
        if self.list.len() > CAP {
            self.list.remove(0);
        }
        self.pos = self.list.len();
    }

    /// The previous place, given where we are now. Walking back from the end saves `current`
    /// so that going forward again returns to it.
    pub fn back(&mut self, current: &Path) -> Option<PathBuf> {
        if self.pos >= self.list.len() {
            self.list.retain(|p| p != current);
            self.list.push(current.to_path_buf());
            self.pos = self.list.len() - 1;
        }
        if self.list[self.pos] != current {
            return Some(self.list[self.pos].clone());
        }
        let target = self.pos.checked_sub(1)?;
        self.pos = target;
        Some(self.list[target].clone())
    }

    pub fn forward(&mut self) -> Option<PathBuf> {
        let next = self.pos + 1;
        if next >= self.list.len() {
            return None;
        }
        self.pos = next;
        Some(self.list[next].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn back_and_forward_walk_the_recorded_places() {
        let mut j = Jumps::default();
        j.record(p("/a"));
        j.record(p("/b"));
        assert_eq!(j.back(Path::new("/c")), Some(p("/b")));
        assert_eq!(j.back(Path::new("/b")), Some(p("/a")));
        assert_eq!(j.back(Path::new("/a")), None, "the oldest place");
        assert_eq!(j.forward(), Some(p("/b")));
        assert_eq!(
            j.forward(),
            Some(p("/c")),
            "returns to where the walk started"
        );
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn nothing_recorded_means_nowhere_to_go() {
        let mut j = Jumps::default();
        assert_eq!(j.back(Path::new("/here")), None);
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn a_new_jump_after_going_back_drops_the_forward_side() {
        let mut j = Jumps::default();
        j.record(p("/a"));
        j.record(p("/b"));
        assert_eq!(j.back(Path::new("/c")), Some(p("/b")));
        j.record(p("/b"));
        assert_eq!(j.forward(), None);
        assert_eq!(j.back(Path::new("/d")), Some(p("/b")));
    }

    #[test]
    fn revisiting_a_place_moves_it_to_the_end_without_duplicates() {
        let mut j = Jumps::default();
        for path in ["/a", "/b", "/a"] {
            j.record(p(path));
        }
        assert_eq!(j.back(Path::new("/c")), Some(p("/a")));
        assert_eq!(j.back(Path::new("/a")), Some(p("/b")));
        assert_eq!(j.back(Path::new("/b")), None);
    }

    #[test]
    fn the_list_is_capped() {
        let mut j = Jumps::default();
        for i in 0..CAP + 20 {
            j.record(p(&format!("/d{i}")));
        }
        let mut count = 0;
        let mut here = p("/now");
        while let Some(prev) = j.back(&here) {
            here = prev;
            count += 1;
        }
        assert_eq!(count, CAP);
    }
}
