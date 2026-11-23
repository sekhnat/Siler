//! Opening the files a download writes before it is published (`.siler.part`, a media
//! object's `.part`, the unpack scratch), so that nothing already at that path is written
//! through: not a symbolic link, and not a file with other names (a hard link), whose data
//! another path would share. Files Siler creates are readable by their owner only.
//!
//! These checks are on path names, so they can't stop someone who can write to the download
//! folder from racing them; they keep a stray or planted entry from being used.

use std::fs::{File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

const OWNER_ONLY: u32 = 0o600;

/// A new file at `path`; fails if anything at all is there already.
pub(crate) fn create(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(OWNER_ONLY)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| unsafe_if_exists(e, path))
}

/// The partial file at `path`, to be continued: it must be a plain file with no other
/// names. Its permissions are tightened to the owner's (partials from before were 0644).
pub(crate) fn resume(path: &Path) -> Result<File> {
    check(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| unsafe_if_link(e, path))?;
    // Checked again on the open file: the path may have changed since.
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(unsafe_part(path));
    }
    file.set_permissions(Permissions::from_mode(OWNER_ONLY))?;
    Ok(file)
}

/// An empty file at `path`, replacing an earlier partial of Siler's own that is left there
/// (one that is being started over). Anything else at the path is refused, not removed.
pub(crate) fn replace(path: &Path) -> Result<File> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(_) => {
            check(path)?;
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
    }
    create(path)
}

/// Gives a finished file the permissions a newly made file gets (`0666` less the umask), as
/// published downloads had before partials were made owner-only. `path` is Siler's own file,
/// about to be renamed to its final name.
pub(crate) fn publishable(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(unsafe_part(path));
    }
    std::fs::set_permissions(path, Permissions::from_mode(0o666 & !umask()))?;
    Ok(())
}

/// The process's umask, read without changing it (`umask(2)` can only set it); 022 when
/// it can't be read.
fn umask() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Umask:"))
                .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
        })
        .unwrap_or(0o022)
}

/// [`resume`], for async callers.
pub(crate) async fn resume_async(path: PathBuf) -> Result<tokio::fs::File> {
    blocking(move || resume(&path)).await
}

/// [`replace`], for async callers.
pub(crate) async fn replace_async(path: PathBuf) -> Result<tokio::fs::File> {
    blocking(move || replace(&path)).await
}

async fn blocking(f: impl FnOnce() -> Result<File> + Send + 'static) -> Result<tokio::fs::File> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Protocol(e.to_string()))?
        .map(tokio::fs::File::from_std)
}

/// A plain file with no other names, without following a link at `path`.
fn check(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_file() && meta.nlink() == 1 {
        Ok(())
    } else {
        Err(unsafe_part(path))
    }
}

fn unsafe_part(path: &Path) -> Error {
    Error::UnsafePart(path.to_path_buf())
}

fn unsafe_if_exists(e: io::Error, path: &Path) -> Error {
    if e.kind() == io::ErrorKind::AlreadyExists {
        unsafe_part(path)
    } else {
        unsafe_if_link(e, path)
    }
}

/// `O_NOFOLLOW` on a symbolic link fails with ELOOP.
fn unsafe_if_link(e: io::Error, path: &Path) -> Error {
    if e.raw_os_error() == Some(libc::ELOOP) {
        unsafe_part(path)
    } else {
        e.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn is_unsafe(r: Result<File>) -> bool {
        matches!(r, Err(Error::UnsafePart(_)))
    }

    #[test]
    fn creates_owner_only_and_never_over_anything() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.siler.part");
        create(&p).unwrap();
        assert_eq!(mode(&p), 0o600);
        // Something there already, even our own file.
        assert!(is_unsafe(create(&p)));
    }

    #[test]
    fn never_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("precious");
        std::fs::write(&target, b"keep me").unwrap();
        let p = dir.path().join("a.siler.part");
        std::os::unix::fs::symlink(&target, &p).unwrap();
        assert!(is_unsafe(create(&p)));
        assert!(is_unsafe(resume(&p)));
        assert!(is_unsafe(replace(&p)));
        // Neither the link nor what it points at was touched.
        assert!(
            std::fs::symlink_metadata(&p)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
    }

    #[test]
    fn refuses_a_file_with_other_names() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other");
        std::fs::write(&other, b"shared").unwrap();
        let p = dir.path().join("a.siler.part");
        std::fs::hard_link(&other, &p).unwrap();
        assert!(is_unsafe(resume(&p)));
        assert!(is_unsafe(replace(&p)));
        assert_eq!(std::fs::read(&other).unwrap(), b"shared");
        assert!(p.exists());
    }

    #[test]
    fn refuses_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.siler.part");
        std::fs::create_dir(&p).unwrap();
        assert!(is_unsafe(resume(&p)));
        assert!(is_unsafe(replace(&p)));
        assert!(p.is_dir());
    }

    #[test]
    fn resumes_an_old_partial_and_tightens_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.siler.part");
        std::fs::write(&p, b"0123").unwrap();
        std::fs::set_permissions(&p, Permissions::from_mode(0o644)).unwrap();
        let mut f = resume(&p).unwrap();
        assert_eq!(mode(&p), 0o600);
        assert_eq!(f.metadata().unwrap().len(), 4);
        use std::io::Seek;
        f.seek(std::io::SeekFrom::End(0)).unwrap();
        f.write_all(b"45").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"012345");
    }

    #[test]
    fn replaces_our_own_stale_partial() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.siler.part");
        std::fs::write(&p, b"stale data").unwrap();
        std::fs::set_permissions(&p, Permissions::from_mode(0o644)).unwrap();
        let f = replace(&p).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 0);
        assert_eq!(mode(&p), 0o600);
        // And with nothing there, it simply creates one.
        let q = dir.path().join("b.siler.part");
        replace(&q).unwrap();
        assert_eq!(mode(&q), 0o600);
    }
}
