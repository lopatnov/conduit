//! Windows half of [`super::open_beneath`] (issue #600, CWE-59, CWE-367).
//!
//! The Unix walk is `openat` + `O_NOFOLLOW`. Windows has no `O_NOFOLLOW`, and `std` cannot open
//! "relative to a directory handle", so a check on the path (`symlink_metadata`) followed by an open
//! of the path leaves a window in which a symlink, or a directory *junction* (no privilege needed
//! to create one), can be swapped in and followed. Pinning directories with a share mode does not
//! close it either: an attribute-only handle ignores the share mode and can turn an empty pinned
//! directory into a junction (measured on Windows 11, see the issue).
//!
//! So each component is opened with `NtCreateFile`, relative to the handle of the directory above
//! it, with `FILE_OPEN_REPARSE_POINT`. A link is then opened as the link itself, and its type is read
//! from the opened handle (`GetFileInformationByHandle`, through `std`'s `file_type().is_symlink()`),
//! so there is nothing left to race: every name lookup is bound to a directory object that was
//! already opened and verified. This is the approach of Go's `os.Root` and of `cap-std`.
//!
//! Choices that follow from the NT API (the name is *not* normalised by Win32):
//! - the name must be one plain component: no `\`, `/`, `:` (drive, alternate data stream), and no
//!   trailing dot or space (which Win32 would strip, so the earlier path checks saw another name);
//! - a name that looks like a generated 8.3 short name (`HTPASS~1`, `PROGRA~1.TXT`) is refused, because
//!   NTFS resolves such an alias to the real entry (`.htpasswd`) and the `dotFiles` policy never saw
//!   that name (issue #617);
//! - lookups are case-insensitive, like `File::open` on a default NTFS volume;
//! - the handle is opened for synchronous I/O, so it works with `std::fs::File` unchanged.
//!
//! A parent that turns into a link while a child is being opened is refused too: after each child
//! open the parent handle is asked again whether it is a link. The measured behaviour on Windows 11
//! (the child lookup through a converted directory fails with error 1921) already fails closed; this
//! check makes it independent of that behaviour on other builds and file systems.
//!
//! A reparse point that is *not* a link (a OneDrive placeholder, a deduplicated file) cannot be
//! read through a `FILE_OPEN_REPARSE_POINT` handle, because the filter that serves it does not run.
//! Such a component is reopened normally, relative to the same verified parent, and must be the
//! same file (volume serial number and file index) as the first open, so a link swapped in meanwhile
//! is refused. That branch has not been run against a real placeholder or deduplicated file (only
//! reparse tags without a handler, which fail closed): it is written from the API contract.
//!
//! There is no `unsafe` here (issue #621): the handle-relative open is `fs_at`, the identity check is
//! `same-file`, and `windows-sys` supplies constants only. The crate denies `unsafe` everywhere
//! (`#![deny(unsafe_code)]` in `lib.rs`).

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};

use fs_at::os::windows::OpenOptionsExt as _;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::Foundation::{ERROR_STOPPED_ON_SYMLINK, OBJ_CASE_INSENSITIVE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ,
    FILE_READ_ATTRIBUTES, SYNCHRONIZE,
};

/// What a refused link is reported as (`ERROR_STOPPED_ON_SYMLINK`, 681).
pub const LINK_REFUSED: i32 = ERROR_STOPPED_ON_SYMLINK as i32;

/// Whether `e` is the error this walk reports for a symlink, a junction or a swapped-in link, so a caller
/// can answer "not found" without repeating the raw Windows error value.
pub fn is_link_refused(e: &io::Error) -> bool {
    e.raw_os_error() == Some(LINK_REFUSED)
}
/// What a directory handle needs: to be a root for relative opens and to report its type. No listing
/// right, so a directory the account can only traverse still works.
const DIRECTORY_ACCESS: u32 = FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// What a refused link is reported as: `ERROR_STOPPED_ON_SYMLINK`.
fn link_error() -> io::Error {
    io::Error::from_raw_os_error(LINK_REFUSED)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Directory,
    File,
}

