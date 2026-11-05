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
//! - lookups are case-insensitive, like `File::open` on a default NTFS volume;
//! - the handle is opened for synchronous I/O, so it works with `std::fs::File` unchanged.
//!
//! A reparse point that is *not* a link (a OneDrive placeholder, a deduplicated file) cannot be
//! read through a `FILE_OPEN_REPARSE_POINT` handle, because the filter that serves it does not run.
//! Such a component is reopened normally, relative to the same verified parent, and must be the
//! same file (volume serial and 128-bit file id) as the first open, so a link swapped in meanwhile
//! is refused. That branch has not been run against a real placeholder or deduplicated file (only
//! reparse tags without a handler, which fail closed): it is written from the API contract.
//!
//! The only `unsafe` is the FFI in [`open_relative`] and [`file_id`]; the crate denies it everywhere
//! else (`#![deny(unsafe_code)]` in `lib.rs`).

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Component, Path};
use std::ptr;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{
    RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// `ERROR_STOPPED_ON_SYMLINK`: what a refused link is reported as.
const ERROR_STOPPED_ON_SYMLINK: i32 = 681;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const STATUS_SUCCESS: i32 = 0;
/// What a directory handle needs: to be a root for relative opens and to report its type. No listing
/// right, so a directory the account can only traverse still works.
const DIRECTORY_ACCESS: u32 = FILE_READ_ATTRIBUTES | SYNCHRONIZE;

fn link_error() -> io::Error {
    io::Error::from_raw_os_error(ERROR_STOPPED_ON_SYMLINK)
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

fn verdict(attributes: u32, is_link: bool) -> Verdict {
    if is_link {
        Verdict::Refuse
    } else if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Verdict::Reopen
    } else {
        Verdict::Use
    }
}

fn open_component(parent: &File, name: &OsStr, kind: Kind) -> io::Result<File> {
    let first = open_relative(parent, name, kind, false)?;
    let meta = first.metadata()?;
    match verdict(meta.file_attributes(), meta.file_type().is_symlink()) {
        Verdict::Use => Ok(first),
        Verdict::Refuse => Err(link_error()),
        Verdict::Reopen => reopen_if_same(first, || open_relative(parent, name, kind, true)),
    }
}

/// The second open of a non-link reparse point must be the same file as the first.
fn reopen_if_same(first: File, open_second: impl FnOnce() -> io::Result<File>) -> io::Result<File> {
    let second = open_second()?;
    if file_id(&first)? == file_id(&second)? {
        Ok(second)
    } else {
        // The name now resolves to something else: swapped in between the two opens.
        Err(link_error())
    }
}

/// One plain path component: what `NtCreateFile` may be handed as a relative name.
fn valid_name(wide: &[u16]) -> bool {
    const BAD: [u16; 4] = [b'\\' as u16, b'/' as u16, b':' as u16, 0];
    let (dot, space) = (u16::from(b'.'), u16::from(b' '));
    !wide.is_empty()
        && !wide.iter().any(|c| BAD.contains(c))
        && !matches!(wide.last(), Some(&c) if c == dot || c == space)
}

/// `NtCreateFile(FILE_OPEN)` of `name` relative to the open directory `parent`.
///
/// With `follow_reparse == false` the name is opened with `FILE_OPEN_REPARSE_POINT`: a link is opened
/// as the link itself rather than followed.
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
    let bytes =
        u16::try_from(wide.len() * 2).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let object_name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: wide.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut options = FILE_SYNCHRONOUS_IO_NONALERT
        | match kind {
            Kind::Directory => FILE_DIRECTORY_FILE,
            Kind::File => FILE_NON_DIRECTORY_FILE,
        };
    if !follow_reparse {
        options |= FILE_OPEN_REPARSE_POINT;
    }

    let mut handle: HANDLE = ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `handle` and `status_block` are valid for writes; `attributes` and everything it points
    // to (`object_name`, the UTF-16 buffer `wide`) outlive the call; `RootDirectory` is a handle owned by
    // `parent`, which the caller keeps alive; the null pointers are the documented "none" values for
    // the allocation size, the security fields and the extended-attribute buffer.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            match kind {
                // A directory is only a stepping stone for the next relative open.
                Kind::Directory => DIRECTORY_ACCESS,
                Kind::File => FILE_GENERIC_READ,
            },
            &attributes,
            &mut status_block,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            options,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: a plain status-to-error conversion, no pointers involved.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    if handle.is_null() {
        return Err(io::Error::other(format!(
            "NtCreateFile returned status {status:#x} and no handle"
        )));
    }
    // SAFETY: the call did not fail and returned a handle, so `handle` is a valid handle that nothing else
    // owns. Taking ownership first means every early return below closes it.
    let owned = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
    // A synchronous FILE_OPEN returns STATUS_SUCCESS and nothing else; anything different is not trusted.
    if status != STATUS_SUCCESS {
        return Err(io::Error::other(format!(
            "unexpected NtCreateFile status {status:#x}"
        )));
    }
    Ok(File::from(owned))
}

/// Volume serial number and 128-bit file id: which file a handle refers to. The 128-bit form is the one
/// that is unique on ReFS (and Dev Drive); NTFS fills its 64-bit index into the low bytes.
fn file_id(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: `info` is a valid, writable `FILE_ID_INFO` and the size passed is its size; the handle is
    // open for the lifetime of `file`.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            ptr::from_mut(&mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

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
        assert_eq!(err.raw_os_error(), Some(ERROR_STOPPED_ON_SYMLINK));
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
        assert_eq!(err.raw_os_error(), Some(ERROR_STOPPED_ON_SYMLINK));
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
        assert_eq!(file_id(&same).unwrap(), file_id(&open("a")).unwrap());

        // a different file the second time: refused as a swapped-in link is
        let err = reopen_if_same(open("a"), || Ok(open("b"))).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(ERROR_STOPPED_ON_SYMLINK));

        // a failing second open is passed on
        let err = reopen_if_same(open("a"), || Err(io::Error::from(io::ErrorKind::NotFound)))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn file_id_tells_files_apart() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "1").unwrap();
        std::fs::write(dir.path().join("b"), "1").unwrap();
        let a1 = open_beneath(dir.path(), Path::new("a")).unwrap();
        let a2 = open_beneath(dir.path(), Path::new("a")).unwrap();
        let b = open_beneath(dir.path(), Path::new("b")).unwrap();
        assert_eq!(file_id(&a1).unwrap(), file_id(&a2).unwrap());
        assert_ne!(file_id(&a1).unwrap(), file_id(&b).unwrap());
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
