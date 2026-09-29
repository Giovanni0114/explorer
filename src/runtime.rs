//! Event loop. Input, directory listings and filesystem changes each arrive on their own
//! thread and meet here as messages, so the UI never blocks on disk.

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::{
    DefaultTerminal,
    crossterm::{
        event::{self, Event, KeyEventKind},
        execute,
        terminal::{EnterAlternateScreen, enable_raw_mode},
    },
};

use crate::{
    actions::{self, Opener},
    app::{App, Exit},
    config::Config,
    fsread,
    keys::Key,
    model::{Effect, Entry, LoadRequest, PreviewRequest},
    preview::{self, Content},
    render,
};

const LOAD_WORKERS: usize = 4;
const PREVIEW_WORKERS: usize = 2;
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(50);
/// How often to redraw while a listing is slow, so "(loading…)" can appear.
const LOADING_TICK: Duration = Duration::from_millis(100);

enum Msg {
    Input(Event),
    Loaded(PathBuf, io::Result<Vec<Entry>>),
    Previewed(PreviewRequest, io::Result<Content>),
    FsChange(Vec<PathBuf>),
}

pub fn run(terminal: &mut DefaultTerminal, app: &mut App, config: &Config) -> io::Result<Exit> {
    let (tx, rx) = mpsc::channel();
    let gate = Arc::new(InputGate::default());
    spawn_input(tx.clone(), Arc::clone(&gate));
    let loader = spawn_loaders(tx.clone());
    let previewer = spawn_previewers(tx.clone());
    let mut watcher = DirWatcher::new(tx);
    let mut dirty: HashSet<PathBuf> = HashSet::new();
    let mut flush_at: Option<Instant> = None;

    loop {
        if flush_at.is_some_and(|t| t <= Instant::now()) {
            flush_at = None;
            for dir in dirty.drain() {
                app.tree_mut().reload(&dir);
            }
        }
        for request in app.tree_mut().take_requests() {
            let _ = loader.send(request);
        }
        for request in app.tree_mut().take_preview_requests() {
            let _ = previewer.send(request);
        }
        watcher.sync(app.tree().dirs());
        app.set_viewport(terminal.size()?.height.saturating_sub(2));
        terminal.draw(|frame| render::render(app, frame.area(), frame.buffer_mut()))?;

        let wait = [
            flush_at.map(|t| t.saturating_duration_since(Instant::now())),
            app.tree().is_loading().then_some(LOADING_TICK),
        ]
        .into_iter()
        .flatten()
        .min();
        let first = match wait {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(msg) => Some(msg),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(Exit::Abort),
            },
            None => match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => return Ok(Exit::Abort),
            },
        };
        for msg in first.into_iter().chain(rx.try_iter().collect::<Vec<_>>()) {
            match msg {
                Msg::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    let response = app.press(Key::from_event(key));
                    if let Some(exit) = response.exit {
                        return Ok(exit);
                    }
                    if let Some(Effect::Open(path)) = response.effect {
                        open(terminal, &gate, config, app, &path);
                    }
                }
                Msg::Input(_) => {}
                Msg::Loaded(dir, result) => app.finish_load(&dir, result),
                Msg::Previewed(request, result) => app.tree_mut().finish_preview(&request, result),
                Msg::FsChange(paths) => {
                    for path in paths {
                        for candidate in [Some(path.as_path()), path.parent()].into_iter().flatten()
                        {
                            if watcher.watches(candidate) {
                                dirty.insert(candidate.to_path_buf());
                            }
                        }
                    }
                    if !dirty.is_empty() && flush_at.is_none() {
                        flush_at = Some(Instant::now() + CHANGE_DEBOUNCE);
                    }
                }
            }
        }
    }
}

