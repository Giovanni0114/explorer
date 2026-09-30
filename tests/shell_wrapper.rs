use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use tx::shell::{Shell, init_script};

/// A stand-in `tx` binary that "quits" in the directory named by `$FAKE_TX_DEST`.
fn fake_tx(bin_dir: &Path) {
    let script = "#!/bin/sh\n\
        [ \"$1\" = --cwd-file ] || exit 64\n\
        [ -n \"$FAKE_TX_DEST\" ] && printf '%s' \"$FAKE_TX_DEST\" > \"$2\"\n\
        exit ${FAKE_TX_EXIT:-0}\n";
    let path = bin_dir.join("tx");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_in(shell: &str, kind: Shell, dest: Option<&Path>, exit: u8) -> (String, i32) {
    let tmp = tempfile::tempdir().unwrap();
    // macOS temp dirs sit behind /var -> /private/var, which `pwd` resolves.
    let real = tmp.path().canonicalize().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fake_tx(&bin);
    let start = tmp.path().join("start dir");
    fs::create_dir(&start).unwrap();
    let init = tmp.path().join("init");
    fs::write(&init, init_script(kind)).unwrap();

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut cmd = Command::new(shell);
    cmd.current_dir(&start)
        .env("PATH", path)
        .env("FAKE_TX_EXIT", exit.to_string())
        .arg("-c")
        .arg(format!(
            ". '{}'; tx; echo \"rc=$? pwd=$PWD\"",
            init.display()
        ));
    if let Some(dest) = dest {
        cmd.env("FAKE_TX_DEST", dest);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = String::from_utf8(out.stdout).unwrap();
    let line = line.trim().to_string();
    let rc = line
        .split("rc=")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let pwd = line.split("pwd=").nth(1).unwrap().to_string();
    (pwd.replace(real.to_str().unwrap(), "<tmp>"), rc)
}

fn wrapper_case(shell: &str, kind: Shell) {
    let dest_root = tempfile::tempdir().unwrap();
    let dest = dest_root.path().join("where I quit");
    fs::create_dir(&dest).unwrap();

    let (pwd, rc) = run_in(shell, kind, Some(&dest), 0);
    assert_eq!(
        pwd,
        dest.to_str().unwrap(),
        "{shell}: cd to the recorded directory, spaces included"
    );
    assert_eq!(rc, 0);

    let (pwd, _) = run_in(shell, kind, None, 0);
    assert_eq!(
        pwd, "<tmp>/start dir",
        "{shell}: no file content means stay put"
    );

    let (_, rc) = run_in(shell, kind, None, 7);
    assert_eq!(rc, 7, "{shell}: the exit status of tx is passed through");
}

#[test]
fn bash_wrapper_changes_directory() {
    wrapper_case("bash", Shell::Bash);
}

#[test]
fn zsh_wrapper_changes_directory() {
    wrapper_case("zsh", Shell::Zsh);
}
