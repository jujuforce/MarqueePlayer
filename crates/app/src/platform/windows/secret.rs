//! A secret kept on disk (the server's token, `server/account.rs`), sealed with DPAPI for the current
//! Windows user (`CryptProtectData`): only this user on this PC can open it again. Copied to
//! another PC (the portable edition's drive) or another account, it does not open, and the app
//! asks to sign in again. The file itself is written as any other.

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData};

/// How `seal` protects, as the file names it.
pub const SCHEME: &str = "dpapi";

/// What DPAPI made of `blob`, copied out and its buffer freed.
fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI gave `cbData` bytes at `pbData`, allocated with LocalAlloc; read once, freed once.
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe { LocalFree(out.pbData.cast()) };
    bytes
}

/// `plain` sealed for this user; None if DPAPI refuses.
pub fn seal(plain: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB { cbData: u32::try_from(plain.len()).ok()?, pbData: plain.as_ptr().cast_mut() };
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `plain` for the call (DPAPI only reads it); `out` is filled by
    // DPAPI and freed in `take`. No prompt (UI forbidden), no entropy, no description.
    let ok = unsafe { CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) };
    (ok != 0).then(|| take(out))
}

/// What `seal` sealed, opened again; None when it cannot be (another user or PC, damage).
pub fn open(sealed: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB { cbData: u32::try_from(sealed.len()).ok()?, pbData: sealed.as_ptr().cast_mut() };
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `seal`.
    let ok = unsafe { CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) };
    (ok != 0).then(|| take(out))
}

/// Write a file only this user should read. On Windows the data folder is the user's own
/// (`%APPDATA%`, or the portable drive): an ordinary write; the secret in it is sealed.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_secret_opens_for_this_user_only_whole() {
        let sealed = seal(b"salt\ntoken").unwrap();
        assert!(!sealed.windows(5).any(|w| w == b"token"), "not readable as is");
        assert_eq!(open(&sealed).as_deref(), Some(&b"salt\ntoken"[..]));
        let mut damaged = sealed.clone();
        let mid = damaged.len() / 2;
        damaged[mid] ^= 0x55;
        assert_eq!(open(&damaged), None);
        assert_eq!(open(b"not dpapi at all"), None);
    }
}
