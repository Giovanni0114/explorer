//! The real freedesktop trash, pointed at a temporary data directory.

use std::fs;

use tx::ops::{SystemTrash, Trasher};

#[test]
fn the_real_trash_round_trips_and_never_clobbers_on_restore() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: this test binary contains only this test, so no other thread touches the environment.
    unsafe { std::env::set_var("XDG_DATA_HOME", data.path()) };
    let work = tempfile::tempdir().unwrap();

    let file = work.path().join("note.txt");
    fs::write(&file, "keep me").unwrap();
    let reference = SystemTrash.trash(&file).unwrap();
    assert!(!file.exists(), "the file left its folder");
    assert_eq!(reference.original, file);
    SystemTrash.restore(&reference).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "keep me");

    let clash = work.path().join("clash.txt");
    fs::write(&clash, "first").unwrap();
    let reference = SystemTrash.trash(&clash).unwrap();
    fs::write(&clash, "second").unwrap();
    assert!(SystemTrash.restore(&reference).is_err());
    assert_eq!(fs::read_to_string(&clash).unwrap(), "second");
}
