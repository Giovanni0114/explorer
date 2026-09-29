//! File operations. Every operation reports the operation that undoes it, and nothing ever
//! replaces or removes a file without the trash holding a copy.

use std::{
    ffi::{OsStr, OsString},
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const CHUNK: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Copy { from: PathBuf, to: PathBuf },
    Move { from: PathBuf, to: PathBuf },
    Trash { path: PathBuf },
    Restore(TrashRef),
    Mkdir { path: PathBuf },
    Touch { path: PathBuf },
    Chmod { path: PathBuf, mode: u32 },
}

/// Identifies one file in the trash, so it can be put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashRef {
    pub id: OsString,
    pub original: PathBuf,
}

/// A finished operation together with the one that reverses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub op: Op,
    pub undo: Op,
}

pub trait Trasher: Send + Sync {
    fn trash(&self, path: &Path) -> io::Result<TrashRef>;
    fn restore(&self, item: &TrashRef) -> io::Result<()>;
}

/// The freedesktop trash of the current user.
pub struct SystemTrash;

impl Trasher for SystemTrash {
    fn trash(&self, path: &Path) -> io::Result<TrashRef> {
        trash::delete(path).map_err(io::Error::other)?;
        let parent = path.parent().and_then(|p| fs::canonicalize(p).ok());
        let newest = trash::os_limited::list()
            .map_err(io::Error::other)?
            .into_iter()
            .filter(|item| {
                item.name == path.file_name().unwrap_or_default()
                    && fs::canonicalize(&item.original_parent).ok() == parent
            })
            .max_by_key(|item| item.time_deleted);
        newest
            .map(|item| TrashRef {
                id: item.id,
                original: path.to_path_buf(),
            })
            .ok_or_else(|| io::Error::other("moved to the trash, but cannot find it there to undo"))
    }

    fn restore(&self, item: &TrashRef) -> io::Result<()> {
        let found = trash::os_limited::list()
            .map_err(io::Error::other)?
            .into_iter()
            .find(|i| i.id == item.id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no longer in the trash"))?;
        trash::os_limited::restore_all([found]).map_err(io::Error::other)
    }
}

/// For builds and tests where no trash may be touched.
pub struct NoTrash;

impl Trasher for NoTrash {
    fn trash(&self, _: &Path) -> io::Result<TrashRef> {
        Err(io::Error::other("the trash is not available"))
    }

    fn restore(&self, _: &TrashRef) -> io::Result<()> {
        Err(io::Error::other("the trash is not available"))
    }
}

pub struct Ctx<'a> {
    pub trash: &'a dyn Trasher,
    pub cancel: &'a AtomicBool,
    /// Called with the bytes copied since the last call.
    pub progress: &'a (dyn Fn(u64) + Sync),
}

#[derive(Debug)]
struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

fn cancelled() -> io::Error {
    io::Error::other(Cancelled)
}

fn is_cancelled(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Cancelled>())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    Cancelled,
    Error(String),
}

#[derive(Debug)]
pub struct Outcome {
    /// Every operation that completed, in order, even when a later one failed.
    pub done: Vec<Done>,
    pub failure: Option<Failure>,
}

/// Runs the operations in order and stops at the first failure or cancel.
pub fn run_job(ops: &[Op], ctx: &Ctx, mut on_step: impl FnMut(usize)) -> Outcome {
    let mut done = Vec::new();
    for op in ops {
        if ctx.cancel.load(Ordering::Relaxed) {
            return Outcome {
                done,
                failure: Some(Failure::Cancelled),
            };
        }
        match run_op(op, ctx) {
            Ok(d) => done.push(d),
            Err(e) if is_cancelled(&e) => {
                return Outcome {
                    done,
                    failure: Some(Failure::Cancelled),
                };
            }
            Err(e) => {
                let message = format!("{}: {e}", primary_path(op).display());
                return Outcome {
                    done,
                    failure: Some(Failure::Error(message)),
                };
            }
        }
        on_step(done.len());
    }
    Outcome {
        done,
        failure: None,
    }
}

