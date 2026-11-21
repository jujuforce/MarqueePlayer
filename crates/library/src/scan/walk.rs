//! The walk: every audio file under the roots, handed over a whole album folder at a time.
//!
//! The scanner writes the library a batch at a time (see `batch.rs`), and an album is always
//! decided with all the tracks of the folder it is grouped in (`group_folder`: the file's own
//! folder, its parent for a "CD1" subfolder, or the stem shared by sibling disc folders, "Live
//! CD1" and "Live CD2"). So the walk hands files over by that folder (a `Unit`), and only once
//! no file of it can still come: every folder that could hold one has been read to its end.
//!
//! A folder's files belong to its key (`dir_key`), and the folders that can hold files of one
//! key are all children of one folder: the key's own folder, its disc subfolders and its sibling
//! disc folders. When a folder is listed, each child folder counts once for its key; when a
//! folder has been walked to its end, it counts no more; a key that nothing counts for any more
//! is complete. Sorted by name, so the same library is always handed over in the same order (an
//! interrupted scan and the one that resumes it go through it alike).
//!
//! Links are followed, as before, except one that leads back to a folder the walk is inside of
//! (a junction to a parent): that would walk round and round.
//!
//! What the walk cannot read it says (`Found::Unreadable`): a folder that does not list (a share's
//! error, no permission, a cloud folder whose provider is not running), a listing cut short, an
//! entry it cannot look at, a link whose target cannot be reached (a drive mounted in a folder,
//! unplugged). Nothing under such a path was seen, so no known track there may be taken for gone
//! (`batch.rs`: they are held). Only "not found" is not an error: a folder or file deleted while
//! the walk was on its way to it is gone.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use super::{FileInfo, group_folder, is_audio};

/// A slow disk, simulated at the scanner's file access (development and tests only): every
/// folder listed and every file opened takes `delay` longer, one at a time across all the
/// scan's threads, as on a single spinning disk or a share whose round trips do not overlap.
/// `MP_SLOW_DISK_MS=<ms>` in the environment, or `Options::slow_disk`. Nothing when zero.
pub(crate) struct Disk {
    delay: Duration,
    lane: Mutex<()>,
}

impl Disk {
    pub(crate) fn new(delay: Option<Duration>) -> Self {
        let delay = delay.unwrap_or_else(|| {
            std::env::var("MP_SLOW_DISK_MS").ok().and_then(|v| v.trim().parse::<u64>().ok()).map_or(Duration::ZERO, Duration::from_millis)
        });
        if !delay.is_zero() {
            log::info!("scan: simulating a slow disk, {} ms a file or folder", delay.as_millis());
        }
        Self { delay, lane: Mutex::new(()) }
    }

    /// Before a folder is listed or a file opened.
    pub(crate) fn wait(&self) {
        if self.delay.is_zero() {
            return;
        }
        let _one_at_a_time = self.lane.lock().unwrap_or_else(|e| e.into_inner());
        std::thread::sleep(self.delay);
    }
}

/// The files of one key (`dir_key`), in walk order, and the key.
pub(crate) struct Unit {
    pub key: String,
    pub files: Vec<FileInfo>,
}

/// The group key of the files directly in `dir`: `group_folder` of any of them, lower case
/// (what `member_of` stores as `group::Member::folder`).
pub(crate) fn dir_key(dir: &Path) -> String {
    group_folder(&dir.join("x")).to_string_lossy().to_lowercase()
}

/// What the walk tells the scan, as it goes.
pub(crate) enum Found {
    /// Listing this folder now.
    Listing(PathBuf),
    /// This many more audio files found.
    Files(usize),
    /// A complete unit.
    Unit(Unit),
    /// This folder (or entry) could not be read, and why: what is under it was not seen.
    Unreadable(PathBuf, String),
    /// Every root walked (or the walk stopped: cancelled).
    Done,
}

pub(crate) struct Walk<'a> {
    disk: &'a Disk,
    cancel: &'a AtomicBool,
    /// The scan stopped (its write failed): nothing more is wanted.
    halt: &'a AtomicBool,
    tell: &'a (dyn Fn(Found) + Sync),
    /// How many folders still being walked can hold files of each key.
    counts: HashMap<String, usize>,
    /// Files of keys not yet complete.
    pending: HashMap<String, Vec<FileInfo>>,
    /// The order keys were first seen in, for the units still pending at the end.
    order: Vec<String>,
}

