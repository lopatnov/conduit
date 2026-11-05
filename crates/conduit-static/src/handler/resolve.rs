//! Finding the file a request names, and opening it below its root without following a link.

use std::path::{Path, PathBuf};

use conduit_core::util::encoding::AcceptEncoding;

use crate::config::StaticOptions;

/// Open `path` (which lies below `root`) for reading without following a symlink at any level.
///
/// The walk from `root` and the open are one step on Unix (`openat` + `O_NOFOLLOW`, issue #557) and on
/// Windows (`NtCreateFile` relative to the parent handle, issue #600; see [`crate::open_beneath`]), which
/// closes the window between the `symlink_metadata` checks in `find_file` and the open. Any other
/// platform has a plain open (see "Symlinks" in `docs/configuration.md`).
pub(super) async fn open_no_follow(root: &Path, path: &Path) -> std::io::Result<tokio::fs::File> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
        .to_owned();
    let root = root.to_owned();
    // `openat(2)` blocks, so it runs on the blocking pool.
    let std_file =
        tokio::task::spawn_blocking(move || crate::open_beneath::open_beneath(&root, &rel))
            .await
            .map_err(std::io::Error::other)??;
    Ok(tokio::fs::File::from_std(std_file))
}

/// Check `path` metadata WITHOUT following symlinks.
///
/// Returns `None` when the path does not exist or is a symbolic link.
/// Symlinks are rejected to prevent directory traversal: a symlink inside
/// the static root could point anywhere on the filesystem (e.g.
/// `/var/www/html/secret` → `/etc/passwd`), bypassing `sanitize_path`'s
/// `..`-traversal protection.
async fn stat_no_symlink(path: &std::path::Path) -> Option<std::fs::Metadata> {
    let meta = tokio::fs::symlink_metadata(path).await.ok()?;
    if meta.is_symlink() {
        return None; // Reject — symlink could escape the static root.
    }
    Some(meta)
}

/// `true` when no directory *between* `root` and the last segment of `rel` is a symlink.
///
/// `stat_no_symlink`/`open_no_follow` only look at the final path component, so a directory symlink
/// earlier in the path (`assets -> /etc`) would be followed by the OS and let a request read
/// outside the root (CWE-59, issue #400). `rel` is already sanitised (no `..`, no empty or root
/// segments). A single-segment path has nothing to check and costs nothing; otherwise the walk is
/// one `symlink_metadata` call per intermediate directory on the blocking pool.
async fn parent_dirs_are_not_symlinks(root: &Path, rel: &str) -> bool {
    let parents: Vec<String> = {
        let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return true;
        }
        segments[..segments.len() - 1]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    };
    let mut cur = root.to_owned();
    tokio::task::spawn_blocking(move || {
        for seg in parents {
            cur.push(seg);
            match std::fs::symlink_metadata(&cur) {
                Ok(m) if !m.is_symlink() => {}
                _ => return false,
            }
        }
        true
    })
    .await
    .unwrap_or(false)
}

/// `true` when a path segment is a name the Windows open refuses (see `open_beneath::name_is_refused`):
/// one that ends in a dot or a space (Win32 strips those, `index.html.` is `index.html`, while the
/// handle-relative open of issue #600 takes the name literally), or that has the shape of a generated
/// 8.3 short name (`HTPASS~1` reaches `.htpasswd`, issue #617). Empty segments are not names.
#[cfg(windows)]
fn has_refused_segment(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|s| !s.is_empty() && crate::open_beneath::name_is_refused(s))
}

pub(super) async fn find_file<'a>(
    roots: &'a [PathBuf],
    rel: &str,
    options: &StaticOptions,
) -> Option<(&'a Path, PathBuf)> {
    // Names the handle-relative open on Windows refuses (a trailing dot or space that Win32 would
    // strip, issue #600; the shape of an 8.3 short name, issue #617): say "not found" here, before
    // anything is built, instead of failing the open later.
    #[cfg(windows)]
    if has_refused_segment(rel) {
        return None;
    }
    for root in roots {
        if !parent_dirs_are_not_symlinks(root, rel).await {
            continue;
        }
        let candidate = root.join(rel);
        match stat_no_symlink(&candidate).await {
            Some(m) if m.is_file() => return Some((root, candidate)),
            Some(m) if m.is_dir() => {
                if let Some(p) = find_index(&candidate, options).await {
                    return Some((root, p));
                }
            }
            _ => {}
        }
    }
    if rel.is_empty() {
        for root in roots {
            if let Some(p) = find_index(root, options).await {
                return Some((root, p));
            }
        }
    }
    None
}

