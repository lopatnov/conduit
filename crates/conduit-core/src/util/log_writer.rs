use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write as _};
use std::sync::Mutex;

/// Opens `path` for append (creating it if absent) without following a
/// symlink at the final path component — see `switch_file`'s doc comment
/// for the TOCTOU rationale (issue #280).
///
/// On Unix, `O_NOFOLLOW` makes the open syscall fail atomically (`ELOOP`)
/// when the final component is a symlink.
///
/// On Windows the same guarantee comes from `FILE_FLAG_OPEN_REPARSE_POINT`:
/// a symlink or directory junction at `path` is opened *itself* instead of
/// being followed, and the file type is then read from the opened handle
/// (`GetFileInformationByHandle`), not from the path, so nothing can be swapped
/// in between the check and the open (issue #600). A planted link is refused;
/// a regular file is opened as usual. As on Unix, only the final component is
/// covered — the log directory stays operator-controlled.
///
/// Any other platform has no equivalent: a plain open that follows a symlink.
fn open_no_follow(path: &str) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        refuse_link(&file)?;
        Ok(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new().create(true).append(true).open(path)
    }
}

/// Windows: error out when `file` is a symlink or directory junction (a name-surrogate reparse
/// point). The type comes from the open handle, so it cannot be raced.
#[cfg(windows)]
fn refuse_link(file: &File) -> io::Result<()> {
    if file.metadata()?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to open a symlink or junction as the access-log file",
        ));
    }
    Ok(())
}

/// Writes access-log lines to either stdout or a file.
///
/// Thread-safe: a single `LogWriter` is shared across all worker threads via
/// `Arc<LogWriter>`.  Use `switch_file` to atomically redirect output to a
/// new path (e.g. on config hot-reload).
pub struct LogWriter {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Open file writer; `None` means output goes to stdout.
    file: Option<BufWriter<File>>,
    /// Path of the currently-open file (for idempotency checks).
    path: Option<String>,
}

impl LogWriter {
    pub fn new() -> Self {
        LogWriter {
            inner: Mutex::new(Inner {
                file: None,
                path: None,
            }),
        }
    }

    /// Open (or re-open) the log file at `path`, replacing any current writer.
    ///
    /// The file is opened in append mode so existing content is preserved.
    ///
    /// # Security
    ///
    /// On Unix and Windows, refuses to open a symlink (on Windows also a
    /// junction), atomically. If an attacker can create a symlink at the
    /// configured log path (e.g., pointing to `/etc/passwd`), the proxy
    /// running as root would append log lines to system files. On any other
    /// platform the open is a plain one and follows such a symlink, so the log
    /// directory must be writable by trusted accounts only.
    ///
    /// Issue #280: an earlier version of this check used
    /// `symlink_metadata()` followed by a *separate* `OpenOptions::open()`
    /// call — two distinct syscalls, leaving a TOCTOU window where a local
    /// attacker able to write to the log directory could swap the final
    /// path component for a symlink between the check and the open, and
    /// `open()` would follow it. `O_NOFOLLOW` (Unix) folds the check into
    /// the open itself: the kernel fails the syscall atomically with
    /// `ELOOP` if the final component is a symlink, with no window between
    /// checking and opening. Same pattern already used for static-file
    /// serving, see `open_beneath` in the `conduit-static` crate.
    pub fn switch_file(&self, path: &str) -> io::Result<()> {
        let file = open_no_follow(path)?;
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.file = Some(BufWriter::new(file));
        inner.path = Some(path.to_owned());
        Ok(())
    }

    /// Close the current file and revert to stdout output.
    pub fn use_stdout(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.file = None;
        inner.path = None;
    }

    /// Path of the currently-open log file, or `None` if writing to stdout.
    pub fn current_path(&self) -> Option<String> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.path.clone()
    }

    /// Write a single log line (newline appended automatically).
    ///
    /// Errors are silently ignored — a logging failure must not abort a request.
    pub fn write_line(&self, line: &str) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ref mut w) = inner.file {
            let _ = writeln!(w, "{line}");
            // Flush every line so that log tailing works correctly.
            let _ = w.flush();
        } else {
            println!("{line}");
        }
    }
}

