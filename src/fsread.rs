use std::{cmp::Ordering, ffi::OsStr, fs, io, path::Path};

use crate::model::{Entry, Kind};

/// Lists `dir`, directories first. Dotfiles are skipped unless `hidden` or named by `keep`.
pub fn read_dir(dir: &Path, keep: Option<&OsStr>, hidden: bool) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let name = item.file_name();
        let dotfile = name.as_encoded_bytes().first() == Some(&b'.');
        if dotfile && !hidden && keep != Some(name.as_os_str()) {
            continue;
        }
        let Ok(meta) = item.metadata() else { continue };
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            let target = fs::read_link(item.path()).unwrap_or_default();
            let to_dir = fs::metadata(item.path()).is_ok_and(|m| m.is_dir());
            Kind::Symlink { target, to_dir }
        } else if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        out.push(Entry {
            name,
            kind,
            size: meta.len(),
            mtime: meta.modified().ok(),
        });
    }
    out.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| compare_names(a, b))
    });
    Ok(out)
}

fn compare_names(a: &Entry, b: &Entry) -> Ordering {
    let (la, lb) = (
        a.display_name().to_lowercase(),
        b.display_name().to_lowercase(),
    );
    la.cmp(&lb).then_with(|| a.name.cmp(&b.name))
}