impl<'a> Walk<'a> {
    pub(crate) fn new(disk: &'a Disk, cancel: &'a AtomicBool, halt: &'a AtomicBool, tell: &'a (dyn Fn(Found) + Sync)) -> Self {
        Self { disk, cancel, halt, tell, counts: HashMap::new(), pending: HashMap::new(), order: Vec::new() }
    }

    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.halt.load(Ordering::Relaxed)
    }

    /// Walk every root that is a folder, then say it is done. Stops early when cancelled.
    pub(crate) fn run(mut self, roots: &[PathBuf]) {
        for root in roots {
            match std::fs::metadata(root) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => continue,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => {
                    self.unreadable(root, &e);
                    continue;
                }
            }
            *self.counts.entry(dir_key(root)).or_default() += 1;
            let mut ancestors = vec![Ancestor { path: root.clone(), canonical: None }];
            self.dir(root, &mut ancestors);
        }
        // Whatever is left (none, unless cancelled: every key's count comes down to zero).
        if !self.stopped() {
            for k in std::mem::take(&mut self.order) {
                if let Some(files) = self.pending.remove(&k) {
                    (self.tell)(Found::Unit(Unit { key: k, files }));
                }
            }
        }
        (self.tell)(Found::Done);
    }

    fn dir(&mut self, dir: &Path, ancestors: &mut Vec<Ancestor>) {
        if self.stopped() {
            return;
        }
        (self.tell)(Found::Listing(dir.to_path_buf()));
        self.disk.wait();
        #[cfg(test)]
        super::tests::listing(dir);
        #[cfg(test)]
        let listed = super::tests::fault(super::tests::Fault::List, dir).map_or_else(|| std::fs::read_dir(dir), Err);
        #[cfg(not(test))]
        let listed = std::fs::read_dir(dir);
        let mut entries: Vec<std::fs::DirEntry> = Vec::new();
        match listed {
            Ok(rd) => {
                let mut cut = false;
                for e in rd {
                    match e {
                        Ok(e) => entries.push(e),
                        // The listing broke off: what it did not reach is not known.
                        Err(e) if !cut => {
                            cut = true;
                            self.unreadable(dir, &e);
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => log::debug!("walk {}: gone meanwhile", dir.display()),
            Err(e) => self.unreadable(dir, &e),
        }
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let key = dir_key(dir);
        let mut subdirs: Vec<PathBuf> = Vec::new();
        let mut files: Vec<FileInfo> = Vec::new();
        for e in entries {
            let path = e.path();
            let ft = match e.file_type() {
                Ok(ft) => ft,
                Err(err) => {
                    self.unreadable(&path, &err);
                    continue;
                }
            };
            // A link (or a junction) is followed: what it points to decides.
            let meta = if ft.is_symlink() { std::fs::metadata(&path) } else { e.metadata() };
            #[cfg(test)]
            let meta = super::tests::fault(super::tests::Fault::Look, &path).map_or(meta, Err);
            let meta = match meta {
                Ok(meta) => meta,
                // Deleted since the listing. Not a link: one that leads nowhere may lead to a
                // drive or share not there now.
                Err(err) if err.kind() == ErrorKind::NotFound && !ft.is_symlink() => continue,
                Err(err) => {
                    self.unreadable(&path, &err);
                    continue;
                }
            };
            if meta.is_dir() {
                if ft.is_symlink() {
                    match leads_back(&path, ancestors) {
                        Ok(false) => {}
                        Ok(true) => {
                            log::debug!("walk: {} leads back to a folder above it; not followed", path.display());
                            continue;
                        }
                        Err(err) => {
                            self.unreadable(&path, &err);
                            continue;
                        }
                    }
                }
                *self.counts.entry(dir_key(&path)).or_default() += 1;
                subdirs.push(path);
            } else if meta.is_file() && is_audio(&path) && !is_appledouble(&path) {
                files.push(file_info(path, &meta));
            }
        }
        if !files.is_empty() {
            (self.tell)(Found::Files(files.len()));
            match self.pending.get_mut(&key) {
                Some(p) => p.extend(files),
                None => {
                    self.order.push(key.clone());
                    self.pending.insert(key.clone(), files);
                }
            }
        }
        for sub in subdirs {
            ancestors.push(Ancestor { path: sub.clone(), canonical: None });
            self.dir(&sub, ancestors);
            ancestors.pop();
            if self.stopped() {
                return;
            }
        }
        self.finished(&key);
    }

    /// `path` could not be read: said, and logged.
    fn unreadable(&self, path: &Path, e: &std::io::Error) {
        log::warn!("walk: cannot read {}: {e}", path.display());
        (self.tell)(Found::Unreadable(path.to_path_buf(), e.to_string()));
    }

    /// A folder of key `key` has been walked to its end.
    fn finished(&mut self, key: &str) {
        let Some(n) = self.counts.get_mut(key) else { return };
        *n -= 1;
        if *n > 0 {
            return;
        }
        self.counts.remove(key);
        if let Some(files) = self.pending.remove(key) {
            self.order.retain(|k| k != key);
            (self.tell)(Found::Unit(Unit { key: key.to_owned(), files }));
        }
    }
}

struct Ancestor {
    path: PathBuf,
    canonical: Option<Option<PathBuf>>,
}

/// Whether the link `path` (a folder) points at one of the folders the walk is inside of; an
/// error when where it points cannot be worked out (it is then not followed).
fn leads_back(path: &Path, ancestors: &mut [Ancestor]) -> std::io::Result<bool> {
    let target = std::fs::canonicalize(path)?;
    Ok(ancestors.iter_mut().any(|a| {
        let canonical = a.canonical.get_or_insert_with(|| std::fs::canonicalize(&a.path).ok());
        canonical.as_deref() == Some(target.as_path())
    }))
}

/// Finder's metadata sidecars share the track's extension. A real track can also start with `._`.
fn is_appledouble(path: &Path) -> bool {
    if !path.file_name().is_some_and(|name| name.as_encoded_bytes().starts_with(b"._")) {
        return false;
    }
    let mut magic = [0; 4];
    std::fs::File::open(path).and_then(|mut file| file.read_exact(&mut magic)).is_ok() && u32::from_be_bytes(magic) == 0x0005_1607
}

fn file_info(path: PathBuf, meta: &std::fs::Metadata) -> FileInfo {
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok());
    // The earliest the file is known to have been here: its creation, or its last change when
    // that is earlier (a copy is created anew but keeps the time it was changed).
    let born = match (meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()), mtime) {
        (Some(c), Some(m)) => c.min(m),
        (c, m) => c.or(m).unwrap_or_default(),
    };
    FileInfo { path, mtime: mtime.map_or(0, |d| d.as_secs() as i64), size: meta.len() as i64, born: born.as_millis() as i64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(root: &Path) -> Vec<(String, Vec<String>)> {
        let disk = Disk::new(Some(Duration::ZERO));
        let (cancel, halt) = (AtomicBool::new(false), AtomicBool::new(false));
        let units = Mutex::new(Vec::new());
        let done = Mutex::new(false);
        let tell = |f: Found| match f {
            Found::Unit(u) => units.lock().unwrap().push(u),
            Found::Done => *done.lock().unwrap() = true,
            _ => {}
        };
        Walk::new(&disk, &cancel, &halt, &tell).run(&[root.to_path_buf()]);
        assert!(*done.lock().unwrap());
        let lower = root.to_string_lossy().to_lowercase();
        units
            .into_inner()
            .unwrap()
            .into_iter()
            .map(|u| {
                let rel = |p: &Path| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                let key = u.key.strip_prefix(lower.as_str()).unwrap_or(&u.key).trim_start_matches(['\\', '/']).replace('\\', "/");
                (key, u.files.iter().map(|f| rel(&f.path)).collect())
            })
            .collect()
    }

    #[test]
    fn only_appledouble_sidecars_are_skipped() {
        let root = crate::testdir::TestDir::new("walk-appledouble");
        std::fs::write(root.join("song.flac"), b"fLaC").unwrap();
        std::fs::write(root.join("._song.flac"), b"\x00\x05\x16\x07\x00\x02\x00\x00").unwrap();
        std::fs::write(root.join("._music.flac"), b"fLaC").unwrap();
        std::fs::write(root.join(".hidden.flac"), b"fLaC").unwrap();
        std::fs::write(root.join("._short.mp3"), b"ID3").unwrap();

        let units = walk(&root);
        assert_eq!(units[0].1, ["._music.flac", "._short.mp3", ".hidden.flac", "song.flac"]);
    }

    /// Disc subfolders and sibling disc folders come with their album, whole, whatever is listed
    /// between them; every other folder comes as soon as it is walked.
    #[test]
    fn a_unit_is_a_whole_album_folder_handed_over_when_complete() {
        let root = crate::testdir::TestDir::new("walk-test");
        let _ = std::fs::remove_dir_all(&root);
        for f in [
            "A/Album/01.flac",
            "A/Album/CD1/01.flac",
            "A/Album/CD2/01.flac",
            "A/Live/01.flac",
            "A/Live At Home/01.flac",
            "A/Live CD2/01.flac",
            "A/live (Disc 3)/01.flac",
            "A/Other/01.flac",
            "A/Other/cover.jpg",
            "B/Single.mp3",
            "loose.ogg",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        let units = walk(&root);
        let keys: Vec<&str> = units.iter().map(|u| u.0.as_str()).collect();
        assert_eq!(keys, vec!["a/album", "a/live at home", "a/other", "a/live", "b", ""], "{units:?}");
        assert_eq!(units[3].1, vec!["A/Live/01.flac", "A/Live CD2/01.flac", "A/live (Disc 3)/01.flac"]);
        assert_eq!(units[0].1, vec!["A/Album/01.flac", "A/Album/CD1/01.flac", "A/Album/CD2/01.flac"]);
        assert!(units.iter().all(|u| u.1.iter().all(|f| !f.ends_with(".jpg"))));
        let _ = std::fs::remove_dir_all(&root);
    }
}
