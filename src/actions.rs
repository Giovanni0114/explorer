use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
    thread,
};

use crate::{
    config::{Config, on_path},
    preview::looks_like_text,
};

const SNIFF_BYTES: usize = 8192;

/// Text if the first bytes contain no NUL and are valid UTF-8 (a multibyte character cut off at the end is fine).
pub fn is_text(path: &Path) -> io::Result<bool> {
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    File::open(path)?
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut head)?;
    Ok(looks_like_text(&head, head.len() == SNIFF_BYTES))
}

pub enum Opener {
    /// Runs in the terminal, so the caller must suspend the TUI around it.
    Editor(Command),
    /// Detached desktop opener.
    Desktop(Command),
}

/// Text files go to the configured editor, everything else to `xdg-open`.
pub fn opener_for(path: &Path, config: &Config) -> Result<Opener, String> {
    let text = is_text(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if text {
        let argv = config
            .resolve_editor(&on_path)
            .ok_or_else(|| format!("no editor found, tried: {}", config.editors.join(", ")))?;
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).arg(path);
        return Ok(Opener::Editor(command));
    }
    if !on_path("xdg-open") {
        return Err("xdg-open not found".into());
    }
    let mut command = Command::new("xdg-open");
    command
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(Opener::Desktop(command))
}

/// Starts `command` without waiting; a thread reaps it so no zombie is left.
pub fn spawn_detached(mut command: Command) -> io::Result<()> {
    let mut child = command.spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        fs::write(&path, bytes).unwrap();
        (tmp, path)
    }

    #[test]
    fn utf8_text_and_empty_files_are_text() {
        assert!(is_text(&write("zażółć gęślą jaźń\n".as_bytes()).1).unwrap());
        assert!(is_text(&write(b"").1).unwrap());
    }

    #[test]
    fn nul_bytes_and_invalid_utf8_are_binary() {
        assert!(!is_text(&write(b"ELF\0\x01\x02").1).unwrap());
        assert!(!is_text(&write(&[0xff, 0xfe, b'a']).1).unwrap());
    }

    #[test]
    fn a_multibyte_character_cut_at_the_sniff_boundary_is_still_text() {
        let mut bytes = vec![b'a'; SNIFF_BYTES - 1];
        bytes.extend("ż".as_bytes());
        assert!(is_text(&write(&bytes).1).unwrap());
    }

    #[test]
    fn text_files_use_the_configured_editor() {
        let (_tmp, path) = write(b"hello");
        let config = Config {
            editors: vec!["sh -c".into()],
            ..Config::default()
        };
        let Ok(Opener::Editor(command)) = opener_for(&path, &config) else {
            panic!("expected an editor")
        };
        assert_eq!(command.get_program(), "sh");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-c", path.to_str().unwrap()]);
    }

    #[test]
    fn a_missing_editor_is_reported_with_what_was_tried() {
        let (_tmp, path) = write(b"hello");
        let config = Config {
            editors: vec!["no-such-editor-tx".into(), "nor-this".into()],
            ..Config::default()
        };
        let err = opener_for(&path, &config).err().unwrap();
        assert!(
            err.contains("no-such-editor-tx") && err.contains("nor-this"),
            "{err}"
        );
    }
}
