//! Puts yanked files on the desktop clipboard so a GUI file manager can paste them.

use std::{
    env,
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
};

use crate::config::on_path;

#[derive(Debug, PartialEq, Eq)]
pub struct Payload {
    pub mime: &'static str,
    pub text: String,
}

/// GNOME's Files only understands its own type, and everything else reads a plain URI list.
pub fn payload(paths: &[PathBuf], cut: bool, gnome: bool) -> Payload {
    let uris: Vec<String> = paths.iter().map(|p| uri(p)).collect();
    if gnome {
        let verb = if cut { "cut" } else { "copy" };
        return Payload {
            mime: "x-special/gnome-copied-files",
            text: format!("{verb}\n{}", uris.join("\n")),
        };
    }
    Payload {
        mime: "text/uri-list",
        text: uris.iter().map(|u| format!("{u}\r\n")).collect(),
    }
}

fn uri(path: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Hands the files to `wl-copy` or `xclip`, whichever fits the session.
pub fn copy_to_system(paths: &[PathBuf], cut: bool) -> io::Result<()> {
    let gnome = env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("GNOME"));
    let payload = payload(paths, cut, gnome);
    let mut command = if env::var_os("WAYLAND_DISPLAY").is_some() && on_path("wl-copy") {
        let mut c = Command::new("wl-copy");
        c.args(["--type", payload.mime]);
        c
    } else if on_path("xclip") {
        let mut c = Command::new("xclip");
        c.args(["-selection", "clipboard", "-t", payload.mime, "-i"]);
        c
    } else {
        return Err(io::Error::other("neither wl-copy nor xclip is installed"));
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(payload.text.as_bytes())?;
    }
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn plain_desktops_get_a_uri_list_with_crlf_lines() {
        let p = payload(&paths(&["/home/me/a.txt", "/tmp/b"]), false, false);
        assert_eq!(p.mime, "text/uri-list");
        assert_eq!(p.text, "file:///home/me/a.txt\r\nfile:///tmp/b\r\n");
    }

    #[test]
    fn gnome_gets_its_own_type_with_the_verb_first() {
        let copy = payload(&paths(&["/a", "/b"]), false, true);
        assert_eq!(copy.mime, "x-special/gnome-copied-files");
        assert_eq!(copy.text, "copy\nfile:///a\nfile:///b");
        assert!(
            payload(&paths(&["/a"]), true, true)
                .text
                .starts_with("cut\n")
        );
    }

    #[test]
    fn special_characters_and_non_utf8_names_are_percent_encoded() {
        let p = payload(&paths(&["/d/my file #1é.txt"]), false, false);
        assert_eq!(p.text, "file:///d/my%20file%20%231%C3%A9.txt\r\n");
        let raw = PathBuf::from(std::ffi::OsStr::from_bytes(b"/d/\xff.bin"));
        assert_eq!(payload(&[raw], false, false).text, "file:///d/%FF.bin\r\n");
    }
}
