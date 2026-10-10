//! On-disk storage of ACME secrets and certificates (TLS private key, account
//! credentials, certificate chain): owner-only, never half-written.
//!
//! Everything goes through a same-directory staging file that is `fsync`ed and
//! then renamed over the target (issue #491 review, F1). A crash, kill or
//! `ENOSPC` mid-write therefore leaves the previous file intact instead of a
//! truncated key — which Pingora's rustls setup would turn into a panic at the
//! next start. A certificate and key are two files, so a crash *between* the
//! two renames can still leave a mismatched pair; [`cached_pair_matches`] is
//! what turns that into a re-order instead of a failed start.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

/// The certificate and private-key file for `domain` inside `storage_dir`:
/// `<domain>.crt.pem` and `<domain>.key.pem` (issue #554).
///
/// Every place that reads or writes these files gets the names from here. `domain` is the site's
/// `host`, which a config author (a Kubernetes tenant, in CRD mode) controls, so it is checked
/// twice: it must be a plain DNS name ([`crate::domain::validate_domain`], the same check config
/// validation runs), and each resulting path must be a single file name directly inside
/// `storage_dir`. The second check cannot fail for a name the first one accepts; it is there so
/// that a change to either can never move a key file out of the storage directory unnoticed.
pub(super) fn pair_paths(storage_dir: &Path, domain: &str) -> io::Result<(PathBuf, PathBuf)> {
    crate::domain::validate_domain(domain).map_err(|why| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to use the ACME domain as a file name: {why}"),
        )
    })?;
    let cert = storage_dir.join(format!("{domain}.crt.pem"));
    let key = storage_dir.join(format!("{domain}.key.pem"));
    for path in [&cert, &key] {
        if !is_file_directly_in(storage_dir, path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to use an ACME file path outside the storage directory",
            ));
        }
    }
    Ok((cert, key))
}

/// `true` when `path` is one file name placed directly inside `dir`: its parent is `dir` itself
/// (not `dir/..` or a subdirectory) and its last component is a normal name (not `..`).
fn is_file_directly_in(dir: &Path, path: &Path) -> bool {
    path.parent() == Some(dir)
        && matches!(path.components().next_back(), Some(Component::Normal(_)))
}

/// `<path>.tmp` — the staging file next to `path` (same directory, so the final
/// `rename` never crosses a filesystem boundary).
fn staging_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(".tmp");
    PathBuf::from(os)
}

/// Create a fresh owner-only (0600) staging file at `path`.
///
/// Any leftover from a previous crashed run is removed first. `create_new`
/// (`O_EXCL`) never follows a symlink, and `O_NOFOLLOW` on top of that keeps
/// the issue #301 guarantee: a symlink pre-placed in the storage directory by
/// a local attacker is never written through — removing it only deletes the
/// link, and the key material lands in a brand-new inode with mode 0600 from
/// creation (issue #278), so there is no window at a looser mode.
fn create_staging_file(path: &Path) -> io::Result<File> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn write_and_sync(file: &mut File, contents: &[u8]) -> io::Result<()> {
    file.write_all(contents)?;
    file.flush()?;
    file.sync_all()
}

