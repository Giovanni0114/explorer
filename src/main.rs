use std::{fs, io, os::unix::ffi::OsStrExt, path::PathBuf};

use clap::Parser;
use tx::{
    app::{App, Exit, Settings},
    config::Config,
    imageview::{Mode as ImageMode, Painter},
    keys::Keymap,
    marks::Marks,
    ops::SystemTrash,
    runtime,
    shell::{self, Shell},
    theme::Depth,
};

/// Tree file explorer: every directory you enter opens a new column to the right.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Directory to start in (default: the current directory)
    path: Option<PathBuf>,

    /// On quit, write the directory you ended in to FILE (used by the shell function)
    #[arg(long, value_name = "FILE")]
    cwd_file: Option<PathBuf>,

    /// Print a shell function that makes quitting tx change the shell's directory
    #[arg(long, value_name = "SHELL")]
    init: Option<Shell>,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    if let Some(shell) = cli.init {
        print!("{}", shell::init_script(shell));
        return Ok(());
    }
    let root = match cli.path {
        Some(path) => path.canonicalize()?,
        None => std::env::current_dir()?,
    };
    let (config, mut notice) = Config::load();
    let keymap = Keymap::with_overrides(&config.keys).unwrap_or_else(|error| {
        notice = Some(format!("config: {error}"));
        Keymap::default()
    });
    // Asking the terminal about graphics reads its answer from stdin, so it runs before anything else does.
    let depth = config
        .colors
        .unwrap_or_else(|| Depth::detect(|name| std::env::var(name).ok()));
    let images = if depth == Depth::None {
        ImageMode::Off
    } else {
        config.images
    };
    let painter = Painter::new(images);
    let settings = Settings {
        show_hidden: config.show_hidden,
        tree_width: config.tree_width,
        painter,
        depth,
        marks: Marks::open(Marks::default_file()),
        trash: std::sync::Arc::new(SystemTrash),
    };
    let mut app = App::with_settings(root, keymap, settings);
    app.message = notice;
    let mut terminal = ratatui::init();
    if config.mouse {
        ratatui::crossterm::execute!(io::stdout(), ratatui::crossterm::event::EnableMouseCapture)?;
        // ratatui's own panic hook restores the screen but not mouse reporting, which would keep flooding the shell.
        let restore_screen = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = ratatui::crossterm::execute!(
                io::stdout(),
                ratatui::crossterm::event::DisableMouseCapture
            );
            restore_screen(info);
        }));
    }
    let result = runtime::run(&mut terminal, &mut app, &config);
    let _ =
        ratatui::crossterm::execute!(io::stdout(), ratatui::crossterm::event::DisableMouseCapture);
    ratatui::restore();
    if let (Exit::Quit, Some(file)) = (result?, cli.cwd_file) {
        fs::write(file, app.tree().current_dir().as_os_str().as_bytes())?;
    }
    Ok(())
}