/// Open `root`/`rel` read-only. `rel` must consist of normal components only (no `..`, no root).
/// The root itself may be a symlink or junction (the operator chose it); nothing below it may be.
pub fn open_beneath(root: &Path, rel: &Path) -> io::Result<File> {
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

    // The root is resolved normally, which follows a symlinked or junctioned root.
    let mut cur = OpenOptions::new()
        .access_mode(DIRECTORY_ACCESS)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(root)?;
    for name in dirs {
        cur = open_component(&cur, name, Kind::Directory)?;
    }
    open_component(&cur, last, Kind::File)
}

/// What to do with a freshly opened component.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// An ordinary file or directory.
    Use,
    /// A symlink or junction.
    Refuse,
    /// A reparse point that is not a link (placeholder, deduplicated file).
    Reopen,
}

/// Decide what to do with an opened component from its attributes and from whether it is a link (a symlink,
/// a junction or another name-surrogate reparse point).
fn verdict(attributes: u32, is_link: bool) -> Verdict {
    if is_link {
        Verdict::Refuse
    } else if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Verdict::Reopen
    } else {
        Verdict::Use
    }
}

/// Open one path component below `parent` and judge it: a plain file or directory is returned, a link is
/// refused, a reparse point that is not a link is reopened and checked against the first open. The parent
/// must still be a plain directory afterwards.
fn open_component(parent: &File, name: &OsStr, kind: Kind) -> io::Result<File> {
    let first = open_relative(parent, name, kind, false)?;
    let meta = first.metadata()?;
    let child = match verdict(meta.file_attributes(), meta.file_type().is_symlink()) {
        Verdict::Use => first,
        Verdict::Refuse => return Err(link_error()),
        Verdict::Reopen => reopen_if_same(first, || open_relative(parent, name, kind, true))?,
    };
    // The lookup above went through `parent`. If `parent` has become a link meanwhile (a directory
    // converted in place), do not trust what the lookup returned.
    ensure_parent_plain(parent)?;
    Ok(child)
}

/// Refuse when the directory handle `parent` now is a symlink or junction.
fn ensure_parent_plain(parent: &File) -> io::Result<()> {
    if parent.metadata()?.file_type().is_symlink() {
        Err(link_error())
    } else {
        Ok(())
    }
}

/// The second open of a non-link reparse point must be the same file as the first.
fn reopen_if_same(first: File, open_second: impl FnOnce() -> io::Result<File>) -> io::Result<File> {
    let second = open_second()?;
    if same_file(&first, &second)? {
        Ok(second)
    } else {
        // The name now resolves to something else: swapped in between the two opens.
        Err(link_error())
    }
}

/// Whether two open handles refer to the same file: same volume, same file index.
///
/// The index is the 64-bit one that `GetFileInformationByHandle` reports. On ReFS the file id is 128
/// bits wide and this keeps only the low part (the exact id needs `FileIdInfo`, which `std` does not
/// expose in stable); two different files that agree in volume and low index are not something a name
/// swap can produce.
fn same_file(a: &File, b: &File) -> io::Result<bool> {
    Ok(same_file::Handle::from_file(a.try_clone()?)?
        == same_file::Handle::from_file(b.try_clone()?)?)
}

/// One plain path component: what `NtCreateFile` may be handed as a relative name.
fn valid_name(wide: &[u16]) -> bool {
    const BAD: [u16; 4] = [b'\\' as u16, b'/' as u16, b':' as u16, 0];
    let (dot, space) = (u16::from(b'.'), u16::from(b' '));
    !wide.is_empty()
        && !wide.iter().any(|c| BAD.contains(c))
        && !matches!(wide.last(), Some(&c) if c == dot || c == space)
        && !looks_like_short_alias(wide)
}

/// Whether `wide` has the shape of a *generated* 8.3 short name, `BASE~N` or `BASE~N.EXT`.
///
/// NTFS keeps a second, short name for every long one on a volume with 8.3 creation on (the system
/// volume by default), and resolves it to the same entry: `HTPASS~1` opens `.htpasswd`. The request path
/// the `dotFiles` policy looked at said `HTPASS~1`, so the policy never applied (issue #617). A name
/// that is *served* under its alias is refused instead: a generated alias is at most 8 characters before
/// the last dot (the base ends in `~` and one or more digits, with at least one character before the
/// `~`) and at most 3 after it, which is what this tests.
///
/// This catches what Windows generates. It does not catch an alias an administrator chose by hand
/// (`fsutil file setshortname`), which needs the same rights as planting a link. A real file named like
/// an alias (`report~1.pdf`) is refused too, on Windows only; a longer base or extension is not.
fn looks_like_short_alias(wide: &[u16]) -> bool {
    let dot = u16::from(b'.');
    let (base, ext) = match wide.iter().rposition(|&c| c == dot) {
        Some(i) => (&wide[..i], &wide[i + 1..]),
        None => (wide, &[][..]),
    };
    // A generated alias has one dot at most, so a dot inside the base is a different kind of name.
    if base.len() > 8 || ext.len() > 3 || base.contains(&dot) {
        return false;
    }
    let digits = base
        .iter()
        .rev()
        .take_while(|&&c| (u16::from(b'0')..=u16::from(b'9')).contains(&c))
        .count();
    // `X~1`: at least one character, the tilde, then one or more digits up to the end of the base.
    digits >= 1 && base.len() > digits + 1 && base[base.len() - digits - 1] == u16::from(b'~')
}

