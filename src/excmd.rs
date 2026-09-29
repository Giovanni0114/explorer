//! The `:` command line.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ex {
    Quit,
    Abort,
    Help,
    Cd(PathBuf),
    /// `Some` sets dotfile visibility, `None` toggles it.
    SetHidden(Option<bool>),
    Marks,
    Mkdir(String),
    Touch(String),
    /// Permission bits from an octal argument.
    Chmod(u32),
    Undo,
    Redo,
    /// Says how pictures are drawn and why.
    Images,
}

/// Parses one command line. `cwd` resolves relative paths and `home` expands a leading `~`.
pub fn parse(line: &str, cwd: &Path, home: Option<&Path>) -> Result<Ex, String> {
    let line = line.trim();
    let (name, arg) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(n, a)| (n, a.trim()));
    match name {
        "q" | "quit" | "qa" | "wq" | "x" => Ok(Ex::Quit),
        "q!" | "cq" | "qa!" => Ok(Ex::Abort),
        "h" | "help" => Ok(Ex::Help),
        "cd" => Ok(Ex::Cd(resolve(arg, cwd, home))),
        "marks" => Ok(Ex::Marks),
        "undo" => Ok(Ex::Undo),
        "images" => Ok(Ex::Images),
        "redo" => Ok(Ex::Redo),
        "mkdir" if !arg.is_empty() => Ok(Ex::Mkdir(arg.to_string())),
        "touch" if !arg.is_empty() => Ok(Ex::Touch(arg.to_string())),
        "mkdir" | "touch" => Err(format!("{name} needs a name")),
        "chmod" => match u32::from_str_radix(arg, 8) {
            Ok(mode) if mode <= 0o7777 => Ok(Ex::Chmod(mode)),
            _ => Err("chmod needs an octal mode such as 644 or 755".into()),
        },
        "set" => match arg {
            "hidden" => Ok(Ex::SetHidden(Some(true))),
            "nohidden" => Ok(Ex::SetHidden(Some(false))),
            "hidden!" | "invhidden" => Ok(Ex::SetHidden(None)),
            "" => Err("set what? try hidden, nohidden or hidden!".into()),
            other => Err(format!("unknown option: {other}")),
        },
        "" => Err("empty command".into()),
        other => Err(format!("unknown command: {other}")),
    }
}

fn resolve(arg: &str, cwd: &Path, home: Option<&Path>) -> PathBuf {
    let expanded = match (arg, home) {
        ("" | "~", Some(home)) => home.to_path_buf(),
        (path, Some(home)) if path.starts_with("~/") => home.join(&path[2..]),
        (path, _) => PathBuf::from(path),
    };
    cwd.join(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Result<Ex, String> {
        parse(line, Path::new("/work/here"), Some(Path::new("/home/me")))
    }

    #[test]
    fn quit_variants() {
        for line in ["q", "quit", "  q  ", "wq"] {
            assert_eq!(p(line), Ok(Ex::Quit), "{line}");
        }
        assert_eq!(p("q!"), Ok(Ex::Abort));
        assert_eq!(p("cq"), Ok(Ex::Abort));
    }

    #[test]
    fn cd_resolves_relative_absolute_and_home_paths() {
        assert_eq!(p("cd sub/dir"), Ok(Ex::Cd("/work/here/sub/dir".into())));
        assert_eq!(p("cd /etc"), Ok(Ex::Cd("/etc".into())));
        assert_eq!(p("cd ~/notes"), Ok(Ex::Cd("/home/me/notes".into())));
        assert_eq!(p("cd ~"), Ok(Ex::Cd("/home/me".into())));
        assert_eq!(p("cd"), Ok(Ex::Cd("/home/me".into())));
        assert_eq!(
            p("cd my dir"),
            Ok(Ex::Cd("/work/here/my dir".into())),
            "spaces stay in the path"
        );
        assert_eq!(p("cd .."), Ok(Ex::Cd("/work/here/..".into())));
    }

    #[test]
    fn set_controls_dotfile_visibility() {
        assert_eq!(p("set hidden"), Ok(Ex::SetHidden(Some(true))));
        assert_eq!(p("set nohidden"), Ok(Ex::SetHidden(Some(false))));
        assert_eq!(p("set hidden!"), Ok(Ex::SetHidden(None)));
        assert_eq!(p("set wrap"), Err("unknown option: wrap".into()));
        assert!(p("set").is_err());
        assert_eq!(p("marks"), Ok(Ex::Marks));
    }

    #[test]
    fn file_commands_take_names_and_modes() {
        assert_eq!(p("mkdir new dir"), Ok(Ex::Mkdir("new dir".into())));
        assert_eq!(p("touch a.txt"), Ok(Ex::Touch("a.txt".into())));
        assert!(p("mkdir").is_err());
        assert!(p("touch   ").is_err());
        assert_eq!(p("chmod 644"), Ok(Ex::Chmod(0o644)));
        assert_eq!(p("chmod 4755"), Ok(Ex::Chmod(0o4755)));
        assert!(p("chmod 999").is_err());
        assert!(p("chmod rwx").is_err());
        assert!(p("chmod 77777").is_err());
        assert_eq!((p("undo"), p("redo")), (Ok(Ex::Undo), Ok(Ex::Redo)));
    }

    #[test]
    fn unknown_and_empty_commands_are_errors() {
        assert_eq!(
            p("frobnicate now"),
            Err("unknown command: frobnicate".into())
        );
        assert!(p("").is_err());
        assert_eq!(p("help"), Ok(Ex::Help));
    }
}