fn primary_path(op: &Op) -> &Path {
    match op {
        Op::Copy { from, .. } | Op::Move { from, .. } => from,
        Op::Trash { path } | Op::Mkdir { path } | Op::Touch { path } | Op::Chmod { path, .. } => {
            path
        }
        Op::Restore(item) => &item.original,
    }
}

pub fn run_op(op: &Op, ctx: &Ctx) -> io::Result<Done> {
    let undo = match op {
        Op::Copy { from, to } => {
            copy_tree(from, to, ctx)?;
            Op::Trash { path: to.clone() }
        }
        Op::Move { from, to } => {
            move_path(from, to, ctx)?;
            Op::Move {
                from: to.clone(),
                to: from.clone(),
            }
        }
        Op::Trash { path } => Op::Restore(ctx.trash.trash(path)?),
        Op::Restore(item) => {
            ctx.trash.restore(item)?;
            Op::Trash {
                path: item.original.clone(),
            }
        }
        Op::Mkdir { path } => {
            fs::create_dir(path)?;
            Op::Trash { path: path.clone() }
        }
        Op::Touch { path } => {
            OpenOptions::new().write(true).create_new(true).open(path)?;
            Op::Trash { path: path.clone() }
        }
        Op::Chmod { path, mode } => {
            let previous = fs::symlink_metadata(path)?.mode() & 0o7777;
            fs::set_permissions(path, fs::Permissions::from_mode(*mode))?;
            Op::Chmod {
                path: path.clone(),
                mode: previous,
            }
        }
    };
    Ok(Done {
        op: op.clone(),
        undo,
    })
}

/// Copies a file, directory or symlink to a path that must not exist yet. A failed or cancelled
/// copy removes what it created and nothing else.
fn copy_tree(from: &Path, to: &Path, ctx: &Ctx) -> io::Result<()> {
    if to.starts_with(from) {
        return Err(io::Error::other("cannot copy something into itself"));
    }
    let meta = fs::symlink_metadata(from)?;
    if !meta.is_dir() && !meta.is_file() && !meta.is_symlink() {
        return Err(io::Error::other("special files are not copied"));
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let result = copy_entry(from, to, &meta, ctx);
    if result.is_err() {
        let _ = remove_path(to);
    }
    result
}

fn copy_entry(from: &Path, to: &Path, meta: &fs::Metadata, ctx: &Ctx) -> io::Result<()> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return Err(cancelled());
    }
    if meta.is_symlink() {
        return std::os::unix::fs::symlink(fs::read_link(from)?, to);
    }
    if meta.is_file() {
        return copy_file(from, to, meta, ctx);
    }
    if !meta.is_dir() {
        return Ok(());
    }
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let child_meta = entry.metadata()?;
        copy_entry(&entry.path(), &to.join(entry.file_name()), &child_meta, ctx)?;
    }
    fs::set_permissions(to, meta.permissions())
}

fn copy_file(from: &Path, to: &Path, meta: &fs::Metadata, ctx: &Ctx) -> io::Result<()> {
    let mut src = File::open(from)?;
    let mut dst = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(meta.mode() & 0o7777)
        .open(to)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if ctx.cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        (ctx.progress)(n as u64);
    }
    dst.set_permissions(meta.permissions())?;
    if let Ok(modified) = meta.modified() {
        dst.set_modified(modified)?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    }
}

/// Renames without ever replacing an existing target. Across filesystems it copies, then removes the source.
fn move_path(from: &Path, to: &Path, ctx: &Ctx) -> io::Result<()> {
    match rename_noreplace(from, to) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => move_by_copy(from, to, ctx),
        other => other,
    }
}

fn move_by_copy(from: &Path, to: &Path, ctx: &Ctx) -> io::Result<()> {
    copy_tree(from, to, ctx)?;
    remove_path(from)
}

fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::{
        fs::{CWD, RenameFlags, renameat_with},
        io::Errno,
    };
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            fs::rename(from, to)
        }
        Err(e) => Err(e.into()),
    }
}