/// Whether a request path segment is a name the Windows open refuses (see [`valid_name`]). Used by the
/// handler to answer "not found" before it builds a response.
pub fn name_is_refused(segment: &str) -> bool {
    let wide: Vec<u16> = segment.encode_utf16().collect();
    !valid_name(&wide)
}

/// `NtCreateFile(FILE_OPEN)` of `name` relative to the open directory `parent`, through `fs_at`.
///
/// With `follow_reparse == false` the name is opened with `FILE_OPEN_REPARSE_POINT`: a link is opened
/// as the link itself rather than followed, and the caller reads its type from the handle.
fn open_relative(
    parent: &File,
    name: &OsStr,
    kind: Kind,
    follow_reparse: bool,
) -> io::Result<File> {
    let wide: Vec<u16> = name.encode_wide().collect();
    if !valid_name(&wide) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut create_options = match kind {
        Kind::Directory => FILE_DIRECTORY_FILE,
        Kind::File => FILE_NON_DIRECTORY_FILE,
    };
    if !follow_reparse {
        create_options |= FILE_OPEN_REPARSE_POINT;
    }
    let mut options = fs_at::OpenOptions::default();
    options
        .object_attributes(OBJ_CASE_INSENSITIVE)
        .desired_access(match kind {
            // A directory is only a stepping stone for the next relative open.
            Kind::Directory => DIRECTORY_ACCESS,
            Kind::File => FILE_GENERIC_READ,
        })
        // `fs_at` adds FILE_SYNCHRONOUS_IO_NONALERT; these replace its defaults, so the type flag and
        // the reparse flag are set here, not left to it.
        .create_options(create_options);
    // `open_path_at` opens whatever the name is (a link is opened as itself, never followed) and
    // reports no error for a link; `open_component` reads the type from the handle.
    options.open_path_at(parent, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fn read(mut f: File) -> String {
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        s
    }

    /// `mklink /J`: a directory junction, which needs no privilege.
    fn junction(link: &Path, target: &Path) {
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink /J failed: {out:?}");
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn opens_nested_regular_files_whatever_the_case() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/b/f.txt"), "hi").unwrap();
        assert_eq!(
            read(open_beneath(root.path(), Path::new("a/b/f.txt")).unwrap()),
            "hi"
        );
        // Lookups are case-insensitive, like `File::open` on a default NTFS volume.
        assert_eq!(
            read(open_beneath(root.path(), Path::new("A/B/F.TXT")).unwrap()),
            "hi"
        );
        assert_eq!(
            open_beneath(root.path(), Path::new("a/missing.txt"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn refuses_a_junction_at_any_level() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        junction(&root.path().join("dir"), outside.path());
        // a directory junction in the middle of the path...
        let err = open_beneath(root.path(), Path::new("dir/secret")).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(LINK_REFUSED));
        // ...and as the last component
        assert!(open_beneath(root.path(), Path::new("dir")).is_err());
        // a junction below a real subdirectory
        std::fs::create_dir(root.path().join("real")).unwrap();
        junction(&root.path().join("real").join("inner"), outside.path());
        assert!(open_beneath(root.path(), Path::new("real/inner/secret")).is_err());
    }

    #[test]
    fn refuses_a_symlink_when_the_machine_can_create_one() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        let file_link = root.path().join("file");
        if let Err(e) =
            std::os::windows::fs::symlink_file(outside.path().join("secret"), &file_link)
        {
            // A developer machine without the symlink privilege cannot run this; CI (an elevated
            // runner) must, so that the file-level refusal is never untested.
            assert!(
                std::env::var_os("CI").is_none(),
                "CI must be able to create symlinks, or this test silently stops covering the refusal ({e})"
            );
            eprintln!("skipping the symlink test: cannot create a symlink here ({e})");
            return;
        }
        std::os::windows::fs::symlink_dir(outside.path(), root.path().join("dir")).unwrap();
        let err = open_beneath(root.path(), Path::new("file")).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(LINK_REFUSED));
        assert!(open_beneath(root.path(), Path::new("dir/secret")).is_err());
    }

    #[test]
    fn the_root_itself_may_be_a_junction() {
        let real = tempfile::tempdir().unwrap();
        std::fs::write(real.path().join("f.txt"), "served").unwrap();
        let holder = tempfile::tempdir().unwrap();
        let root = holder.path().join("current");
        junction(&root, real.path());
        assert_eq!(
            read(open_beneath(&root, Path::new("f.txt")).unwrap()),
            "served"
        );
    }

    #[test]
    fn rejects_non_normal_components() {
        let root = tempfile::tempdir().unwrap();
        for bad in ["../x", "a/../b", "", "C:/Windows/win.ini", "/x"] {
            assert!(open_beneath(root.path(), Path::new(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_plain_names_are_handed_to_the_nt_api() {
        assert!(valid_name(&wide("index.html")));
        assert!(valid_name(&wide("a b.txt")));
        for bad in [
            "",
            "a\\b",
            "a/b",
            "f.txt:stream",
            "C:",
            "dot.",
            "space ",
            "a\0b",
        ] {
            assert!(!valid_name(&wide(bad)), "{bad:?}");
        }
    }

    #[test]
    fn a_reparse_point_that_is_not_a_link_is_reopened_not_refused() {
        // Placeholders and deduplicated files carry FILE_ATTRIBUTE_REPARSE_POINT without being a
        // name-surrogate link: they are served, a symlink or junction is not.
        assert_eq!(verdict(0, false), Verdict::Use);
        assert_eq!(
            verdict(FILE_ATTRIBUTE_REPARSE_POINT, false),
            Verdict::Reopen
        );
        assert_eq!(verdict(FILE_ATTRIBUTE_REPARSE_POINT, true), Verdict::Refuse);
    }

    /// The identity check of the reopen branch: another file on the second open is refused. Removing
    /// the comparison makes this fail.
    #[test]
    fn a_reopen_that_lands_on_another_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "1").unwrap();
        std::fs::write(dir.path().join("b"), "1").unwrap();
        let open = |n: &str| open_beneath(dir.path(), Path::new(n)).unwrap();

        // the same file twice: accepted, and the second handle is the one returned
        let same = reopen_if_same(open("a"), || Ok(open("a"))).unwrap();
        assert!(same_file(&same, &open("a")).unwrap());

        // a different file the second time: refused as a swapped-in link is
        let err = reopen_if_same(open("a"), || Ok(open("b"))).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(LINK_REFUSED));

        // a failing second open is passed on
        let err = reopen_if_same(open("a"), || Err(io::Error::from(io::ErrorKind::NotFound)))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn same_file_tells_files_apart() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "1").unwrap();
        std::fs::write(dir.path().join("b"), "1").unwrap();
        let a1 = open_beneath(dir.path(), Path::new("a")).unwrap();
        let a2 = open_beneath(dir.path(), Path::new("a")).unwrap();
        let b = open_beneath(dir.path(), Path::new("b")).unwrap();
        assert!(same_file(&a1, &a2).unwrap());
        assert!(!same_file(&a1, &b).unwrap());
    }

    /// Manual check for issue #619: the walk against a *real* cloud placeholder (OneDrive Files On-Demand, Dropbox
    /// Smart Sync, any Cloud Files API provider). Run it with the full path of an online-only file:
    ///
    /// `CONDUIT_REPARSE_FILE=<path> cargo test -p lopatnov-conduit-static --features static -- --ignored a_real_cloud_placeholder --nocapture`
    ///
    /// By default it only opens the file (no read, so nothing is downloaded) and prints what it saw: the
    /// attributes of the file as itself, what the first open reports, the verdict, and whether the second open is
    /// the same file. With `CONDUIT_REPARSE_READ=1` it also reads the file through the walk and through a plain
    /// `File::open` and compares the bytes; that DOWNLOADS the placeholder, so use a small one.
    #[test]
    #[ignore = "needs a real cloud placeholder: set CONDUIT_REPARSE_FILE to the path of an online-only file"]
    fn a_real_cloud_placeholder_is_served_like_a_plain_open() {
        let path = std::env::var_os("CONDUIT_REPARSE_FILE").expect("set CONDUIT_REPARSE_FILE");
        let path = Path::new(&path);
        let (root, name) = (path.parent().unwrap(), path.file_name().unwrap());

        // `symlink_metadata` opens the file as itself; a plain `metadata` follows the reparse point through the
        // provider's filter, which can already download a small placeholder.
        let before = std::fs::symlink_metadata(path).expect("the placeholder exists");
        eprintln!(
            "as itself:   attributes {:#010x}, reported size {} bytes",
            before.file_attributes(),
            before.len()
        );
        let dir = OpenOptions::new()
            .access_mode(DIRECTORY_ACCESS)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(root)
            .unwrap();

        // First open, as the walk does it: the file as itself, not followed.
        let first = open_relative(&dir, name, Kind::File, false).expect("first open");
        let fm = first.metadata().unwrap();
        let v = verdict(fm.file_attributes(), fm.file_type().is_symlink());
        eprintln!(
            "first open:  attributes {:#010x}, is_symlink {}, verdict {v:?}",
            fm.file_attributes(),
            fm.file_type().is_symlink()
        );

        // Everything that can make a provider download the file comes last, and the walk goes before it: a
        // provider may hydrate on the second (following) open, and the walk must meet the placeholder, not a
        // file that a diagnostic has already downloaded.
        let read_it = std::env::var_os("CONDUIT_REPARSE_READ").is_some_and(|v| v == "1");

        // The whole walk, which is what the server calls.
        let mut served = open_beneath(root, Path::new(name))
            .expect("the walk must open a placeholder, not refuse it");
        assert!(!served.metadata().unwrap().file_type().is_symlink());
        eprintln!("open_beneath: opened the placeholder without reading it");
        let via_walk = read_it.then(|| {
            let mut bytes = Vec::new();
            served.read_to_end(&mut bytes).unwrap();
            bytes
        });

        // Second open, following the reparse point through the provider's filter, then the identity check.
        let second = open_relative(&dir, name, Kind::File, true).expect("second open");
        eprintln!(
            "second open: attributes {:#010x}, same file as the first: {}",
            second.metadata().unwrap().file_attributes(),
            same_file(&first, &second).unwrap()
        );

        if let Some(via_walk) = via_walk {
            // Bytes, not text: a placeholder need not be UTF-8.
            let via_plain = std::fs::read(path).unwrap();
            eprintln!(
                "read: {} bytes through the walk, {} through a plain open, equal: {}",
                via_walk.len(),
                via_plain.len(),
                via_walk == via_plain
            );
            assert_eq!(
                via_walk, via_plain,
                "the walk must serve what a plain open serves"
            );
        }
    }

    /// Issue #617: the shape of a generated 8.3 short name is refused, ordinary names are not.
    #[test]
    fn a_name_shaped_like_a_generated_short_alias_is_refused() {
        for alias in [
            "HTPASS~1",
            "htpass~1",
            "HTPASS~1.TXT",
            "GIT~1",
            "A~1",
            "ABCDE~12.JS",
            "PROGRA~1",
            "ABC~1234",
        ] {
            assert!(!valid_name(&wide(alias)), "{alias} must be refused");
            assert!(name_is_refused(alias), "{alias} must be refused");
        }
        for ordinary in [
            "index.html",
            "style.css",
            "a~b",
            "~1",
            "~",
            "tilde~",
            "report~1.html",
            "a-very-long-name~1",
            "file~1.tar.gz",
            "a.b~1.txt",
            "ABCDEFGH~1",
            "x~y1",
            "~$doc.docx",
        ] {
            assert!(valid_name(&wide(ordinary)), "{ordinary} must be allowed");
            assert!(!name_is_refused(ordinary), "{ordinary} must be allowed");
        }
    }

    /// `dir /x` style helper: the short path of an existing file, or `None` if the volume keeps no
    /// 8.3 names (then the short path is the long one).
    fn short_path(path: &Path) -> Option<std::path::PathBuf> {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .raw_arg(format!("for %I in (\"{}\") do @echo %~sI", path.display()))
            .output()
            .ok()?;
        let short = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        (short != path && short.exists()).then_some(short)
    }

    /// Issue #617 end to end: `.htpasswd` opened through its real generated alias is refused, the real
    /// name still opens. Needs a volume with 8.3 names (the system volume by default).
    #[test]
    fn a_real_short_alias_does_not_reach_the_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".htpasswd"), "secret").unwrap();
        std::fs::create_dir(root.path().join("somelongdirname")).unwrap();
        std::fs::write(root.path().join("somelongdirname").join("inner.txt"), "x").unwrap();

        let Some(short) = short_path(&root.path().join(".htpasswd")) else {
            eprintln!("skipping: this volume keeps no 8.3 names for the temp directory");
            return;
        };
        let alias = short.file_name().unwrap().to_owned();
        // The alias reaches the real file for a plain open: this is what the walk must not allow.
        assert_eq!(
            std::fs::read_to_string(root.path().join(&alias)).unwrap(),
            "secret"
        );
        assert!(open_beneath(root.path(), Path::new(&alias)).is_err());
        assert_eq!(
            read(open_beneath(root.path(), Path::new(".htpasswd")).unwrap()),
            "secret"
        );

        // A directory alias in the middle of the path is refused the same way.
        if let Some(dir_alias) = short_path(&root.path().join("somelongdirname")) {
            let dir_alias = dir_alias.file_name().unwrap().to_owned();
            let rel = Path::new(&dir_alias).join("inner.txt");
            assert!(open_beneath(root.path(), &rel).is_err());
        }
        assert_eq!(
            read(open_beneath(root.path(), Path::new("somelongdirname/inner.txt")).unwrap()),
            "x"
        );
    }

    /// A handle on a junction (what a directory converted in place would be) is refused as a parent, a
    /// plain directory handle is not; and a lookup through the junction handle returns an error whether
    /// the OS refuses it (error 1921 on Windows 11) or `open_component`'s own check does.
    #[test]
    fn a_parent_that_is_a_link_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        junction(&root.path().join("j"), outside.path());
        let open_dir = |path: &Path, extra: u32| {
            OpenOptions::new()
                .access_mode(DIRECTORY_ACCESS)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | extra)
                .open(path)
                .unwrap()
        };
        let link = open_dir(&root.path().join("j"), FILE_FLAG_OPEN_REPARSE_POINT);
        assert!(link.metadata().unwrap().file_type().is_symlink());
        let err = ensure_parent_plain(&link).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(LINK_REFUSED));
        assert!(ensure_parent_plain(&open_dir(root.path(), 0)).is_ok());
        assert!(open_component(&link, OsStr::new("secret"), Kind::File).is_err());
    }

    /// A directory swapped for a junction while opens are running must never leak outside bytes.
    #[test]
    fn swapping_a_directory_for_a_junction_never_leaks_outside_bytes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("f"), "OUTSIDE").unwrap();
        let (dir, link, parked) = (
            root.path().join("d"),
            root.path().join("j"),
            root.path().join("parked"),
        );
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("f"), "inside").unwrap();
        junction(&link, outside.path());

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let swapper = {
            let (stop, dir, link, parked) = (stop.clone(), dir.clone(), link.clone(), parked);
            std::thread::spawn(move || {
                let mut swaps = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    // d -> parked, junction -> d (readers may now hit it), then everything back.
                    if std::fs::rename(&dir, &parked).is_ok() {
                        if std::fs::rename(&link, &dir).is_ok() {
                            swaps += 1;
                            std::thread::yield_now();
                            let _ = std::fs::rename(&dir, &link);
                        }
                        let _ = std::fs::rename(&parked, &dir);
                    }
                }
                swaps
            })
        };
        let mut opened = 0u32;
        for _ in 0..20_000 {
            if let Ok(f) = open_beneath(root.path(), Path::new("d/f")) {
                opened += 1;
                assert_ne!(read(f), "OUTSIDE");
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let swaps = swapper.join().unwrap();
        assert!(
            opened > 0,
            "the test never opened anything, so it proves nothing"
        );
        assert!(
            swaps > 0,
            "no directory was ever swapped for the junction, so the test proves nothing"
        );
    }
}
