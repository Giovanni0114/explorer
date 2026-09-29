use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

const POSIX: &str = r#"tx() {
  local tmp dir rc
  tmp="$(mktemp -t tx-cwd.XXXXXX)" || return
  command tx --cwd-file "$tmp" "$@"
  rc=$?
  if dir="$(<"$tmp")" && [ -n "$dir" ] && [ "$dir" != "$PWD" ]; then
    cd -- "$dir"
  fi
  rm -f -- "$tmp"
  return $rc
}
"#;

const FISH: &str = r#"function tx
    set -l tmp (mktemp -t tx-cwd.XXXXXX); or return
    command tx --cwd-file $tmp $argv
    set -l rc $status
    set -l dir (cat $tmp)
    if test -n "$dir"; and test "$dir" != "$PWD"
        cd -- $dir
    end
    rm -f -- $tmp
    return $rc
end
"#;

/// A `tx` shell function that runs the binary and then `cd`s to where you left it.
pub fn init_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash | Shell::Zsh => POSIX,
        Shell::Fish => FISH,
    }
}