/// Bytes that copying `ops` will write, for progress. Unreadable paths count as zero.
pub fn total_bytes(ops: &[Op]) -> u64 {
    fn size(path: &Path) -> u64 {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return 0;
        };
        if meta.is_dir() {
            let Ok(entries) = fs::read_dir(path) else {
                return 0;
            };
            return entries.flatten().map(|e| size(&e.path())).sum();
        }
        if meta.is_file() { meta.len() } else { 0 }
    }
    ops.iter()
        .map(|op| match op {
            Op::Copy { from, .. } => size(from),
            _ => 0,
        })
        .sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    Copy,
    Cut,
}

impl PasteMode {
    fn op(self, from: PathBuf, to: PathBuf) -> Op {
        match self {
            PasteMode::Copy => Op::Copy { from, to },
            PasteMode::Cut => Op::Move { from, to },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Skip,
    /// Puts the existing file in the trash first.
    Overwrite,
    KeepBoth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    Clean,
    /// The target exists and the user has to decide.
    Conflict,
    /// Copying into the folder the file is already in makes a numbered copy.
    SameDirCopy,
    /// Moving a file into the folder it is already in.
    Noop,
    Problem(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteItem {
    pub from: PathBuf,
    pub to: PathBuf,
    pub kind: ItemKind,
}

pub fn plan_paste(
    sources: &[PathBuf],
    dest_dir: &Path,
    mode: PasteMode,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<PasteItem> {
    sources
        .iter()
        .map(|from| {
            let name = from.file_name().unwrap_or_default();
            let to = dest_dir.join(name);
            let same_dir = from.parent() == Some(dest_dir);
            let kind = if from.file_name().is_none() {
                ItemKind::Problem(format!("{} has no name", from.display()))
            } else if dest_dir.starts_with(from) {
                ItemKind::Problem(format!(
                    "cannot paste {} into itself",
                    name.to_string_lossy()
                ))
            } else if same_dir && mode == PasteMode::Cut {
                ItemKind::Noop
            } else if same_dir {
                ItemKind::SameDirCopy
            } else if exists(&to) {
                ItemKind::Conflict
            } else {
                ItemKind::Clean
            };
            PasteItem {
                from: from.clone(),
                to,
                kind,
            }
        })
        .collect()
}

/// The operations for one item. `choice` only matters for conflicts.
pub fn resolve(
    item: &PasteItem,
    mode: PasteMode,
    choice: Choice,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<Vec<Op>, String> {
    let keep_both = || {
        let to = unique_name(
            item.to.parent().unwrap_or(Path::new("/")),
            item.to.file_name().unwrap_or_default(),
            exists,
        );
        Ok(vec![mode.op(item.from.clone(), to)])
    };
    match &item.kind {
        ItemKind::Clean => Ok(vec![mode.op(item.from.clone(), item.to.clone())]),
        ItemKind::Noop => Ok(Vec::new()),
        ItemKind::Problem(message) => Err(message.clone()),
        ItemKind::SameDirCopy => keep_both(),
        ItemKind::Conflict => match choice {
            Choice::Skip => Ok(Vec::new()),
            Choice::Overwrite => Ok(vec![
                Op::Trash {
                    path: item.to.clone(),
                },
                mode.op(item.from.clone(), item.to.clone()),
            ]),
            Choice::KeepBoth => keep_both(),
        },
    }
}

/// `name (1).ext`, `name (2).ext`, … in `dir`, the first one that is free.
pub fn unique_name(dir: &Path, name: &OsStr, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let text = name.to_string_lossy();
    let (stem, ext) = match text.rfind('.') {
        Some(dot) if dot > 0 => (&text[..dot], &text[dot..]),
        _ => (&text[..], ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !exists(candidate))
        .expect("an unbounded range always yields a free name")
}

pub fn describe(op: &Op) -> String {
    let name = |p: &Path| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    match op {
        Op::Copy { from, .. } => format!("copy {}", name(from)),
        Op::Move { from, to } if from.parent() == to.parent() => {
            format!("rename {} to {}", name(from), name(to))
        }
        Op::Move { from, .. } => format!("move {}", name(from)),
        Op::Trash { path } => format!("trash {}", name(path)),
        Op::Restore(item) => format!("restore {}", name(&item.original)),
        Op::Mkdir { path } => format!("create folder {}", name(path)),
        Op::Touch { path } => format!("create file {}", name(path)),
        Op::Chmod { path, mode } => format!("chmod {mode:o} {}", name(path)),
    }
}

/// A trash that moves files into a directory, for tests.
#[cfg(test)]
pub struct FakeTrash {
    dir: PathBuf,
    counter: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl FakeTrash {
    pub fn new(dir: &Path) -> FakeTrash {
        fs::create_dir_all(dir).unwrap();
        FakeTrash {
            dir: dir.to_path_buf(),
            counter: 0.into(),
        }
    }

    pub fn len(&self) -> usize {
        fs::read_dir(&self.dir).unwrap().count()
    }
}

#[cfg(test)]
impl Trasher for FakeTrash {
    fn trash(&self, path: &Path) -> io::Result<TrashRef> {
        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let slot = self.dir.join(format!("item{id}"));
        fs::rename(path, &slot)?;
        Ok(TrashRef {
            id: slot.into_os_string(),
            original: path.to_path_buf(),
        })
    }

    fn restore(&self, item: &TrashRef) -> io::Result<()> {
        rename_noreplace(Path::new(&item.id), &item.original)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, process::Command};

    use super::*;

    struct Env {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        trash: FakeTrash,
        cancel: AtomicBool,
    }

    impl Env {
        fn new() -> Env {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("root");
            fs::create_dir(&root).unwrap();
            let trash = FakeTrash::new(&tmp.path().join("trash"));
            Env {
                _tmp: tmp,
                root,
                trash,
                cancel: AtomicBool::new(false),
            }
        }

        fn run(&self, ops: &[Op]) -> Outcome {
            let ctx = Ctx {
                trash: &self.trash,
                cancel: &self.cancel,
                progress: &|_| {},
            };
            run_job(ops, &ctx, |_| {})
        }

        fn p(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.p(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.p(rel)).unwrap()
        }

        fn snapshot(&self) -> BTreeMap<String, String> {
            fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
                for entry in fs::read_dir(dir).unwrap().flatten() {
                    let path = entry.path();
                    let rel = path.strip_prefix(root).unwrap().display().to_string();
                    let meta = fs::symlink_metadata(&path).unwrap();
                    if meta.is_dir() {
                        out.insert(format!("{rel}/"), String::new());
                        walk(&path, root, out);
                    } else if meta.is_symlink() {
                        out.insert(
                            rel,
                            format!("-> {}", fs::read_link(&path).unwrap().display()),
                        );
                    } else {
                        out.insert(rel, fs::read_to_string(&path).unwrap_or_default());
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(&self.root, &self.root, &mut out);
            out
        }
    }

    fn copy(from: PathBuf, to: PathBuf) -> Op {
        Op::Copy { from, to }
    }

    #[test]
    fn copying_a_tree_keeps_contents_links_modes_and_times() {
        let e = Env::new();
        e.write("src/a.txt", "alpha");
        e.write("src/sub/b.txt", "beta");
        std::os::unix::fs::symlink("a.txt", e.p("src/link")).unwrap();
        fs::set_permissions(e.p("src/a.txt"), fs::Permissions::from_mode(0o750)).unwrap();
        let outcome = e.run(&[copy(e.p("src"), e.p("dst"))]);
        assert!(outcome.failure.is_none(), "{outcome:?}");
        assert_eq!(e.read("dst/a.txt"), "alpha");
        assert_eq!(e.read("dst/sub/b.txt"), "beta");
        assert_eq!(fs::read_link(e.p("dst/link")).unwrap(), Path::new("a.txt"));
        let (a, b) = (
            fs::metadata(e.p("src/a.txt")).unwrap(),
            fs::metadata(e.p("dst/a.txt")).unwrap(),
        );
        assert_eq!(b.mode() & 0o7777, 0o750);
        assert_eq!(a.modified().unwrap(), b.modified().unwrap());
    }

    #[test]
    fn copying_never_overwrites_and_leaves_the_target_alone() {
        let e = Env::new();
        e.write("a.txt", "new");
        e.write("b.txt", "precious");
        let outcome = e.run(&[copy(e.p("a.txt"), e.p("b.txt"))]);
        assert!(matches!(outcome.failure, Some(Failure::Error(_))));
        assert_eq!(e.read("b.txt"), "precious");
        assert!(outcome.done.is_empty());
    }

    #[test]
    fn a_directory_cannot_be_copied_into_itself() {
        let e = Env::new();
        e.write("d/f.txt", "x");
        let outcome = e.run(&[copy(e.p("d"), e.p("d/inner"))]);
        let Some(Failure::Error(message)) = outcome.failure else {
            panic!("expected an error")
        };
        assert!(message.contains("into itself"), "{message}");
        assert!(!e.p("d/inner").exists());
    }

    #[test]
    fn cancelling_removes_the_partial_copy_and_keeps_the_source() {
        let e = Env::new();
        e.write("d/one.txt", "1");
        e.write("d/two.txt", "2");
        e.cancel.store(true, Ordering::Relaxed);
        let ctx = Ctx {
            trash: &e.trash,
            cancel: &e.cancel,
            progress: &|_| {},
        };
        let err = run_op(&copy(e.p("d"), e.p("d2")), &ctx).unwrap_err();
        assert!(is_cancelled(&err));
        assert!(!e.p("d2").exists(), "partial copy is cleaned up");
        assert_eq!(e.read("d/one.txt"), "1");
        e.cancel.store(false, Ordering::Relaxed);
        let outcome = e.run(&[copy(e.p("d"), e.p("d3"))]);
        assert!(outcome.failure.is_none());
    }

    #[test]
    fn a_job_cancelled_between_operations_reports_what_finished() {
        let e = Env::new();
        e.write("a", "a");
        let ctx = Ctx {
            trash: &e.trash,
            cancel: &e.cancel,
            progress: &|_| {},
        };
        let ops = [
            copy(e.p("a"), e.p("b")),
            copy(e.p("a"), e.p("c")),
            copy(e.p("a"), e.p("d")),
        ];
        let outcome = run_job(&ops, &ctx, |finished| {
            if finished == 1 {
                e.cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(outcome.failure, Some(Failure::Cancelled));
        assert_eq!(outcome.done.len(), 1);
        assert!(e.p("b").exists() && !e.p("c").exists());
    }

    #[test]
    fn special_files_are_refused_without_being_opened() {
        let e = Env::new();
        let fifo = e.p("pipe");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let outcome = e.run(&[copy(fifo, e.p("pipe2"))]);
        assert!(matches!(outcome.failure, Some(Failure::Error(_))));
        assert!(!e.p("pipe2").exists());
    }

    #[test]
    fn progress_reports_every_byte() {
        let e = Env::new();
        e.write("big", &"x".repeat(3 * CHUNK + 17));
        let seen = std::sync::atomic::AtomicU64::new(0);
        let ctx = Ctx {
            trash: &e.trash,
            cancel: &e.cancel,
            progress: &|n| {
                seen.fetch_add(n, Ordering::Relaxed);
            },
        };
        run_op(&copy(e.p("big"), e.p("big2")), &ctx).unwrap();
        assert_eq!(seen.load(Ordering::Relaxed), (3 * CHUNK + 17) as u64);
        assert_eq!(
            total_bytes(&[copy(e.p("big"), e.p("x")), Op::Mkdir { path: e.p("y") }]),
            (3 * CHUNK + 17) as u64
        );
    }

    #[test]
    fn moving_renames_and_refuses_to_replace() {
        let e = Env::new();
        e.write("a.txt", "a");
        e.write("b.txt", "b");
        let outcome = e.run(&[Op::Move {
            from: e.p("a.txt"),
            to: e.p("c.txt"),
        }]);
        assert!(outcome.failure.is_none());
        assert_eq!(e.read("c.txt"), "a");
        assert!(!e.p("a.txt").exists());
        let blocked = e.run(&[Op::Move {
            from: e.p("c.txt"),
            to: e.p("b.txt"),
        }]);
        assert!(matches!(blocked.failure, Some(Failure::Error(_))));
        assert_eq!((e.read("b.txt"), e.read("c.txt")), ("b".into(), "a".into()));
    }

    #[test]
    fn moving_across_devices_copies_then_removes_the_source() {
        let e = Env::new();
        e.write("d/x.txt", "x");
        let ctx = Ctx {
            trash: &e.trash,
            cancel: &e.cancel,
            progress: &|_| {},
        };
        move_by_copy(&e.p("d"), &e.p("moved"), &ctx).unwrap();
        assert_eq!(e.read("moved/x.txt"), "x");
        assert!(!e.p("d").exists());
    }

    #[test]
    fn a_failed_cross_device_move_keeps_the_source() {
        let e = Env::new();
        e.write("d/x.txt", "x");
        e.write("taken/y.txt", "y");
        let ctx = Ctx {
            trash: &e.trash,
            cancel: &e.cancel,
            progress: &|_| {},
        };
        assert!(move_by_copy(&e.p("d"), &e.p("taken"), &ctx).is_err());
        assert_eq!(e.read("d/x.txt"), "x");
        assert_eq!(e.read("taken/y.txt"), "y");
    }

    #[test]
    fn trash_and_restore_round_trip_and_restore_refuses_to_clobber() {
        let e = Env::new();
        e.write("a.txt", "a");
        let trashed = e.run(&[Op::Trash { path: e.p("a.txt") }]);
        assert!(!e.p("a.txt").exists());
        assert_eq!(e.trash.len(), 1);
        let undo = trashed.done[0].undo.clone();
        e.write("a.txt", "someone made a new one");
        let blocked = e.run(std::slice::from_ref(&undo));
        assert!(matches!(blocked.failure, Some(Failure::Error(_))));
        assert_eq!(e.read("a.txt"), "someone made a new one");
        fs::remove_file(e.p("a.txt")).unwrap();
        assert!(e.run(&[undo]).failure.is_none());
        assert_eq!(e.read("a.txt"), "a");
    }

    #[test]
    fn mkdir_touch_and_chmod_report_their_undo() {
        let e = Env::new();
        e.write("f", "x");
        fs::set_permissions(e.p("f"), fs::Permissions::from_mode(0o644)).unwrap();
        let outcome = e.run(&[
            Op::Mkdir { path: e.p("dir") },
            Op::Touch { path: e.p("new") },
            Op::Chmod {
                path: e.p("f"),
                mode: 0o600,
            },
        ]);
        assert!(outcome.failure.is_none());
        assert!(e.p("dir").is_dir() && e.p("new").is_file());
        assert_eq!(fs::metadata(e.p("f")).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            outcome.done[2].undo,
            Op::Chmod {
                path: e.p("f"),
                mode: 0o644
            }
        );
        assert!(
            e.run(&[Op::Mkdir { path: e.p("dir") }]).failure.is_some(),
            "existing folders are not recreated"
        );
        assert!(e.run(&[Op::Touch { path: e.p("f") }]).failure.is_some());
        assert_eq!(e.read("f"), "x", "touch never truncates");
    }

    #[test]
    fn a_job_stops_at_the_first_failure_and_keeps_the_finished_steps() {
        let e = Env::new();
        e.write("a", "a");
        let outcome = e.run(&[
            copy(e.p("a"), e.p("b")),
            copy(e.p("missing"), e.p("c")),
            copy(e.p("a"), e.p("d")),
        ]);
        assert_eq!(outcome.done.len(), 1);
        let Some(Failure::Error(message)) = outcome.failure else {
            panic!("expected an error")
        };
        assert!(message.contains("missing"), "{message}");
        assert!(!e.p("d").exists());
    }

    #[test]
    fn undoing_a_whole_job_restores_the_exact_starting_state() {
        let e = Env::new();
        e.write("src/a.txt", "from src");
        e.write("dst/a.txt", "original in dst");
        e.write("dst/keep.txt", "keep");
        let before = e.snapshot();
        let ops = [
            Op::Trash {
                path: e.p("dst/a.txt"),
            },
            copy(e.p("src/a.txt"), e.p("dst/a.txt")),
            Op::Mkdir { path: e.p("new") },
            Op::Move {
                from: e.p("dst/keep.txt"),
                to: e.p("new/keep.txt"),
            },
        ];
        let outcome = e.run(&ops);
        assert!(outcome.failure.is_none(), "{outcome:?}");
        assert_ne!(e.snapshot(), before);
        let undo: Vec<Op> = outcome.done.iter().rev().map(|d| d.undo.clone()).collect();
        let undone = e.run(&undo);
        assert!(undone.failure.is_none(), "{undone:?}");
        assert_eq!(e.snapshot(), before);
        let redo: Vec<Op> = undone.done.iter().rev().map(|d| d.undo.clone()).collect();
        let redone = e.run(&redo);
        assert!(redone.failure.is_none(), "{redone:?}");
        assert_eq!(e.read("dst/a.txt"), "from src");
        assert_eq!(e.read("new/keep.txt"), "keep");
    }

    #[test]
    fn unique_names_number_before_the_extension() {
        let taken = |names: &'static [&'static str]| {
            move |p: &Path| names.contains(&p.file_name().unwrap().to_str().unwrap())
        };
        let dir = Path::new("/d");
        let n = |name: &str, t: &dyn Fn(&Path) -> bool| unique_name(dir, OsStr::new(name), t);
        assert_eq!(
            n("a.txt", &taken(&["a (1).txt"])),
            Path::new("/d/a (2).txt")
        );
        assert_eq!(n("a.txt", &taken(&[])), Path::new("/d/a (1).txt"));
        assert_eq!(n("Makefile", &taken(&[])), Path::new("/d/Makefile (1)"));
        assert_eq!(n(".env", &taken(&[])), Path::new("/d/.env (1)"));
        assert_eq!(n("a.tar.gz", &taken(&[])), Path::new("/d/a.tar (1).gz"));
    }

    #[test]
    fn paste_planning_classifies_each_source() {
        let exists = |p: &Path| p == Path::new("/dst/taken");
        let item = |from: &str, dest: &str, mode| {
            plan_paste(&[PathBuf::from(from)], Path::new(dest), mode, &exists)
                .pop()
                .unwrap()
        };
        assert_eq!(
            item("/src/a", "/dst", PasteMode::Copy).kind,
            ItemKind::Clean
        );
        assert_eq!(
            item("/src/taken", "/dst", PasteMode::Copy).kind,
            ItemKind::Conflict
        );
        assert_eq!(item("/dst/a", "/dst", PasteMode::Cut).kind, ItemKind::Noop);
        assert_eq!(
            item("/dst/a", "/dst", PasteMode::Copy).kind,
            ItemKind::SameDirCopy
        );
        assert!(matches!(
            item("/dst", "/dst/inner", PasteMode::Copy).kind,
            ItemKind::Problem(_)
        ));
        assert!(matches!(
            item("/dst", "/dst", PasteMode::Cut).kind,
            ItemKind::Problem(_)
        ));
    }

    #[test]
    fn resolving_conflicts_by_choice() {
        let exists = |p: &Path| p == Path::new("/dst/a.txt");
        let item = PasteItem {
            from: "/src/a.txt".into(),
            to: "/dst/a.txt".into(),
            kind: ItemKind::Conflict,
        };
        let r = |choice| resolve(&item, PasteMode::Cut, choice, &exists).unwrap();
        assert_eq!(r(Choice::Skip), []);
        assert_eq!(
            r(Choice::Overwrite),
            [
                Op::Trash {
                    path: "/dst/a.txt".into()
                },
                Op::Move {
                    from: "/src/a.txt".into(),
                    to: "/dst/a.txt".into()
                }
            ]
        );
        assert_eq!(
            r(Choice::KeepBoth),
            [Op::Move {
                from: "/src/a.txt".into(),
                to: "/dst/a (1).txt".into()
            }]
        );
        let problem = PasteItem {
            kind: ItemKind::Problem("nope".into()),
            ..item.clone()
        };
        assert_eq!(
            resolve(&problem, PasteMode::Copy, Choice::Skip, &exists),
            Err("nope".into())
        );
    }

    #[test]
    fn descriptions_read_naturally() {
        let m = Op::Move {
            from: "/d/a".into(),
            to: "/d/b".into(),
        };
        assert_eq!(describe(&m), "rename a to b");
        let moved = Op::Move {
            from: "/d/a".into(),
            to: "/e/a".into(),
        };
        assert_eq!(describe(&moved), "move a");
        assert_eq!(
            describe(&Op::Trash {
                path: "/d/x".into()
            }),
            "trash x"
        );
    }
}
