//! Linux: the file manager, the browser and the clipboard, by freedesktop conventions.
//!
//! "Show in file manager" asks the desktop's file manager over D-Bus
//! (`org.freedesktop.FileManager1.ShowItems`, which Dolphin, Nautilus, Nemo, Caja, Thunar
//! and others implement: a window with the file selected); without it, `xdg-open` opens
//! the folder. Web pages open with `xdg-open` (the desktop's browser). The clipboard is
//! Slint's (Wayland or X11, whichever the window is on), through a text field nobody sees
//! (`clipboard-copy` in app.slint). Only ever reads: nothing is opened, moved or changed
//! but a window. The D-Bus call runs on a thread of its own (a file manager started by
//! activation can take a while); the UI never waits for it.

use std::path::{Path, PathBuf};

/// Library paths are native here.
pub fn native_path(path: &str) -> PathBuf {
    PathBuf::from(path)
}

/// `file:///…`, every byte but the unreserved ones and `/` escaped.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

/// A file manager window with `target` selected in its folder (`select`), or the folder
/// `target` opened.
pub fn show_item(target: &Path, select: bool) -> bool {
    let (target, uri) = (target.to_path_buf(), file_uri(target));
    let spawned = std::thread::Builder::new().name("mp-show".into()).spawn(move || {
        let method = if select { "ShowItems" } else { "ShowFolders" };
        let asked = super::dbus::session().is_some_and(|bus| {
            bus.call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                method,
                &(vec![uri.as_str()], ""),
            )
            .inspect_err(|e| log::info!("FileManager1.{method}: {e}; opening the folder instead"))
            .is_ok()
        });
        if !asked {
            let folder = if select { target.parent().map(Path::to_path_buf).unwrap_or(target.clone()) } else { target.clone() };
            xdg_open(folder.as_os_str());
        }
    });
    spawned.is_ok()
}

/// A folder of the app's own (Settings > About) in a file manager window.
pub fn open_folder(target: &Path) -> bool {
    show_item(target, false)
}

/// What "Show in file manager" would do, for the bench's report (nothing is run).
#[cfg(feature = "bench")]
pub fn describe_show(path: &str) -> String {
    format!("org.freedesktop.FileManager1.ShowItems([\"{}\"]), else xdg-open on its folder", file_uri(Path::new(path)))
}

/// Open a web page in the desktop's browser.
pub fn open_url(url: &str) -> bool {
    xdg_open(std::ffi::OsStr::new(url))
}

fn xdg_open(what: &std::ffi::OsStr) -> bool {
    open_with(std::ffi::OsStr::new("xdg-open"), what)
}

/// `what` handed to `program` as its one argument (a link with spaces, `&` or quotes stays one).
fn open_with(program: &std::ffi::OsStr, what: &std::ffi::OsStr) -> bool {
    match std::process::Command::new(program).arg(what).stdin(std::process::Stdio::null()).spawn() {
        Ok(mut child) => {
            // Reaped on a thread of its own (xdg-open exits once it handed the job over).
            let _ = std::thread::Builder::new().name("mp-xdg-open".into()).spawn(move || child.wait());
            true
        }
        Err(e) => {
            log::warn!("could not start xdg-open: {e}");
            false
        }
    }
}

/// Put text on the clipboard (View metadata's Copy). False when it could not.
pub fn copy_text(text: &str) -> bool {
    let text: slint::SharedString = text.into();
    crate::state::with(|st| st.app.upgrade().map(|app| app.invoke_clipboard_copy(text))).flatten().is_some()
}

/// Text from the clipboard (the equalizer's Paste), through the toolkit's clipboard
/// (Wayland or X11): `Ok(None)` when it holds no text, an error when it holds more than
/// `max_bytes`.
pub fn paste_text(max_bytes: usize) -> Result<Option<String>, String> {
    let text = crate::state::with(|st| st.app.upgrade().map(|app| app.invoke_clipboard_paste())).flatten().ok_or_else(|| "Could not read the clipboard".to_owned())?;
    if text.is_empty() {
        Ok(None)
    } else if text.len() > max_bytes {
        Err(format!("The clipboard holds too much text to paste ({:.1} MB)", text.len() as f64 / 1_048_576.0))
    } else {
        Ok(Some(text.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_reaches_the_opener_as_one_argument() {
        // A stand-in for xdg-open (no browser is started): it writes what it was given.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("mp-xdg-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (script, got) = (dir.join("opener"), dir.join("got"));
        std::fs::write(&script, format!("#!/bin/sh
printf '%s|%s' \"$#\" \"$1\" > '{}'
", got.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test thread forking while the script was open for writing leaves its child
        // holding that descriptor until it execs: executing the script fails meanwhile ("text
        // file busy"). Once it runs, no writer is left.
        let t0 = std::time::Instant::now();
        while let Err(e) = std::process::Command::new(&script).arg("probe").status() {
            assert!(e.raw_os_error() == Some(libc::ETXTBSY) && t0.elapsed() < std::time::Duration::from_secs(10), "the stand-in opener: {e}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for url in [listenbrainz::TOKEN_PAGE, "https://example.org/a b?x=1&y='2'"] {
            let _ = std::fs::remove_file(&got);
            assert!(open_with(script.as_os_str(), std::ffi::OsStr::new(url)));
            let t0 = std::time::Instant::now();
            while !got.exists() && t0.elapsed() < std::time::Duration::from_secs(10) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            assert_eq!(std::fs::read_to_string(&got).unwrap(), format!("1|{url}"));
        }
        assert!(!open_with(dir.join("no-such-opener").as_os_str(), std::ffi::OsStr::new("x")), "no opener: false, nothing else");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_uris_escape_what_a_uri_cannot_hold() {
        assert_eq!(file_uri(Path::new("/home/me/Music/Sigur Rós/01 #1.flac")), "file:///home/me/Music/Sigur%20R%C3%B3s/01%20%231.flac");
        assert_eq!(file_uri(Path::new("/a/b-c_d.e~f")), "file:///a/b-c_d.e~f");
    }
}
