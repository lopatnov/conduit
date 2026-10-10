//! Owner-only file writes for ACME secrets (TLS private key, account
//! credentials).
use std::path::Path;

/// Writes `contents` to `path` with owner-only (0600) permissions on Unix,
/// covering both initial creation and the overwrite case.
///
/// Issue #278: the TLS private key and ACME account credentials are
/// secrets — plain `std::fs::write` can create files as `0644` (world-
/// readable) under a permissive umask. `OpenOptions::mode()` only applies
/// when `O_CREAT` actually creates the file, so a pre-existing file from
/// before this fix (or one that somehow ended up with looser permissions)
/// would keep its old mode on a mere re-open — the explicit
/// `set_permissions` call after writing re-tightens it every time,
/// covering the overwrite/renewal case, not just first creation.
///
/// On non-Unix platforms (no POSIX permission bits), falls back to a plain
/// write.
pub(super) fn write_secret_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        // Deliberately `.truncate(false)` (explicit, not just omitted --
        // clippy::suspicious_open_options requires stating the intent):
        // OpenOptions::truncate(true) truncates as part of the `open()`
        // syscall itself, before this code gets a chance to chmod a
        // pre-existing looser-permission file first — that would leave a
        // window where the just-truncated (now empty) file is being
        // refilled with fresh secret content while still at its *old*
        // mode, e.g. `0644` (CodeRabbit finding on this PR). Truncating
        // manually via `set_len(0)` *after* chmod closes that window:
        // permissions are tightened before any content-modifying
        // operation ever touches the file.
        //
        // `O_NOFOLLOW` (issue #301): without it, a symlink pre-placed at
        // `path` by a local attacker able to write to the ACME storage
        // directory would be followed by `open()`, writing secret material
        // (TLS private key / ACME account credentials) to the symlink's
        // target instead of the intended location. Same pattern already
        // established twice in this codebase — see
        // `conduit_core::util::log_writer`'s and `static_files`'s own
        // `open_no_follow()`. `ELOOP` (symlink detected) surfaces as a
        // plain `io::Error` from `open()`, same as any other open failure.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.set_len(0)?;
        file.write_all(contents)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_creates_with_owner_only_permissions() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        write_secret_file(&path, b"top secret key material").unwrap();
        assert_eq!(
            mode_of(&path),
            0o600,
            "a freshly created secret file must be owner-only, not umask-dependent"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"top secret key material");
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_tightens_permissions_on_overwrite() {
        // Simulates a file that pre-dates this fix, or was otherwise created
        // with looser permissions (e.g. by an older Conduit build) — the
        // next write must re-tighten it to 0600, not just leave the
        // existing mode alone (OpenOptions::mode() only applies when
        // O_CREAT actually creates the file).
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        std::fs::write(&path, b"old content").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&path), 0o644, "test setup sanity check");

        write_secret_file(&path, b"new secret content").unwrap();

        assert_eq!(
            mode_of(&path),
            0o600,
            "overwriting a pre-existing file must re-tighten its permissions"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"new secret content");
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_shorter_overwrite_leaves_no_stale_trailing_bytes() {
        // write_secret_file no longer uses OpenOptions::truncate(true)
        // (removed per a CodeRabbit finding on this PR — see its doc
        // comment) and truncates manually via set_len(0) instead. This
        // proves that still correctly drops old trailing content when the
        // new contents are shorter than what was there before, not just
        // that permissions end up right.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        write_secret_file(&path, b"a very long old secret payload").unwrap();
        write_secret_file(&path, b"short").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"short",
            "no stale bytes from the longer previous content must survive"
        );
    }

    #[test]
    #[cfg(unix)]
    fn write_secret_file_rejects_symlink_at_target_path() {
        // Issue #301: a symlink pre-placed at `path` must not be followed —
        // O_NOFOLLOW should make the open() fail (ELOOP) rather than write
        // secret material through to the symlink's target.
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let real_target = dir.path().join("real-secret.pem");
        let link = dir.path().join("secret.pem");
        std::fs::write(&real_target, b"pre-existing, must not be overwritten").unwrap();
        symlink(&real_target, &link).unwrap();

        let result = write_secret_file(&link, b"attacker-adjacent content");
        assert!(result.is_err(), "writing through a symlink must fail");
        assert_eq!(
            std::fs::read(&real_target).unwrap(),
            b"pre-existing, must not be overwritten",
            "the symlink target must be untouched"
        );
    }
}