impl Default for LogWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_impl_matches_new() {
        let w = LogWriter::default();
        let inner = w.inner.lock().unwrap();
        assert!(
            inner.file.is_none(),
            "default writer should write to stdout"
        );
        assert!(inner.path.is_none());
    }

    #[test]
    fn new_writer_uses_stdout() {
        let w = LogWriter::new();
        let inner = w.inner.lock().unwrap();
        assert!(inner.file.is_none(), "new writer should write to stdout");
        assert!(inner.path.is_none());
    }

    #[test]
    fn write_line_to_stdout_does_not_panic() {
        let w = LogWriter::new();
        // Writing to stdout is fine in tests — the line just goes to test output.
        w.write_line("test log line");
    }

    #[test]
    fn switch_file_writes_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.log");
        let w = LogWriter::new();
        w.switch_file(path.to_str().unwrap()).expect("switch_file");
        w.write_line("hello from test");

        // Flush by switching back to stdout (drops the BufWriter).
        w.use_stdout();
        let content = std::fs::read_to_string(&path).expect("read log");
        assert!(content.contains("hello from test"));
    }

    #[test]
    fn switch_file_twice_keeps_last_path() {
        let dir = tempfile::tempdir().unwrap();
        let path1 = dir.path().join("log1.log");
        let path2 = dir.path().join("log2.log");
        let w = LogWriter::new();
        w.switch_file(path1.to_str().unwrap()).unwrap();
        w.switch_file(path2.to_str().unwrap()).unwrap();
        let inner = w.inner.lock().unwrap();
        assert_eq!(inner.path.as_deref(), Some(path2.to_str().unwrap()));
    }

    #[test]
    #[cfg(unix)]
    fn switch_to_symlink_returns_error() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.log");
        let link = dir.path().join("link.log");
        std::fs::write(&target, "").unwrap();
        symlink(&target, &link).unwrap();

        let w = LogWriter::new();
        let result = w.switch_file(link.to_str().unwrap());
        assert!(result.is_err(), "symlink should be rejected");
    }

    /// Windows: a file symlink planted at the log path is opened as the link itself and refused,
    /// and its target is left untouched. Creating one needs the symlink privilege (or Developer
    /// Mode), so this skips, with a message, on a machine that does not grant it; the check is
    /// also covered without a privilege by `refuse_link_flags_a_junction_but_not_a_plain_directory`.
    #[test]
    #[cfg(windows)]
    fn switch_to_a_file_symlink_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.log");
        std::fs::write(&target, "original\n").unwrap();

        let file_link = dir.path().join("link.log");
        match std::os::windows::fs::symlink_file(&target, &file_link) {
            Ok(()) => {
                let w = LogWriter::new();
                // The message, not just `is_err()`: only it proves the new check made the call.
                let err = w
                    .switch_file(file_link.to_str().unwrap())
                    .expect_err("a file symlink must be rejected");
                assert!(
                    err.to_string().contains("refusing to open a symlink"),
                    "unexpected error: {err}"
                );
                assert_eq!(
                    std::fs::read_to_string(&target).unwrap(),
                    "original\n",
                    "the symlink target must be untouched"
                );
            }
            Err(e) => {
                // CI (an elevated runner) must be able to, or the refusal goes untested.
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI must be able to create symlinks, or this test silently stops covering the refusal ({e})"
                );
                eprintln!("skipping the file-symlink half: cannot create one here ({e})");
            }
        }
    }

    /// The check itself, run against a junction without needing the symlink privilege: a handle
    /// opened on the junction (reparse point flag, so it is the link itself) is refused, a handle
    /// on the directory it points at is not.
    #[test]
    #[cfg(windows)]
    fn refuse_link_flags_a_junction_but_not_a_plain_directory() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let open_dir = |p: &std::path::Path| {
            OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(p)
                .unwrap()
        };

        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real_dir");
        std::fs::create_dir(&real_dir).unwrap();
        let junction = dir.path().join("junction");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&real_dir)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J failed: {made:?}");

        assert!(refuse_link(&open_dir(&junction)).is_err());
        assert!(refuse_link(&open_dir(&real_dir)).is_ok());
    }

    #[test]
    #[cfg(windows)]
    fn windows_open_keeps_appending_to_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.log");
        std::fs::write(&path, "first\n").unwrap();
        let w = LogWriter::new();
        w.switch_file(path.to_str().unwrap()).unwrap();
        w.write_line("second");
        w.use_stdout();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn use_stdout_resets_to_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.log");
        let w = LogWriter::new();
        w.switch_file(path.to_str().unwrap()).unwrap();
        w.use_stdout();
        let inner = w.inner.lock().unwrap();
        assert!(inner.file.is_none(), "use_stdout should clear file writer");
    }

    #[test]
    fn current_path_returns_none_when_using_stdout() {
        let w = LogWriter::new();
        assert!(w.current_path().is_none());
    }

    #[test]
    fn current_path_returns_path_when_file_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.log");
        let w = LogWriter::new();
        w.switch_file(path.to_str().unwrap()).unwrap();
        assert_eq!(w.current_path().as_deref(), Some(path.to_str().unwrap()));
    }

    #[test]
    fn current_path_returns_none_after_use_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.log");
        let w = LogWriter::new();
        w.switch_file(path.to_str().unwrap()).unwrap();
        w.use_stdout();
        assert!(w.current_path().is_none());
    }

    #[test]
    fn write_line_to_file_is_flushed() {
        // Verify that write_line flushes the buffer so subsequent reads see the data.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flushed.log");
        let w = LogWriter::new();
        w.switch_file(path.to_str().unwrap()).unwrap();
        w.write_line("flushed line");
        // Read back WITHOUT switching to stdout (testing that flush happened).
        let content = std::fs::read_to_string(&path).expect("file must be readable");
        assert!(
            content.contains("flushed line"),
            "written line must be flushed: {content:?}"
        );
    }
}
