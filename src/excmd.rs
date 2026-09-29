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
    fn unknown_and_empty_commands_are_errors() {
        assert_eq!(
            p("frobnicate now"),
            Err("unknown command: frobnicate".into())
        );
        assert!(p("").is_err());
        assert_eq!(p("help"), Ok(Ex::Help));
    }
}