async fn find_index(dir: &Path, options: &StaticOptions) -> Option<PathBuf> {
    let defaults = vec!["index.html".to_string()];
    let indices = options.index.as_deref().unwrap_or(&defaults);
    for name in indices {
        // Same Windows rule as `find_file`: such a name would pass the Win32 stat below and then be
        // refused by the handle-relative open after the headers are sent, so try the next entry.
        #[cfg(windows)]
        if has_refused_segment(name) {
            continue;
        }
        let p = dir.join(name);
        if stat_no_symlink(&p)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

/// Look for a pre-compressed sibling file next to `path`.
///
/// Preference order: brotli (`.br`) → gzip (`.gz`), filtered by what the
/// client declares in `Accept-Encoding`.  Returns the path to the
/// pre-compressed file and its encoding token (`"br"` or `"gzip"`), or `None`
/// when no suitable sibling exists.
async fn find_pre_compressed(
    path: &Path,
    accept_enc: &AcceptEncoding,
) -> Option<(PathBuf, &'static str)> {
    // Candidates in preference order.
    let candidates: &[(&'static str, &'static str)] = &[("br", ".br"), ("gzip", ".gz")];

    for (enc, suffix) in candidates {
        let accept = match *enc {
            "br" => accept_enc.brotli,
            "gzip" => accept_enc.gzip,
            _ => false,
        };
        if !accept {
            continue;
        }
        let mut pre = path.as_os_str().to_owned();
        pre.push(suffix);
        let pre_path = PathBuf::from(pre);
        if stat_no_symlink(&pre_path)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some((pre_path, enc));
        }
    }
    None
}

/// Re-stat an already-`find_pre_compressed`-confirmed candidate, returning
/// its current size.
///
/// Split out from [`resolve_pre_compressed`] specifically so this half —
/// the actual TOCTOU guard — is independently testable: calling
/// `resolve_pre_compressed` again after removing the file would re-run
/// `find_pre_compressed` too and short-circuit before ever reaching this
/// re-stat, so a test built that way would prove nothing about this
/// function's own failure handling.
///
/// On a failed re-stat (the sibling was removed or replaced since
/// `find_pre_compressed` confirmed it), returns `None` so the caller falls
/// through to serving the uncompressed original instead of defaulting to a
/// `pre_size` of `0`, which would send a 200 with `content-length: 0` for
/// an asset that actually exists uncompressed.
async fn confirm_pre_compressed(
    pre_path: PathBuf,
    encoding: &'static str,
) -> Option<(PathBuf, &'static str, u64)> {
    let pre_size = stat_no_symlink(&pre_path).await?.len();
    Some((pre_path, encoding, pre_size))
}

/// Resolve a pre-compressed sibling to serve, including its current size.
///
/// Guards the same TOCTOU window as the main file check — see
/// [`confirm_pre_compressed`]'s doc comment for why the re-stat is a
/// separate function rather than inlined here.
pub(super) async fn resolve_pre_compressed(
    path: &Path,
    accept_enc: &AcceptEncoding,
) -> Option<(PathBuf, &'static str, u64)> {
    let (pre_path, encoding) = find_pre_compressed(path, accept_enc).await?;
    confirm_pre_compressed(pre_path, encoding).await
}

/// Attempt to decode a two-byte hex sequence (the digits after `%`) into a
/// raw byte.  Returns `None` if the sequence is invalid or encodes a
/// dangerous byte.
fn try_decode_percent_seq(hex: &[u8]) -> Option<u8> {
    let hs = std::str::from_utf8(hex).ok()?;
    let byte = u8::from_str_radix(hs, 16).ok()?;
    // Never decode dangerous bytes:
    // • %2F '/' and %5C '\' — path separators enable directory traversal.
    // • %00 NUL — null bytes truncate paths at the OS syscall boundary on
    //   POSIX systems (e.g. `file.php%00.txt` → opens `file.php`).
    match byte {
        b'/' | b'\\' | b'\0' => None,
        b => Some(b),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(byte) = try_decode_percent_seq(&bytes[i + 1..i + 3]) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sanitize_path(path: &str) -> String {
    // Normalize backslashes to forward slashes before splitting so that
    // Windows path separators cannot be used to bypass traversal checks.
    let normalized = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for segment in normalized.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            // A colon-bearing segment enables two distinct Windows-specific
            // escapes if let through: a drive prefix like `C:` makes
            // `PathBuf::join` treat the rest as an absolute path (replacing
            // `root` entirely, since a prefix+root is "absolute" per
            // `Path::join`'s own documented behavior — verified: `find_file`'s
            // `root.join(rel)` would resolve straight to `C:/Windows/win.ini`,
            // not under `root`), and `name.txt:stream` targets an NTFS
            // alternate data stream instead of the plain file. Neither shape
            // is meaningful outside Windows — `:` is an ordinary, legal
            // filename character on Linux/macOS (this repo's actual runtime
            // targets), so gate the drop to Windows builds only rather than
            // silently mismatching a legitimately colon-named file on every
            // platform (Gitar + security-engineer review on PR #348). Drop
            // the segment rather than rejecting the whole request — matches
            // how "."/".."/empty segments are already silently filtered out
            // just above.
            s if cfg!(windows) && s.contains(':') => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

pub(super) fn has_dotfile(rel_path: &str) -> bool {
    rel_path
        .split('/')
        .any(|seg| seg.starts_with('.') && !seg.is_empty())
}

/// Decode and sanitize the request path, optionally stripping a route prefix.
pub(super) fn decode_rel_path(req_path: &str, strip_prefix: Option<&str>) -> String {
    let decoded = percent_decode(req_path);
    match strip_prefix {
        Some(pfx) => {
            let trimmed = pfx.trim_end_matches('/');
            let after = if trimmed.is_empty() {
                decoded.as_str()
            } else {
                decoded.strip_prefix(trimmed).unwrap_or(&decoded)
            };
            sanitize_path(after)
        }
        None => sanitize_path(&decoded),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Windows symlink creation requires elevated privileges in CI, so this test only
    // runs on unix — `std::os::unix::fs::symlink` isn't available on Windows anyway.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_in_static_root_is_not_served() {
        // stat_no_symlink must return None for symlinks — serving them would
        // allow directory traversal (symlink → /etc/passwd, etc.).
        let dir = tempfile::tempdir().unwrap();
        let real_file = dir.path().join("real.txt");
        std::fs::write(&real_file, "contents").unwrap();

        // Create a symlink pointing to the real file.
        let link_path = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real_file, &link_path).unwrap();

        // stat_no_symlink must reject the symlink.
        assert!(
            stat_no_symlink(&link_path).await.is_none(),
            "symlink must not be served by static file handler"
        );
        // Regular file is fine.
        assert!(
            stat_no_symlink(&real_file).await.is_some(),
            "regular file must be served"
        );
    }

    /// Issue #400: a directory symlink *inside* the path must not be followed — the final
    /// component of `assets/secret.txt` is a regular file, but `assets` leads outside the root.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_directory_in_the_path_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("assets")).unwrap();
        std::fs::create_dir(root.path().join("real")).unwrap();
        std::fs::write(root.path().join("real/ok.txt"), "fine").unwrap();
        std::fs::write(root.path().join("top.txt"), "fine").unwrap();

        let roots = vec![root.path().to_path_buf()];
        let options = StaticOptions::default();
        assert!(find_file(&roots, "assets/secret.txt", &options)
            .await
            .is_none());
        assert!(find_file(&roots, "real/ok.txt", &options).await.is_some());
        assert!(find_file(&roots, "top.txt", &options).await.is_some());
    }

    /// `open_no_follow` is the last line of defence against a symlink swapped in
    /// between `stat_no_symlink` and the actual open (TOCTOU), so it must itself
    /// refuse to follow one — `O_NOFOLLOW` makes `open(2)` fail with `ELOOP`.
    #[cfg(unix)]
    #[tokio::test]
    async fn open_no_follow_refuses_a_symlink_but_opens_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let real_file = dir.path().join("real.txt");
        std::fs::write(&real_file, b"hello").unwrap();
        let link_path = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real_file, &link_path).unwrap();

        let err = open_no_follow(dir.path(), &link_path)
            .await
            .expect_err("a symlink must not be opened");
        assert_eq!(
            err.raw_os_error(),
            Some(rustix::io::Errno::LOOP.raw_os_error()),
            "expected ELOOP from O_NOFOLLOW, got: {err}"
        );

        assert!(
            open_no_follow(dir.path(), &real_file).await.is_ok(),
            "a regular file must still open"
        );
    }

    #[tokio::test]
    async fn find_file_returns_the_root_that_holds_the_file_or_its_index() {
        let (a, b, c) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        // `a` holds `first.txt`, `b` holds `f.txt` and `d/index.html`, `c` (the last root) is
        // empty except for a root-level index.html: a lookup that always returned the first or
        // the last root would get one of these wrong.
        std::fs::write(a.path().join("first.txt"), "x").unwrap();
        std::fs::write(b.path().join("f.txt"), "x").unwrap();
        std::fs::create_dir(b.path().join("d")).unwrap();
        std::fs::write(b.path().join("d/index.html"), "x").unwrap();
        std::fs::write(c.path().join("index.html"), "x").unwrap();
        let roots = [
            a.path().to_path_buf(),
            b.path().to_path_buf(),
            c.path().to_path_buf(),
        ];
        let opts = StaticOptions::default();
        let (root, path) = find_file(&roots, "first.txt", &opts).await.unwrap();
        assert_eq!((root, path), (a.path(), a.path().join("first.txt")));
        let (root, path) = find_file(&roots, "f.txt", &opts).await.unwrap();
        assert_eq!((root, path), (b.path(), b.path().join("f.txt")));
        let (root, path) = find_file(&roots, "d", &opts).await.unwrap();
        assert_eq!((root, path), (b.path(), b.path().join("d/index.html")));
        // An empty `rel` is the root itself: the first root with an index file wins.
        let (root, path) = find_file(&roots, "", &opts).await.unwrap();
        assert_eq!((root, path), (c.path(), c.path().join("index.html")));
        assert!(find_file(&roots, "missing", &opts).await.is_none());
    }

    #[tokio::test]
    async fn resolve_pre_compressed_returns_size_when_sibling_exists() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("app.js");
        std::fs::write(&base, "irrelevant").unwrap();
        std::fs::write(dir.path().join("app.js.gz"), b"compressed-bytes").unwrap();

        let accept_enc = AcceptEncoding {
            gzip: true,
            ..Default::default()
        };
        let (pre_path, encoding, pre_size) = resolve_pre_compressed(&base, &accept_enc)
            .await
            .expect("gz sibling exists and is confirmed by both stats");
        assert_eq!(pre_path, dir.path().join("app.js.gz"));
        assert_eq!(encoding, "gzip");
        assert_eq!(pre_size, "compressed-bytes".len() as u64);
    }

    #[tokio::test]
    async fn confirm_pre_compressed_returns_size_when_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let gz_path = dir.path().join("app.js.gz");
        std::fs::write(&gz_path, b"compressed-bytes").unwrap();

        let (returned_path, encoding, pre_size) = confirm_pre_compressed(gz_path.clone(), "gzip")
            .await
            .expect("file exists, re-stat must succeed");
        assert_eq!(returned_path, gz_path);
        assert_eq!(encoding, "gzip");
        assert_eq!(pre_size, "compressed-bytes".len() as u64);
    }

    #[tokio::test]
    async fn confirm_pre_compressed_falls_through_when_file_removed() {
        // Directly exercises the TOCTOU guard itself (not resolve_pre_compressed,
        // which would re-run find_pre_compressed internally and short-circuit
        // before ever reaching this re-stat if the file were already gone —
        // confirm_pre_compressed exists precisely so this failure path is
        // reachable in a deterministic test). Before the fix, the caller
        // defaulted `pre_size` to 0 and still served a 200 with an empty body
        // for a file confirmed present moments earlier; the fix must return
        // `None` here so the caller falls through to the uncompressed original.
        let dir = tempfile::tempdir().unwrap();
        let gz_path = dir.path().join("app.js.gz");
        std::fs::write(&gz_path, b"compressed-bytes").unwrap();

        // Confirm the happy path works before removing the file.
        assert!(
            confirm_pre_compressed(gz_path.clone(), "gzip")
                .await
                .is_some(),
            "sanity: re-stat must succeed while the file still exists"
        );

        std::fs::remove_file(&gz_path).unwrap();

        assert!(
            confirm_pre_compressed(gz_path, "gzip").await.is_none(),
            "must fall through, not default pre_size to 0, when the re-stat fails"
        );
    }

    #[test]
    fn percent_decode_null_byte_is_rejected() {
        // %00 must NOT be decoded — it would truncate the path at the OS level.
        let decoded = percent_decode("file%00.txt");
        assert!(
            !decoded.contains('\0'),
            "null byte must not appear in decoded path: {decoded:?}"
        );
        // The literal `%00` should remain undecoded, not become NUL.
        assert_eq!(
            decoded, "file%00.txt",
            "undecoded %00 must be preserved as-is"
        );
    }

    #[test]
    fn percent_decode_slash_not_decoded() {
        let decoded = percent_decode("%2Fetc%2Fpasswd");
        assert_eq!(decoded, "%2Fetc%2Fpasswd", "%2F must not be decoded");
    }

    #[test]
    fn percent_decode_normal_chars_decoded() {
        let decoded = percent_decode("hello%20world");
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn sanitize_path_removes_dotdot() {
        assert_eq!(sanitize_path("../etc/passwd"), "etc/passwd");
        assert_eq!(sanitize_path("a/../../b"), "b");
    }

    /// Win32 would have served `index.html.` as `index.html`; the Windows open takes names literally.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_trailing_dot_or_space_in_the_url_is_not_found_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "x").unwrap();
        // A real `sub/index.html`, so `sub./index.html` could only be refused by looking at the
        // *intermediate* segment (Win32 would resolve `sub.` to `sub`).
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("index.html"), "x").unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let opts = StaticOptions::default();
        assert!(find_file(&roots, "index.html", &opts).await.is_some());
        assert!(find_file(&roots, "sub/index.html", &opts).await.is_some());
        for rel in ["index.html.", "index.html ", "sub./index.html"] {
            assert!(find_file(&roots, rel, &opts).await.is_none(), "{rel:?}");
        }
    }

    /// Issue #617: `/HTPASS~1` would reach `.htpasswd` through its NTFS 8.3 alias, past the `dotFiles`
    /// policy that only looked at the request path. The shape of a generated alias is not found, in the
    /// file, in a directory segment, and as a configured index name; longer or ordinary names are.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_short_alias_shaped_name_is_not_found_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HTPASS~1"), "x").unwrap();
        std::fs::create_dir(dir.path().join("GIT~1")).unwrap();
        std::fs::write(dir.path().join("GIT~1").join("config"), "x").unwrap();
        std::fs::write(dir.path().join("report~1.html"), "x").unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let opts = StaticOptions::default();
        for rel in ["HTPASS~1", "htpass~1", "GIT~1/config"] {
            assert!(find_file(&roots, rel, &opts).await.is_none(), "{rel:?}");
        }
        // Longer than an alias can be (extension of four characters): an ordinary file.
        assert!(find_file(&roots, "report~1.html", &opts).await.is_some());
        let opts = StaticOptions {
            index: Some(vec!["INDEX~1".to_string(), "report~1.html".to_string()]),
            ..Default::default()
        };
        std::fs::write(dir.path().join("INDEX~1"), "x").unwrap();
        assert_eq!(
            find_index(dir.path(), &opts).await,
            Some(dir.path().join("report~1.html"))
        );
    }

    /// A configured index name Win32 would resolve (`index.html.`) is skipped, so the next entry wins.
    #[cfg(windows)]
    #[tokio::test]
    async fn find_index_skips_names_the_windows_open_would_refuse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "x").unwrap();
        let opts = StaticOptions {
            index: Some(vec![
                "index.html.".to_string(),
                "index.html ".to_string(),
                "index.html".to_string(),
            ]),
            ..Default::default()
        };
        assert_eq!(
            find_index(dir.path(), &opts).await,
            Some(dir.path().join("index.html"))
        );
    }

    #[test]
    fn sanitize_path_removes_backslash_traversal() {
        // Backslashes are normalized to forward slashes first.
        assert_eq!(sanitize_path("a\\..\\b"), "b");
    }

    #[test]
    fn sanitize_path_collapses_empty_segments() {
        assert_eq!(sanitize_path("a//b"), "a/b");
    }

    #[test]
    fn sanitize_path_removes_single_dot_segments() {
        assert_eq!(sanitize_path("a/./b"), "a/b");
    }

    #[test]
    fn sanitize_path_empty_input() {
        assert_eq!(sanitize_path(""), "");
    }

    #[test]
    fn sanitize_path_root_slash() {
        assert_eq!(sanitize_path("/"), "");
    }

    #[test]
    fn sanitize_path_normal_path() {
        assert_eq!(sanitize_path("images/logo.png"), "images/logo.png");
    }

    #[test]
    #[cfg(windows)]
    fn sanitize_path_drops_windows_drive_prefix() {
        // `root.join("C:/Windows/win.ini")` on Windows treats the drive
        // prefix as an absolute path and replaces `root` entirely — the
        // colon-bearing segment must never survive sanitization. Windows-only:
        // `:` is an ordinary filename character on Linux/macOS, where this
        // escape doesn't exist (see sanitize_path_keeps_colon_on_non_windows).
        assert_eq!(sanitize_path("C:/Windows/win.ini"), "Windows/win.ini");
        assert_eq!(sanitize_path("/C:/Windows/win.ini"), "Windows/win.ini");
    }

    #[test]
    #[cfg(windows)]
    fn sanitize_path_drops_ntfs_alternate_data_stream_suffix() {
        // `name.txt:hidden` targets an NTFS alternate data stream on Windows
        // rather than the plain file — reject the whole segment, not just
        // the drive-prefix shape.
        assert_eq!(sanitize_path("secret.txt:hidden"), "");
    }

    #[test]
    #[cfg(not(windows))]
    fn sanitize_path_keeps_colon_on_non_windows() {
        // `:` is a legal filename character on Linux/macOS (this repo's
        // actual runtime targets) and neither Windows escape (drive-prefix
        // absolute-path replacement, NTFS alternate data streams) exists
        // there — a legitimately colon-named file must stay reachable
        // rather than being silently dropped/mismatched (Gitar +
        // security-engineer review on PR #348).
        assert_eq!(sanitize_path("secret.txt:hidden"), "secret.txt:hidden");
    }

    #[test]
    fn has_dotfile_hidden_file() {
        assert!(has_dotfile(".hidden"), "leading dot must be a dotfile");
    }

    #[test]
    fn has_dotfile_hidden_dir() {
        assert!(has_dotfile(".git/config"), "hidden dir must be a dotfile");
    }

    #[test]
    fn has_dotfile_normal_file() {
        assert!(!has_dotfile("images/logo.png"));
    }

    #[test]
    fn has_dotfile_double_dot_is_dotfile() {
        // ".." starts with '.' so has_dotfile returns true — this is intentional
        // since dotfile blocking should also reject ".." segments (sanitize_path
        // handles the actual traversal prevention separately).
        assert!(has_dotfile(".."));
    }

    #[test]
    fn has_dotfile_empty_string() {
        assert!(!has_dotfile(""));
    }

    #[test]
    fn try_decode_percent_seq_normal_char() {
        // %41 = 'A'
        assert_eq!(try_decode_percent_seq(b"41"), Some(b'A'));
    }

    #[test]
    fn try_decode_percent_seq_space() {
        // %20 = space
        assert_eq!(try_decode_percent_seq(b"20"), Some(b' '));
    }

    #[test]
    fn try_decode_percent_seq_slash_rejected() {
        // %2F = '/' — must NOT be decoded
        assert!(try_decode_percent_seq(b"2F").is_none());
        assert!(try_decode_percent_seq(b"2f").is_none());
    }

    #[test]
    fn try_decode_percent_seq_backslash_rejected() {
        // %5C = '\' — must NOT be decoded
        assert!(try_decode_percent_seq(b"5C").is_none());
    }

    #[test]
    fn try_decode_percent_seq_null_rejected() {
        // %00 = NUL — must NOT be decoded
        assert!(try_decode_percent_seq(b"00").is_none());
    }

    #[test]
    fn try_decode_percent_seq_invalid_hex_returns_none() {
        assert!(try_decode_percent_seq(b"ZZ").is_none());
        assert!(try_decode_percent_seq(b"GG").is_none());
    }

    #[test]
    fn decode_rel_path_no_prefix() {
        assert_eq!(decode_rel_path("/images/logo.png", None), "images/logo.png");
    }

    #[test]
    fn decode_rel_path_strips_prefix() {
        assert_eq!(
            decode_rel_path("/api/images/logo.png", Some("/api")),
            "images/logo.png"
        );
    }

    #[test]
    fn decode_rel_path_percent_decoded() {
        assert_eq!(
            decode_rel_path("/files/hello%20world.txt", None),
            "files/hello world.txt"
        );
    }

    #[test]
    fn decode_rel_path_traversal_prevented() {
        // Even after percent-decoding, .. should be removed by sanitize_path.
        assert_eq!(decode_rel_path("/../etc/passwd", None), "etc/passwd");
    }
}
