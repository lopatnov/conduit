use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write as _};
use std::sync::Mutex;

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
    /// Refuses to open symlinks.  If an attacker can create a symlink at the
    /// configured log path (e.g., pointing to `/etc/passwd`), the proxy
    /// running as root would overwrite system files.  Checking for symlinks
    /// before opening eliminates this local privilege-escalation vector.
    pub fn switch_file(&self, path: &str) -> io::Result<()> {
        // Reject symlinks — follow-on writes would go to the link target,
        // enabling a local privilege-escalation attack on systems where the
        // proxy runs with elevated permissions.
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "log file '{path}' is a symbolic link — refusing to open \
                         (symlink attack prevention)"
                    ),
                ));
            }
            _ => {} // File does not exist yet (will be created) or is a regular file.
        }

        let file = OpenOptions::new().create(true).append(true).open(path)?;
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
        assert!(inner.file.is_none(), "default writer should write to stdout");
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
