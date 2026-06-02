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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.file = Some(BufWriter::new(file));
        inner.path = Some(path.to_owned());
        Ok(())
    }

    /// Close the current file and revert to stdout output.
    pub fn use_stdout(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.file = None;
        inner.path = None;
    }

    /// Path of the currently-open log file, or `None` if writing to stdout.
    pub fn current_path(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.path.clone()
    }

    /// Write a single log line (newline appended automatically).
    ///
    /// Errors are silently ignored — a logging failure must not abort a request.
    pub fn write_line(&self, line: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
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
