//! A secret kept on disk (the server's token, `server/account.rs`): on Linux and macOS, in a file only
//! its user can read (mode 0600), as `~/.netrc` or an SSH key is; nothing sealed (there is no
//! keyring every Linux desktop has, the AppImage's portable folder moves between machines, and
//! the macOS Keychain asks the user again for every unsigned build). On a drive without
//! Unix permissions (FAT or exFAT, where a portable folder may be) the mode does not hold, and
//! the file is as private as the drive: the log says so.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// How `seal` protects, as the file names it.
pub const SCHEME: &str = "file-0600";

pub fn seal(plain: &[u8]) -> Option<Vec<u8>> {
    Some(plain.to_vec())
}

pub fn open(sealed: &[u8]) -> Option<Vec<u8>> {
    Some(sealed.to_vec())
}

/// Write a file only this user can read: made new with mode 0600 (what was at `path` before,
/// a file or a link, is removed first, so the secret is never written through a link into a
/// file somewhere else), and set so again should the umask have taken bits away.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).custom_flags(libc::O_NOFOLLOW).mode(0o600).open(path)?;
    keep_private(f.set_permissions(std::fs::Permissions::from_mode(0o600)))?;
    if f.metadata().is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
        log::warn!("{} can be read by other users: its drive does not keep Unix permissions", path.display());
    }
    f.write_all(bytes)?;
    f.sync_all()
}

/// What setting the mode came to: a drive that keeps no Unix permissions (FAT, exFAT: the
/// system refuses or cannot) is no failure, as the check after it says so; anything else is.
fn keep_private(set: std::io::Result<()>) -> std::io::Result<()> {
    match set {
        Err(e) if matches!(e.kind(), std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported) => Ok(()),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_the_users_alone() {
        let dir = std::env::temp_dir().join(format!("mp-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.json");
        std::fs::write(&p, b"old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&p, b"new").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_drive_without_unix_permissions_does_not_stop_the_save() {
        use std::io::{Error, ErrorKind};
        // FAT and exFAT answer EPERM (or "not supported") to a mode change: saved anyway, and
        // the check after it warns.
        assert!(keep_private(Err(Error::from_raw_os_error(libc::EPERM))).is_ok());
        assert!(keep_private(Err(Error::from(ErrorKind::Unsupported))).is_ok());
        assert!(keep_private(Ok(())).is_ok());
        // Anything else still stops it.
        assert_eq!(keep_private(Err(Error::from_raw_os_error(libc::EIO))).unwrap_err().kind(), Error::from_raw_os_error(libc::EIO).kind());
    }

    #[test]
    fn a_link_at_the_path_is_not_written_through() {
        let dir = std::env::temp_dir().join(format!("mp-secret-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let elsewhere = dir.join("elsewhere.txt");
        std::fs::write(&elsewhere, b"someone else's").unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o644)).unwrap();
        let p = dir.join("s.json.tmp");
        std::os::unix::fs::symlink(&elsewhere, &p).unwrap();
        write_private(&p, b"secret").unwrap();
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"someone else's", "the link's target is untouched");
        assert_eq!(std::fs::metadata(&elsewhere).unwrap().permissions().mode() & 0o777, 0o644);
        let m = std::fs::symlink_metadata(&p).unwrap();
        assert!(m.file_type().is_file(), "a file of its own in the link's place");
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&p).unwrap(), b"secret");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
