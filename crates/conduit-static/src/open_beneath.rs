//! Open a file below a root without ever following a symlink (issue #557, CWE-59).
//!
//! Checking a path with `symlink_metadata` and opening it afterwards leaves a window in which
//! someone with write access inside the served directory can swap a directory for a symlink.
//! Here the walk and the open are one step: the root is opened once, then every component is
//! opened relative to the previous directory descriptor (safe `rustix` wrappers, no `unsafe` here)
//! with `O_NOFOLLOW` (and `O_DIRECTORY` for the intermediate ones), so the kernel refuses a symlink
//! at any level below the root, whatever happens to the path meanwhile. The root path itself is
//! resolved normally (a symlinked root is a supported deployment); see "Symlinks" in
//! `docs/configuration.md`. This is the same guarantee as Linux `openat2(RESOLVE_BENEATH |
//! RESOLVE_NO_SYMLINKS)`, built from `openat` so it also holds on macOS and the BSDs.
//!
//! Windows has its own walk (`windows.rs`, issue #600): `NtCreateFile` per component, relative to
//! the handle of the directory above it, with `FILE_OPEN_REPARSE_POINT`, so a symlink or directory
//! junction is refused at any level for the same reason. Any other platform keeps a plain open and
//! the check-then-open in `handler.rs`; see "Symlinks" in `docs/configuration.md`.

#[cfg(unix)]
use std::io;
#[cfg(not(windows))]
use std::path::{Component, Path};

#[cfg(windows)]
mod windows;
#[cfg(all(windows, test))]
pub use windows::LINK_REFUSED;
#[cfg(windows)]
pub use windows::{is_link_refused, name_is_refused, open_beneath};

/// Open `root`/`rel` read-only. `rel` must consist of normal components only (no `..`, no root).
/// The root itself may be a symlink (the operator chose it); nothing below it may be.
#[cfg(unix)]
pub fn open_beneath(root: &Path, rel: &Path) -> io::Result<std::fs::File> {
    use rustix::fs::{openat, Mode, OFlags, CWD};

    let mut names = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(n) => names.push(n),
            _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
        }
    }
    let Some((last, dirs)) = names.split_last() else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };

    let flags = OFlags::RDONLY | OFlags::CLOEXEC;
    let mut cur = openat(CWD, root, flags | OFlags::DIRECTORY, Mode::empty())?;
    for name in dirs {
        cur = openat(
            &cur,
            *name,
            flags | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
    }
    // `O_NONBLOCK` on the last component only: opening a FIFO that someone swapped in for a regular file after
    // `find_file` looked at it would otherwise wait for a writer and pin a blocking-pool thread (issue #591).
    // It does nothing to a regular file; the caller refuses what is not one (`open_served` checks the opened
    // handle, so a FIFO is a miss).
    let fd = openat(
        &cur,
        *last,
        flags | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    Ok(std::fs::File::from(fd))
}

/// Fallback for a platform with neither `openat` nor `NtCreateFile`: reject components that are not plain names,
/// then a plain open (the window between check and open remains; see "Symlinks" in `docs/configuration.md`).
#[cfg(not(any(unix, windows)))]
pub fn open_beneath(root: &Path, rel: &Path) -> std::io::Result<std::fs::File> {
    for c in rel.components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
    }
    std::fs::File::open(root.join(rel))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;

    fn read(mut f: std::fs::File) -> String {
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn opens_nested_regular_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/b/f.txt"), "hi").unwrap();
        let f = open_beneath(root.path(), Path::new("a/b/f.txt")).unwrap();
        assert_eq!(read(f), "hi");
    }

    #[test]
    fn refuses_a_symlink_at_any_level() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("dir")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("file"))
            .unwrap();
        // directory symlink (ELOOP/ENOTDIR depending on the OS) and final-component symlink
        assert!(open_beneath(root.path(), Path::new("dir/secret")).is_err());
        let err = open_beneath(root.path(), Path::new("file")).unwrap_err();
        assert_eq!(
            err.raw_os_error(),
            Some(rustix::io::Errno::LOOP.raw_os_error())
        );
    }

    #[test]
    fn rejects_non_normal_components() {
        let root = tempfile::tempdir().unwrap();
        for bad in ["../x", "/etc/passwd", "a/../b", ""] {
            assert!(open_beneath(root.path(), Path::new(bad)).is_err(), "{bad}");
        }
    }

    /// Issue #591: opening a FIFO must not wait for a writer (a blocking-pool thread would be pinned for as long as
    /// nobody opens the other end); it opens at once, and is not a regular file, so the caller turns it away.
    #[test]
    fn a_fifo_opens_at_once_instead_of_waiting_for_a_writer() {
        use rustix::fs::{mknodat, FileType, Mode, CWD};
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("pipe");
        mknodat(CWD, &fifo, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = root.path().to_owned();
        // A thread, so that a regression fails the test (a blocked open) instead of hanging it.
        std::thread::spawn(move || {
            let opened = open_beneath(&dir, Path::new("pipe"));
            let _ = tx.send(opened.map(|f| f.metadata().map(|m| m.is_file())));
        });
        let opened = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("opening a FIFO must not block");
        assert!(!opened.unwrap().unwrap(), "a FIFO is not a regular file");
    }

    /// A directory swapped for a symlink while opens are running must never leak outside bytes.
    #[test]
    fn swapping_a_directory_for_a_symlink_never_leaks_outside_bytes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("f"), "OUTSIDE").unwrap();
        let dir = root.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("f"), "inside").unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let swapper = {
            let (stop, dir, target, real) = (
                stop.clone(),
                dir.clone(),
                outside.path().to_owned(),
                root.path().join("real"),
            );
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = std::fs::rename(&dir, &real);
                    let _ = std::os::unix::fs::symlink(&target, &dir);
                    let _ = std::fs::remove_file(&dir);
                    let _ = std::fs::rename(&real, &dir);
                }
            })
        };
        for _ in 0..20_000 {
            if let Ok(f) = open_beneath(root.path(), Path::new("d/f")) {
                assert_ne!(read(f), "OUTSIDE");
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        swapper.join().unwrap();
    }
}