fn open(
    terminal: &mut DefaultTerminal,
    gate: &InputGate,
    config: &Config,
    app: &mut App,
    path: &Path,
) {
    let opener = match actions::opener_for(path, config) {
        Ok(opener) => opener,
        Err(message) => return app.message = Some(message),
    };
    let outcome = match opener {
        Opener::Desktop(command) => actions::spawn_detached(command),
        Opener::Editor(mut command) => {
            gate.pause();
            ratatui::restore();
            let status = command.status();
            let resumed =
                enable_raw_mode().and_then(|()| execute!(io::stdout(), EnterAlternateScreen));
            gate.resume();
            resumed
                .and_then(|()| terminal.clear())
                .and(status.map(|_| ()))
        }
    };
    if let Err(e) = outcome {
        app.message = Some(format!("{}: {e}", path.display()));
    }
}

/// Lets the input thread be parked while an editor owns the terminal, so it cannot steal keystrokes.
#[derive(Default)]
struct InputGate {
    paused: AtomicBool,
    parked: AtomicBool,
}

impl InputGate {
    fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        while !self.parked.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }
}

fn spawn_input(tx: Sender<Msg>, gate: Arc<InputGate>) {
    thread::spawn(move || {
        loop {
            if gate.paused.load(Ordering::SeqCst) {
                gate.parked.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            gate.parked.store(false, Ordering::SeqCst);
            match event::poll(Duration::from_millis(30)) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        if tx.send(Msg::Input(ev)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
}

fn spawn_loaders(msgs: Sender<Msg>) -> Sender<LoadRequest> {
    let (tx, rx) = mpsc::channel::<LoadRequest>();
    let rx: Arc<Mutex<Receiver<LoadRequest>>> = Arc::new(Mutex::new(rx));
    for _ in 0..LOAD_WORKERS {
        let (rx, msgs) = (Arc::clone(&rx), msgs.clone());
        thread::spawn(move || {
            loop {
                let request = match rx.lock().unwrap().recv() {
                    Ok(request) => request,
                    Err(_) => return,
                };
                let result =
                    fsread::read_dir(&request.dir, request.keep.as_deref(), request.hidden);
                if msgs.send(Msg::Loaded(request.dir, result)).is_err() {
                    return;
                }
            }
        });
    }
    tx
}

fn spawn_previewers(msgs: Sender<Msg>) -> Sender<PreviewRequest> {
    let (tx, rx) = mpsc::channel::<PreviewRequest>();
    let rx: Arc<Mutex<Receiver<PreviewRequest>>> = Arc::new(Mutex::new(rx));
    for _ in 0..PREVIEW_WORKERS {
        let (rx, msgs) = (Arc::clone(&rx), msgs.clone());
        thread::spawn(move || {
            loop {
                let request = match rx.lock().unwrap().recv() {
                    Ok(request) => request,
                    Err(_) => return,
                };
                let result = preview::build(&request.path);
                if msgs.send(Msg::Previewed(request, result)).is_err() {
                    return;
                }
            }
        });
    }
    tx
}

/// inotify watches, one per directory on screen.
struct DirWatcher {
    inner: Option<RecommendedWatcher>,
    watched: HashSet<PathBuf>,
}

impl DirWatcher {
    fn new(tx: Sender<Msg>) -> DirWatcher {
        let inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && !matches!(event.kind, EventKind::Access(_))
            {
                let _ = tx.send(Msg::FsChange(event.paths));
            }
        })
        .ok();
        DirWatcher {
            inner,
            watched: HashSet::new(),
        }
    }

    fn watches(&self, dir: &Path) -> bool {
        self.watched.contains(dir)
    }

    fn sync<'a>(&mut self, wanted: impl Iterator<Item = &'a Path>) {
        let Some(watcher) = self.inner.as_mut() else {
            return;
        };
        let wanted: HashSet<PathBuf> = wanted.map(Path::to_path_buf).collect();
        for gone in self.watched.difference(&wanted) {
            let _ = watcher.unwatch(gone);
        }
        self.watched.retain(|d| wanted.contains(d));
        for new in wanted {
            if !self.watched.contains(&new)
                && watcher.watch(&new, RecursiveMode::NonRecursive).is_ok()
            {
                self.watched.insert(new);
            }
        }
    }
}
