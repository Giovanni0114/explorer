//! Writing an edited file back without ever leaving it half written.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use crate::model::PreviewKey;

/// Size and modification time, which change whenever something else writes the file.
pub fn fingerprint(path: &Path) -> io::Result<PreviewKey> {
    let meta = fs::metadata(path)?;
    Ok((meta.len(), meta.modified().ok()))
}

/// Replaces the contents of `path` with `bytes`. A symlink is followed and its target written.
/// Normally the new text goes to a temporary file that is renamed over the old one, so a crash
/// leaves either the old or the new file. Files with several hard links, and files in folders
/// you cannot write to, are rewritten in place instead, which keeps the links and still works.
pub fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let meta = match fs::metadata(&target) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if meta.as_ref().is_some_and(|m| m.nlink() > 1) {
        return write_in_place(&target, bytes);
    }
    match write_via_rename(&target, bytes, meta.as_ref()) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && meta.is_some() => {
            write_in_place(&target, bytes)
        }
        other => other,
    }
}

fn temp_path(target: &Path) -> PathBuf {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!(".{name}.tx-{}.tmp", std::process::id()))
}

fn write_via_rename(target: &Path, bytes: &[u8], meta: Option<&fs::Metadata>) -> io::Result<()> {
    let temp = temp_path(target);
    let mode = meta.map_or(0o666, |m| m.mode() & 0o7777);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        file.write_all(bytes)?;
        if let Some(meta) = meta {
            // Only root can give a file away. For everyone else the file keeps their own ids.
            let _ = std::os::unix::fs::fchown(&file, Some(meta.uid()), Some(meta.gid()));
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return result;
    }
    if let Some(dir) = target.parent()
        && let Ok(dir) = fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn write_in_place(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).truncate(true).open(target)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o7777
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tx-"))
            .collect()
    }

    #[test]
    fn writes_replace_the_contents_and_keep_the_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("script.sh");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o750)).unwrap();
        write_file(&file, b"new text").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new text");
        assert_eq!(mode(&file), 0o750);
        assert!(leftovers(tmp.path()).is_empty());
    }

    #[test]
    fn a_symlink_stays_a_link_and_its_target_gets_the_text() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real.txt");
        let link = tmp.path().join("link.txt");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink("real.txt", &link).unwrap();
        write_file(&link, b"through the link").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "through the link");
    }

    #[test]
    fn hard_links_keep_sharing_the_same_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::write(&a, "old").unwrap();
        fs::hard_link(&a, &b).unwrap();
        write_file(&a, b"shared").unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "shared");
    }

    #[test]
    fn a_file_in_a_read_only_folder_is_rewritten_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("locked");
        fs::create_dir(&dir).unwrap();
        let file = dir.join("f.txt");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_file(&file, b"new");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        if nix_is_root() {
            return;
        }
        result.unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
    }

    #[test]
    fn a_read_only_file_is_refused_and_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("locked");
        fs::create_dir(&dir).unwrap();
        let file = dir.join("f.txt");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_file(&file, b"new");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        if nix_is_root() {
            return;
        }
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
    }

    #[test]
    fn a_missing_file_is_created() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("fresh.txt");
        write_file(&file, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello");
    }

    #[test]
    fn the_fingerprint_changes_when_the_file_does() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        fs::write(&file, "one").unwrap();
        let before = fingerprint(&file).unwrap();
        fs::write(&file, "three").unwrap();
        assert_ne!(fingerprint(&file).unwrap(), before);
    }

    fn nix_is_root() -> bool {
        fs::metadata("/proc/self")
            .map(|m| m.uid() == 0)
            .unwrap_or(false)
    }
}