/// Atomically replace `path` with `contents`, owner-only (0600 on Unix).
///
/// A symlink at `path` is replaced by the new file, never followed (#301).
pub(super) fn write_secret_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = staging_path(path);
    let result = (|| {
        let mut file = create_staging_file(&tmp)?;
        write_and_sync(&mut file, contents)?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A certificate + key pair whose staging files already exist.
///
/// [`PendingPair::create`] runs **before** the ACME order, so an unwritable or
/// read-only storage directory fails there — before any CA contact — instead
/// of after a certificate has been issued and thrown away (F2: it would
/// otherwise burn one order per renewal tick against Let's Encrypt's
/// duplicate-certificate limit). [`PendingPair::commit`] is synchronous with no
/// `.await` between the renames, so task cancellation cannot land inside it.
pub(super) struct PendingPair {
    cert_file: File,
    key_file: File,
    cert_tmp: PathBuf,
    key_tmp: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

impl PendingPair {
    pub(super) fn create(storage_dir: &Path, domain: &str) -> io::Result<Self> {
        let (cert, key) = pair_paths(storage_dir, domain)?;
        let cert_tmp = staging_path(&cert);
        let key_tmp = staging_path(&key);
        let cert_file = create_staging_file(&cert_tmp)?;
        // A failure here drops `pending` partially built — clean the first one.
        let key_file = match create_staging_file(&key_tmp) {
            Ok(f) => f,
            Err(e) => {
                let _ = std::fs::remove_file(&cert_tmp);
                return Err(e);
            }
        };
        Ok(Self {
            cert_file,
            key_file,
            cert_tmp,
            key_tmp,
            cert,
            key,
        })
    }

    /// Write both files, `fsync` them, then rename key → cert into place.
    pub(super) fn commit(mut self, cert_pem: &str, key_pem: &str) -> io::Result<()> {
        write_and_sync(&mut self.key_file, key_pem.as_bytes())?;
        write_and_sync(&mut self.cert_file, cert_pem.as_bytes())?;
        std::fs::rename(&self.key_tmp, &self.key)?;
        std::fs::rename(&self.cert_tmp, &self.cert)
    }
}

impl Drop for PendingPair {
    /// Whatever is still staged (an error before `commit` finished, or the
    /// order failing) is removed; after a successful rename the temp names no
    /// longer exist and these are harmless no-ops.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.cert_tmp);
        let _ = std::fs::remove_file(&self.key_tmp);
    }
}

/// `true` when `key_pem` is the private key of the leaf certificate in
/// `cert_pem`. Compares the raw public key — no new dependency (`rcgen` and
/// `x509-parser` are already used here).
///
/// Any parse failure is `false`, so an unreadable or truncated pair is treated
/// as "not a usable cache" and re-ordered rather than handed to the TLS stack.
pub(super) fn cached_pair_matches(cert_pem: &str, key_pem: &str) -> bool {
    use x509_parser::pem::parse_x509_pem;
    let Ok(key) = rcgen::KeyPair::from_pem(key_pem) else {
        return false;
    };
    let Ok((_, pem)) = parse_x509_pem(cert_pem.as_bytes()) else {
        return false;
    };
    let Ok(x509) = pem.parse_x509() else {
        return false;
    };
    x509.public_key().subject_public_key.data.as_ref() == key.public_key_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed cert + its key, PEM-encoded.
    fn pair(name: &str) -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("keygen");
        let cert = rcgen::CertificateParams::new(vec![name.to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("self-signed");
        (cert.pem(), key.serialize_pem())
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    // ── cached_pair_matches ──────────────────────────────────────────────────

    #[test]
    fn matching_pair_is_accepted() {
        let (cert, key) = pair("example.com");
        assert!(cached_pair_matches(&cert, &key));
    }

    #[test]
    fn cert_with_a_different_key_is_rejected() {
        // A crash between the two renames leaves exactly this: the new cert
        // next to the previous key (or vice versa).
        let (cert, _) = pair("example.com");
        let (_, other_key) = pair("example.com");
        assert!(!cached_pair_matches(&cert, &other_key));
    }

    #[test]
    fn unparseable_input_is_rejected() {
        let (cert, key) = pair("example.com");
        assert!(!cached_pair_matches("not a cert", &key));
        assert!(!cached_pair_matches(&cert, "not a key"));
        assert!(!cached_pair_matches(&cert, ""));
    }

    // ── write_secret_atomic ──────────────────────────────────────────────────

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_staging_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        std::fs::write(&path, b"a much longer previous content").unwrap();
        write_secret_atomic(&path, b"short").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
        assert!(!staging_path(&path).exists());
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_is_owner_only_even_over_a_world_readable_file() {
        // Issue #278: a file left at 0644 by an older build must not keep that
        // mode — the new content is a fresh inode created at 0600.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret.pem");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_secret_atomic(&path, b"top secret key material").unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_replaces_a_symlink_instead_of_following_it() {
        // Issue #301: the symlink target must stay untouched.
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let real_target = dir.path().join("real-secret.pem");
        let link = dir.path().join("secret.pem");
        std::fs::write(&real_target, b"pre-existing, must not be overwritten").unwrap();
        symlink(&real_target, &link).unwrap();

        write_secret_atomic(&link, b"attacker-adjacent content").unwrap();

        assert_eq!(
            std::fs::read(&real_target).unwrap(),
            b"pre-existing, must not be overwritten",
            "the symlink target must be untouched"
        );
        assert!(!std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&link).unwrap(), b"attacker-adjacent content");
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_planted_at_the_staging_path_is_not_followed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"victim").unwrap();
        let path = dir.path().join("secret.pem");
        symlink(&victim, staging_path(&path)).unwrap();

        write_secret_atomic(&path, b"key material").unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), b"victim");
        assert_eq!(std::fs::read(&path).unwrap(), b"key material");
    }

    // ── PendingPair ──────────────────────────────────────────────────────────

    #[test]
    fn commit_writes_both_files_and_leaves_no_staging_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let cert = dir.path().join("example.com.crt.pem");
        let key = dir.path().join("example.com.key.pem");
        let pending = PendingPair::create(dir.path(), "example.com").unwrap();
        pending.commit("CERT", "KEY").unwrap();
        assert_eq!(std::fs::read_to_string(&cert).unwrap(), "CERT");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "KEY");
        assert!(!staging_path(&cert).exists());
        assert!(!staging_path(&key).exists());
        #[cfg(unix)]
        assert_eq!(mode_of(&key), 0o600);
    }

    #[test]
    fn dropping_without_commit_keeps_the_existing_pair_and_cleans_up() {
        // The order failed after the staging files were created: the cached
        // cert/key (still the best we have) must be untouched.
        let dir = tempfile::TempDir::new().unwrap();
        let cert = dir.path().join("example.com.crt.pem");
        let key = dir.path().join("example.com.key.pem");
        std::fs::write(&cert, "OLD CERT").unwrap();
        std::fs::write(&key, "OLD KEY").unwrap();

        drop(PendingPair::create(dir.path(), "example.com").unwrap());

        assert_eq!(std::fs::read_to_string(&cert).unwrap(), "OLD CERT");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "OLD KEY");
        assert!(!staging_path(&cert).exists());
        assert!(!staging_path(&key).exists());
    }

    #[test]
    fn create_fails_before_any_work_when_the_directory_is_missing() {
        // Stand-in for an unwritable storage dir: the pre-flight must fail
        // here, i.e. before the CA is contacted.
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("no-such-dir");
        assert!(PendingPair::create(&missing, "example.com").is_err());
    }

    #[test]
    fn a_stale_staging_file_from_a_crashed_run_is_replaced() {
        let dir = tempfile::TempDir::new().unwrap();
        let stale = staging_path(&dir.path().join("example.com.key.pem"));
        std::fs::write(&stale, "half-written garbage").unwrap();
        PendingPair::create(dir.path(), "example.com")
            .unwrap()
            .commit("C", "K")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("example.com.key.pem")).unwrap(),
            "K"
        );
    }

    // ── file names from the site host (issue #554) ───────────────────────────

    #[test]
    fn pair_paths_puts_both_files_directly_inside_the_storage_dir() {
        let dir = Path::new("storage/certs");
        for domain in [
            "example.com",
            "a.example.com",
            "*.example.com",
            "localhost",
            "xn--bcher-kva.example",
        ] {
            let (cert, key) = pair_paths(dir, domain).unwrap();
            assert_eq!(cert, dir.join(format!("{domain}.crt.pem")));
            assert_eq!(key, dir.join(format!("{domain}.key.pem")));
            assert!(
                is_file_directly_in(dir, &cert),
                "certificate path of {domain}"
            );
            assert!(is_file_directly_in(dir, &key), "key path of {domain}");
        }
    }

    #[test]
    fn pair_paths_refuses_names_that_could_leave_the_storage_dir() {
        let dir = Path::new("storage/certs");
        for domain in [
            "..",
            "../x",
            "../../etc/cron.d/x",
            "a/b",
            "/abs",
            "a\\b",
            "..\\x",
            "a/../../b",
            "x\0y",
            ".hidden.example.com",
            "",
            "*",
        ] {
            let err = pair_paths(dir, domain).expect_err(domain);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{domain:?}");
        }
    }

    /// The second line of defence, tested on its own because no name that passes the DNS check
    /// can trigger it.
    #[test]
    fn a_path_is_inside_the_dir_only_as_one_plain_file_name() {
        let dir = Path::new("storage/certs");
        assert!(is_file_directly_in(dir, &dir.join("a.example.com.crt.pem")));
        assert!(!is_file_directly_in(dir, &dir.join("../a.crt.pem")));
        assert!(!is_file_directly_in(dir, &dir.join("sub/a.crt.pem")));
        assert!(!is_file_directly_in(dir, &dir.join("..")));
        assert!(!is_file_directly_in(dir, Path::new("/etc/a.crt.pem")));
        assert!(!is_file_directly_in(dir, Path::new("a.crt.pem")));
    }

    #[test]
    fn staging_files_for_an_invalid_name_are_never_created() {
        let dir = tempfile::TempDir::new().unwrap();
        let storage = dir.path().join("certs");
        std::fs::create_dir(&storage).unwrap();
        let err = PendingPair::create(&storage, "../escape")
            .err()
            .expect("refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // Before the fix the staging files landed next to `certs`, outside it.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&storage).unwrap().count(), 0);
    }
}
