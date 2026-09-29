//! State around file operations: the yank register, the selection, the job that is running,
//! the undo journal and the step-by-step conflict questions of a paste.

use std::{
    collections::{BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::ops::{
    Choice, Done, Failure, ItemKind, Op, Outcome, PasteItem, PasteMode, Trasher, resolve,
};

const JOURNAL_CAP: usize = 100;

pub struct Register {
    pub paths: Vec<PathBuf>,
    pub mode: PasteMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Normal,
    Undo,
    Redo,
}

/// Everything one user action changed, in the order it happened.
#[derive(Debug, Clone)]
pub struct Batch {
    pub label: String,
    pub done: Vec<Done>,
}

/// Work for the runtime's operation thread.
pub struct Job {
    pub id: u64,
    pub ops: Vec<Op>,
    pub cancel: Arc<AtomicBool>,
}

pub struct JobSpec {
    pub label: String,
    pub success: String,
    pub ops: Vec<Op>,
    /// Entry to put the cursor on when the job is done.
    pub focus_on: Option<PathBuf>,
    /// A finished move empties the register, since the files are no longer where it says.
    pub clears_register: bool,
}

struct Running {
    id: u64,
    kind: JobKind,
    label: String,
    success: String,
    cancel: Arc<AtomicBool>,
    total_ops: usize,
    progress: (u64, u64),
    focus_on: Option<PathBuf>,
    clears_register: bool,
    /// The batch an undo or redo is working through.
    source: Option<Batch>,
}

/// What the app should do once a job is over.
#[derive(Debug, PartialEq, Eq)]
pub struct Finished {
    pub message: String,
    /// Directories whose listings are stale now.
    pub reload: Vec<PathBuf>,
    pub focus_on: Option<PathBuf>,
}

pub struct FileOps {
    register: Option<Register>,
    selection: BTreeSet<PathBuf>,
    undo: Vec<Batch>,
    redo: Vec<Batch>,
    outbox: Vec<Job>,
    running: Option<Running>,
    next_id: u64,
    trasher: Arc<dyn Trasher>,
}

impl FileOps {
    pub fn new(trasher: Arc<dyn Trasher>) -> FileOps {
        FileOps {
            register: None,
            selection: BTreeSet::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            outbox: Vec::new(),
            running: None,
            next_id: 1,
            trasher,
        }
    }

    pub fn trasher(&self) -> Arc<dyn Trasher> {
        Arc::clone(&self.trasher)
    }

    pub fn register(&self) -> Option<&Register> {
        self.register.as_ref()
    }

    pub fn set_register(&mut self, paths: Vec<PathBuf>, mode: PasteMode) {
        self.register = Some(Register { paths, mode });
    }

    pub fn selection(&self) -> &BTreeSet<PathBuf> {
        &self.selection
    }

    pub fn toggle_selected(&mut self, path: PathBuf) {
        if !self.selection.remove(&path) {
            self.selection.insert(path);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// What is running, and how far along it is, if it can tell.
    pub fn running(&self) -> Option<(&str, Option<u8>)> {
        let r = self.running.as_ref()?;
        let percent = (r.progress.1 > 0)
            .then(|| (r.progress.0.saturating_mul(100) / r.progress.1).min(100) as u8);
        Some((&r.label, percent))
    }

    pub fn cancel(&mut self) -> bool {
        match &self.running {
            Some(r) => {
                r.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn start(&mut self, spec: JobSpec) -> Result<(), String> {
        self.launch(spec, JobKind::Normal, None)
    }

    pub fn undo(&mut self) -> Result<(), String> {
        self.check_idle()?;
        let batch = self.undo.pop().ok_or("nothing to undo")?;
        let spec = reversal(&batch, format!("undid {}", batch.label));
        self.launch(spec, JobKind::Undo, Some(batch))
    }

    pub fn redo(&mut self) -> Result<(), String> {
        self.check_idle()?;
        let batch = self.redo.pop().ok_or("nothing to redo")?;
        let spec = reversal(&batch, format!("redid {}", batch.label));
        self.launch(spec, JobKind::Redo, Some(batch))
    }

    fn check_idle(&self) -> Result<(), String> {
        match &self.running {
            Some(r) => Err(format!("busy: {} (Esc cancels)", r.label)),
            None => Ok(()),
        }
    }

    fn launch(
        &mut self,
        spec: JobSpec,
        kind: JobKind,
        source: Option<Batch>,
    ) -> Result<(), String> {
        self.check_idle()?;
        if spec.ops.is_empty() {
            return Err("nothing to do".into());
        }
        let id = self.next_id;
        self.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some(Running {
            id,
            kind,
            label: spec.label,
            success: spec.success,
            cancel: Arc::clone(&cancel),
            total_ops: spec.ops.len(),
            progress: (0, 0),
            focus_on: spec.focus_on,
            clears_register: spec.clears_register,
            source,
        });
        self.outbox.push(Job {
            id,
            ops: spec.ops,
            cancel,
        });
        Ok(())
    }

    pub fn take_jobs(&mut self) -> Vec<Job> {
        std::mem::take(&mut self.outbox)
    }

    pub fn progress(&mut self, id: u64, done: u64, total: u64) {
        if let Some(r) = self.running.as_mut().filter(|r| r.id == id) {
            r.progress = (done, total);
        }
    }

    pub fn finish(&mut self, id: u64, outcome: Outcome) -> Option<Finished> {
        if self.running.as_ref().is_none_or(|r| r.id != id) {
            return None;
        }
        let r = self.running.take()?;
        let (finished, total) = (outcome.done.len(), r.total_ops);
        let message = match &outcome.failure {
            None => r.success.clone(),
            Some(Failure::Cancelled) => format!("cancelled, {finished} of {total} done"),
            Some(Failure::Error(e)) => format!("{e} ({finished} of {total} done)"),
        };
        let reload = touched_dirs(&outcome.done);
        if outcome.failure.is_none() && r.clears_register {
            self.register = None;
        }
        self.journal(&r, outcome.done);
        Some(Finished {
            message,
            reload,
            focus_on: r.focus_on.filter(|_| finished > 0),
        })
    }

    fn journal(&mut self, r: &Running, done: Vec<Done>) {
        let finished = done.len();
        let label = r
            .source
            .as_ref()
            .map_or(r.label.clone(), |s| s.label.clone());
        let leftover = r
            .source
            .as_ref()
            .map(|s| Batch {
                label: s.label.clone(),
                done: s.done[..s.done.len().saturating_sub(finished)].to_vec(),
            })
            .filter(|b| !b.done.is_empty());
        let batch = (finished > 0).then_some(Batch { label, done });
        match r.kind {
            JobKind::Normal => {
                if let Some(batch) = batch {
                    push_capped(&mut self.undo, batch);
                    self.redo.clear();
                }
            }
            JobKind::Undo => {
                if let Some(batch) = batch {
                    push_capped(&mut self.redo, batch);
                }
                if let Some(rest) = leftover {
                    self.undo.push(rest);
                }
            }
            JobKind::Redo => {
                if let Some(batch) = batch {
                    push_capped(&mut self.undo, batch);
                }
                if let Some(rest) = leftover {
                    self.redo.push(rest);
                }
            }
        }
    }
}

fn push_capped(stack: &mut Vec<Batch>, batch: Batch) {
    stack.push(batch);
    if stack.len() > JOURNAL_CAP {
        stack.remove(0);
    }
}

fn reversal(batch: &Batch, success: String) -> JobSpec {
    JobSpec {
        label: format!("undoing {}", batch.label),
        success,
        ops: batch.done.iter().rev().map(|d| d.undo.clone()).collect(),
        focus_on: None,
        clears_register: false,
    }
}

fn touched_dirs(done: &[Done]) -> Vec<PathBuf> {
    let mut dirs = BTreeSet::new();
    for step in done {
        for op in [&step.op, &step.undo] {
            let paths: Vec<&Path> = match op {
                Op::Copy { from, to } | Op::Move { from, to } => vec![from, to],
                Op::Trash { path }
                | Op::Mkdir { path }
                | Op::Touch { path }
                | Op::Chmod { path, .. } => vec![path],
                Op::Restore(item) => vec![&item.original],
            };
            dirs.extend(
                paths
                    .into_iter()
                    .filter_map(Path::parent)
                    .map(Path::to_path_buf),
            );
        }
    }
    dirs.into_iter().collect()
}

/// What a paste needs next.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// The target exists. The user chooses what to do with this item.
    Ask(PasteItem),
    Ready(Vec<Op>),
    Failed(String),
}

/// Walks the items of a paste, asking about each conflict unless one answer covers all of them.
pub struct PasteFlow {
    mode: PasteMode,
    queue: VecDeque<PasteItem>,
    ops: Vec<Op>,
    all: Option<Choice>,
}

impl PasteFlow {
    pub fn new(items: Vec<PasteItem>, mode: PasteMode) -> PasteFlow {
        PasteFlow {
            mode,
            queue: items.into(),
            ops: Vec::new(),
            all: None,
        }
    }

    pub fn advance(&mut self, exists: &dyn Fn(&Path) -> bool) -> Step {
        while let Some(item) = self.queue.front() {
            if item.kind == ItemKind::Conflict && self.all.is_none() {
                return Step::Ask(item.clone());
            }
            let item = self.queue.pop_front().expect("front was just seen");
            match resolve(&item, self.mode, self.all.unwrap_or(Choice::Skip), exists) {
                Ok(ops) => self.ops.extend(ops),
                Err(message) => return Step::Failed(message),
            }
        }
        Step::Ready(std::mem::take(&mut self.ops))
    }

    /// Answers the question `advance` asked. `all` applies the answer to every later conflict too.
    pub fn choose(&mut self, choice: Choice, all: bool, exists: &dyn Fn(&Path) -> bool) -> Step {
        if all {
            self.all = Some(choice);
        }
        if let Some(item) = self.queue.pop_front() {
            match resolve(&item, self.mode, choice, exists) {
                Ok(ops) => self.ops.extend(ops),
                Err(message) => return Step::Failed(message),
            }
        }
        self.advance(exists)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{FakeTrash, NoTrash, plan_paste, run_job};

    fn ops() -> FileOps {
        FileOps::new(Arc::new(NoTrash))
    }

    fn spec(label: &str, op_count: usize) -> JobSpec {
        JobSpec {
            label: label.into(),
            success: format!("{label} done"),
            ops: (0..op_count)
                .map(|i| Op::Mkdir {
                    path: format!("/tmp/x{i}").into(),
                })
                .collect(),
            focus_on: Some("/tmp/x0".into()),
            clears_register: false,
        }
    }

    fn ok(n: usize) -> Outcome {
        Outcome {
            done: (0..n)
                .map(|i| Done {
                    op: Op::Mkdir {
                        path: format!("/tmp/x{i}").into(),
                    },
                    undo: Op::Trash {
                        path: format!("/tmp/x{i}").into(),
                    },
                })
                .collect(),
            failure: None,
        }
    }

    #[test]
    fn one_job_runs_at_a_time() {
        let mut f = ops();
        f.start(spec("first", 1)).unwrap();
        assert_eq!(f.take_jobs().len(), 1);
        let err = f.start(spec("second", 1)).unwrap_err();
        assert!(err.contains("busy: first"), "{err}");
        assert!(f.undo().unwrap_err().contains("busy"));
        assert_eq!(
            f.start(spec("empty", 0)).unwrap_err(),
            "busy: first (Esc cancels)"
        );
    }

    #[test]
    fn empty_jobs_are_refused() {
        let mut f = ops();
        assert_eq!(f.start(spec("nothing", 0)).unwrap_err(), "nothing to do");
    }

    #[test]
    fn finishing_reports_success_and_lists_the_stale_directories() {
        let mut f = ops();
        f.start(spec("make", 2)).unwrap();
        let id = f.take_jobs()[0].id;
        let finished = f.finish(id, ok(2)).unwrap();
        assert_eq!(finished.message, "make done");
        assert_eq!(finished.reload, [PathBuf::from("/tmp")]);
        assert_eq!(finished.focus_on, Some("/tmp/x0".into()));
        assert!(f.running().is_none());
    }

    #[test]
    fn results_of_other_jobs_are_ignored() {
        let mut f = ops();
        f.start(spec("make", 1)).unwrap();
        assert!(f.finish(999, ok(1)).is_none());
        assert!(f.running().is_some());
    }

    #[test]
    fn progress_is_a_percentage_once_the_total_is_known() {
        let mut f = ops();
        f.start(spec("copy", 1)).unwrap();
        let id = f.take_jobs()[0].id;
        assert_eq!(f.running(), Some(("copy", None)));
        f.progress(id, 50, 200);
        assert_eq!(f.running(), Some(("copy", Some(25))));
        f.progress(id, 500, 200);
        assert_eq!(f.running(), Some(("copy", Some(100))));
        f.progress(id + 1, 1, 2);
        assert_eq!(
            f.running(),
            Some(("copy", Some(100))),
            "other jobs do not move it"
        );
    }

    #[test]
    fn cancel_signals_the_running_job_and_the_partial_result_is_reported() {
        let mut f = ops();
        assert!(!f.cancel());
        f.start(spec("copy", 3)).unwrap();
        let job = f.take_jobs().pop().unwrap();
        assert!(f.cancel());
        assert!(job.cancel.load(Ordering::Relaxed));
        let mut outcome = ok(1);
        outcome.failure = Some(Failure::Cancelled);
        assert_eq!(
            f.finish(job.id, outcome).unwrap().message,
            "cancelled, 1 of 3 done"
        );
        assert!(f.undo().is_ok(), "the finished step can still be undone");
    }

    #[test]
    fn a_failure_keeps_the_finished_steps_undoable_and_names_the_error() {
        let mut f = ops();
        f.start(spec("copy", 3)).unwrap();
        let id = f.take_jobs()[0].id;
        let mut outcome = ok(2);
        outcome.failure = Some(Failure::Error("/x/y: permission denied".into()));
        let finished = f.finish(id, outcome).unwrap();
        assert_eq!(finished.message, "/x/y: permission denied (2 of 3 done)");
        f.undo().unwrap();
        assert_eq!(f.take_jobs()[0].ops.len(), 2);
    }

    #[test]
    fn a_job_that_did_nothing_is_not_journaled_and_does_not_focus() {
        let mut f = ops();
        f.start(spec("copy", 1)).unwrap();
        let id = f.take_jobs()[0].id;
        let mut outcome = ok(0);
        outcome.failure = Some(Failure::Error("nope".into()));
        assert_eq!(f.finish(id, outcome).unwrap().focus_on, None);
        assert_eq!(f.undo().unwrap_err(), "nothing to undo");
    }

    #[test]
    fn undo_and_redo_walk_the_journal_in_opposite_orders() {
        let mut f = ops();
        for label in ["one", "two"] {
            f.start(spec(label, 1)).unwrap();
            let id = f.take_jobs()[0].id;
            f.finish(id, ok(1));
        }
        f.undo().unwrap();
        let job = f.take_jobs().pop().unwrap();
        assert!(matches!(job.ops[0], Op::Trash { .. }), "the inverse runs");
        let done = f.finish(job.id, ok(1)).unwrap();
        assert_eq!(done.message, "undid two");
        f.undo().unwrap();
        let job = f.take_jobs().pop().unwrap();
        assert_eq!(f.finish(job.id, ok(1)).unwrap().message, "undid one");
        assert_eq!(f.undo().unwrap_err(), "nothing to undo");
        f.redo().unwrap();
        let job = f.take_jobs().pop().unwrap();
        assert_eq!(f.finish(job.id, ok(1)).unwrap().message, "redid one");
        f.redo().unwrap();
        assert_eq!(f.take_jobs().len(), 1);
    }

    #[test]
    fn a_new_action_clears_the_redo_stack() {
        let mut f = ops();
        f.start(spec("one", 1)).unwrap();
        let id = f.take_jobs()[0].id;
        f.finish(id, ok(1));
        f.undo().unwrap();
        let id = f.take_jobs()[0].id;
        f.finish(id, ok(1));
        f.start(spec("two", 1)).unwrap();
        let id = f.take_jobs()[0].id;
        f.finish(id, ok(1));
        assert_eq!(f.redo().unwrap_err(), "nothing to redo");
    }

    #[test]
    fn a_half_finished_undo_puts_the_rest_back() {
        let mut f = ops();
        f.start(spec("many", 3)).unwrap();
        let id = f.take_jobs()[0].id;
        f.finish(id, ok(3));
        f.undo().unwrap();
        let job = f.take_jobs().pop().unwrap();
        let mut outcome = ok(1);
        outcome.failure = Some(Failure::Error("restore failed".into()));
        f.finish(job.id, outcome);
        f.undo().unwrap();
        assert_eq!(f.take_jobs()[0].ops.len(), 2, "two steps are still to undo");
        assert!(f.running().is_some());
    }

    #[test]
    fn a_finished_move_empties_the_register() {
        let mut f = ops();
        f.set_register(vec!["/a".into()], PasteMode::Cut);
        let mut s = spec("move", 1);
        s.clears_register = true;
        f.start(s).unwrap();
        let id = f.take_jobs()[0].id;
        f.finish(id, ok(1));
        assert!(f.register().is_none());
        f.set_register(vec!["/a".into()], PasteMode::Copy);
        f.start(spec("copy", 1)).unwrap();
        let id = f.take_jobs()[0].id;
        f.finish(id, ok(1));
        assert!(f.register().is_some(), "copies can be pasted again");
    }

    #[test]
    fn selection_toggles_and_clears() {
        let mut f = ops();
        f.toggle_selected("/a".into());
        f.toggle_selected("/b".into());
        f.toggle_selected("/a".into());
        assert_eq!(f.selection().iter().collect::<Vec<_>>(), [Path::new("/b")]);
        f.clear_selection();
        assert!(f.selection().is_empty());
    }

    fn paste(
        names: &[&str],
        existing: &'static [&'static str],
    ) -> (PasteFlow, impl Fn(&Path) -> bool) {
        let sources: Vec<PathBuf> = names
            .iter()
            .map(|n| PathBuf::from(format!("/src/{n}")))
            .collect();
        let exists = move |p: &Path| existing.contains(&p.to_str().unwrap());
        let items = plan_paste(&sources, Path::new("/dst"), PasteMode::Copy, &exists);
        (PasteFlow::new(items, PasteMode::Copy), exists)
    }

    #[test]
    fn a_paste_without_conflicts_is_ready_at_once() {
        let (mut flow, exists) = paste(&["a", "b"], &[]);
        let Step::Ready(ops) = flow.advance(&exists) else {
            panic!("expected ready")
        };
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn each_conflict_is_asked_about_in_order() {
        let (mut flow, exists) = paste(&["a", "b", "c"], &["/dst/a", "/dst/c"]);
        let Step::Ask(item) = flow.advance(&exists) else {
            panic!("expected a question")
        };
        assert_eq!(item.to, Path::new("/dst/a"));
        let Step::Ask(item) = flow.choose(Choice::Overwrite, false, &exists) else {
            panic!("expected a second question")
        };
        assert_eq!(item.to, Path::new("/dst/c"));
        let Step::Ready(ops) = flow.choose(Choice::Skip, false, &exists) else {
            panic!("expected ready")
        };
        assert_eq!(
            ops.len(),
            3,
            "trash and copy for a, copy for b, nothing for c"
        );
        assert!(matches!(ops[0], Op::Trash { .. }));
    }

    #[test]
    fn an_answer_for_all_covers_the_remaining_conflicts() {
        let (mut flow, exists) = paste(&["a", "b", "c"], &["/dst/a", "/dst/b", "/dst/c"]);
        assert!(matches!(flow.advance(&exists), Step::Ask(_)));
        let Step::Ready(ops) = flow.choose(Choice::KeepBoth, true, &exists) else {
            panic!("one answer should settle everything")
        };
        let targets: Vec<_> = ops
            .iter()
            .map(|op| match op {
                Op::Copy { to, .. } => to.to_str().unwrap().to_string(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(targets, ["/dst/a (1)", "/dst/b (1)", "/dst/c (1)"]);
    }

    #[test]
    fn a_problem_item_stops_the_whole_paste() {
        let sources = vec![PathBuf::from("/dst"), PathBuf::from("/src/ok")];
        let items = plan_paste(&sources, Path::new("/dst/inner"), PasteMode::Copy, &|_| {
            false
        });
        let mut flow = PasteFlow::new(items, PasteMode::Copy);
        let Step::Failed(message) = flow.advance(&|_| false) else {
            panic!("expected failure")
        };
        assert!(message.contains("into itself"), "{message}");
    }

    #[test]
    fn a_whole_delete_then_undo_through_the_journal_restores_the_files() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = Arc::new(FakeTrash::new(&tmp.path().join("trash")));
        let file = tmp.path().join("doc.txt");
        std::fs::write(&file, "text").unwrap();
        let mut f = FileOps::new(trash.clone());
        f.start(JobSpec {
            label: "delete".into(),
            success: "deleted".into(),
            ops: vec![Op::Trash { path: file.clone() }],
            focus_on: None,
            clears_register: false,
        })
        .unwrap();
        let cancel = AtomicBool::new(false);
        let run = |job: Job| {
            let ctx = crate::ops::Ctx {
                trash: &*trash,
                cancel: &cancel,
                progress: &|_| {},
            };
            (job.id, run_job(&job.ops, &ctx, |_| {}))
        };
        let (id, outcome) = run(f.take_jobs().pop().unwrap());
        f.finish(id, outcome);
        assert!(!file.exists());
        f.undo().unwrap();
        let (id, outcome) = run(f.take_jobs().pop().unwrap());
        assert_eq!(f.finish(id, outcome).unwrap().message, "undid delete");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "text");
    }
}
