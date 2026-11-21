//! macOS: the Finder, the browser and the clipboard. "Show in Finder" is `open -R` (a Finder
//! window with the file selected), a folder or a web page `open` (the Finder, the default
//! browser). The clipboard is the general pasteboard (`NSPasteboard`), plain text. Only ever
//! reads: nothing is opened, moved or changed but a window.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Library paths are native here.
pub fn native_path(path: &str) -> PathBuf {
    PathBuf::from(path)
}

/// A Finder window with `target` selected in its folder (`select`), or the folder `target`
/// opened.
pub fn show_item(target: &Path, select: bool) -> bool {
    if select { open(&[OsStr::new("-R"), target.as_os_str()]) } else { open(&[target.as_os_str()]) }
}

/// A folder of the app's own (Settings > About) in a Finder window.
pub fn open_folder(target: &Path) -> bool {
    show_item(target, false)
}

/// What "Show in Finder" would do, for the bench's report (nothing is run).
#[cfg(feature = "bench")]
pub fn describe_show(path: &str) -> String {
    format!("open -R \"{path}\"")
}

/// Open a web page in the default browser.
pub fn open_url(url: &str) -> bool {
    open(&[OsStr::new(url)])
}

/// `/usr/bin/open` with `args`, each one argument (a link with spaces, `&` or quotes stays one).
fn open(args: &[&OsStr]) -> bool {
    match std::process::Command::new("/usr/bin/open").args(args).stdin(std::process::Stdio::null()).spawn() {
        Ok(mut child) => {
            // Reaped on a thread of its own (open exits once Launch Services has the job).
            let _ = std::thread::Builder::new().name("mp-open".into()).spawn(move || child.wait());
            true
        }
        Err(e) => {
            log::warn!("could not start open: {e}");
            false
        }
    }
}

/// Put text on the clipboard (View metadata's Copy). False when it could not.
pub fn copy_text(text: &str) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    let board = NSPasteboard::generalPasteboard();
    board.clearContents();
    // SAFETY: AppKit's constant, read once.
    let kind = unsafe { NSPasteboardTypeString };
    board.setString_forType(&objc2_foundation::NSString::from_str(text), kind)
}

/// Text from the clipboard (the equalizer's Paste): `Ok(None)` when it holds no text, an
/// error when it holds more than `max_bytes`.
pub fn paste_text(max_bytes: usize) -> Result<Option<String>, String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    let board = NSPasteboard::generalPasteboard();
    // SAFETY: as above.
    let kind = unsafe { NSPasteboardTypeString };
    let Some(text) = board.stringForType(kind) else { return Ok(None) };
    let text = text.to_string();
    if text.is_empty() {
        Ok(None)
    } else if text.len() > max_bytes {
        Err(format!("The clipboard holds too much text to paste ({:.1} MB)", text.len() as f64 / 1_048_576.0))
    } else {
        Ok(Some(text))
    }
}
