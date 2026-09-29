use std::{
    collections::BTreeMap,
    env,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use serde::Deserialize;

const DEFAULT_EDITORS: [&str; 2] = ["nvim", "nano"];

#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    /// Editor commands in order of preference; the first one found on `PATH` is used.
    /// An entry may carry arguments, like `"code --wait"`.
    pub editors: Vec<String>,
    /// `[keys]` entries, `"gg" = "first"`; an empty command unbinds the key.
    pub keys: BTreeMap<String, String>,
    /// Show dotfiles from the start.
    pub show_hidden: bool,
    /// Percent of the screen width the folder columns may use. The file preview gets the rest.
    pub tree_width: u8,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            editors: DEFAULT_EDITORS.map(String::from).to_vec(),
            keys: BTreeMap::new(),
            show_hidden: false,
            tree_width: 50,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    editor: Option<OneOrMany>,
    keys: Option<BTreeMap<String, String>>,
    show_hidden: Option<bool>,
    tree_width: Option<u8>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl Config {
    /// Reads `$XDG_CONFIG_HOME/tx/config.toml` (or `~/.config/tx/config.toml`).
    /// A missing file gives the defaults; a broken one gives the defaults and a message.
    pub fn load() -> (Config, Option<String>) {
        let Some(path) = config_path() else {
            return (Config::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match Config::parse(&text) {
                Ok(config) => (config, None),
                Err(e) => (
                    Config::default(),
                    Some(format!("{}: {}", path.display(), e.message())),
                ),
            },
            Err(_) => (Config::default(), None),
        }
    }

    pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
        let raw: Raw = toml::from_str(text)?;
        let editors = match raw.editor {
            None => Config::default().editors,
            Some(OneOrMany::One(one)) => vec![one],
            Some(OneOrMany::Many(many)) => many,
        };
        Ok(Config {
            editors,
            keys: raw.keys.unwrap_or_default(),
            show_hidden: raw.show_hidden.unwrap_or(false),
            tree_width: match raw.tree_width {
                None => 50,
                Some(percent @ 20..=100) => percent,
                Some(_) => {
                    return Err(serde::de::Error::custom(
                        "tree_width is a percent of the screen, from 20 to 100",
                    ));
                }
            },
        })
    }

    /// The command line prefix of the first configured editor that `on_path` finds.
    pub fn resolve_editor(&self, on_path: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
        self.editors
            .iter()
            .map(|e| e.split_whitespace().map(String::from).collect::<Vec<_>>())
            .find(|argv| argv.first().is_some_and(|program| on_path(program)))
    }
}

fn config_path() -> Option<PathBuf> {
    let base = match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("tx").join("config.toml"))
}

/// Whether `program` names an executable, either as a path or somewhere on `PATH`.
pub fn on_path(program: &str) -> bool {
    let is_executable = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return is_executable(Path::new(program));
    }
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| is_executable(&dir.join(program))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(config: &Config, installed: &[&str]) -> Option<Vec<String>> {
        config.resolve_editor(&|p| installed.contains(&p))
    }

    #[test]
    fn default_prefers_neovim_then_nano() {
        let config = Config::default();
        assert_eq!(
            resolve(&config, &["nvim", "nano"]),
            Some(vec!["nvim".into()])
        );
        assert_eq!(resolve(&config, &["nano"]), Some(vec!["nano".into()]));
        assert_eq!(resolve(&config, &["vi"]), None);
    }

    #[test]
    fn editor_can_be_a_string_or_a_list_and_may_carry_arguments() {
        let one = Config::parse(r#"editor = "code --wait""#).unwrap();
        assert_eq!(
            resolve(&one, &["code"]),
            Some(vec!["code".into(), "--wait".into()])
        );
        let many = Config::parse(r#"editor = ["hx", "micro"]"#).unwrap();
        assert_eq!(
            resolve(&many, &["micro", "nvim"]),
            Some(vec!["micro".into()])
        );
    }

    #[test]
    fn keys_table_is_read() {
        let config = Config::parse("[keys]\n\"gg\" = \"first\"\nq = \"none\"").unwrap();
        assert_eq!(config.keys["gg"], "first");
        assert_eq!(config.keys["q"], "none");
        assert_eq!(config.editors, Config::default().editors);
    }

    #[test]
    fn tree_width_is_a_percent_between_20_and_100() {
        assert_eq!(Config::parse("").unwrap().tree_width, 50);
        assert_eq!(Config::parse("tree_width = 35").unwrap().tree_width, 35);
        assert!(Config::parse("tree_width = 10").is_err());
        assert!(Config::parse("tree_width = 150").is_err());
        assert!(Config::parse("tree_width = 300").is_err());
    }

    #[test]
    fn show_hidden_is_read() {
        assert!(Config::parse("show_hidden = true").unwrap().show_hidden);
        assert!(!Config::parse("").unwrap().show_hidden);
        assert!(Config::parse("show_hidden = 'yes'").is_err());
    }

    #[test]
    fn an_empty_file_keeps_the_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn unknown_keys_and_wrong_types_are_reported() {
        assert!(Config::parse("editr = 'vim'").is_err());
        assert!(Config::parse("editor = 3").is_err());
    }

    #[test]
    fn on_path_finds_real_executables_only() {
        assert!(on_path("sh"));
        assert!(!on_path("definitely-not-a-program-tx"));
        assert!(on_path("/bin/sh"));
        assert!(!on_path("/etc/passwd"));
    }
}
