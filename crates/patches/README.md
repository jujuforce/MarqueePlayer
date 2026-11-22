# Patched dependencies

Copies of crates.io dependencies with a fix the app needs before upstream has it, wired in by
`[patch.crates-io]` in the workspace `Cargo.toml`. Each keeps its own licence files. Remove a
copy (and its `[patch]` line) once a release upstream has the fix.

## blocking 1.7.0

`blocking` is the thread pool `zbus` uses (on Linux: the desktop's media controls, the tray,
the accessibility bridge). In 1.7.0 an idle pool thread retires only when the pool also has
more threads than its limit (`&&`), so the last one never does: it wakes every 500 ms for the
life of the process, also while the app is paused and hidden (measured: 120 wakeups a minute,
all from `blocking-1`). The patch makes either condition retire it (`||`: too many threads, or
500 ms without work), as the code's own comments describe; the pool starts a thread again
when work comes. One line, in `src/lib.rs` (`main_loop`), marked "Marquee Player's patch".
Upstream: https://github.com/smol-rs/blocking (the same code on master as of 2026-11).
The Windows build does not depend on this crate.


## softbuffer 0.4.8

The released Core Graphics backend allocates a fresh framebuffer for every draw and reports
a buffer age of zero. Slint consequently redraws the whole Retina window when the seek
position moves. The patched backend retains the previous frame and gives Core Animation
immutable 256-pixel tiles. It compares damaged tiles with their previously published pixels
and uploads the ones that changed. A resize or an abandoned mutable frame forces a full repaint.

The app opts its software surface into premultiplied alpha and names its foreground layer
so that the visualizer can run beneath the Slint controls. UIKit and ordinary softbuffer surfaces keep their original
full-frame XRGB presentation. The changes are in `src/backends/cg.rs` and the macOS re-export in
`src/lib.rs`. Review this patch on the next major softbuffer update. Its development tree
already supports buffer reuse and premultiplied alpha. [Upstream Core Graphics backend](https://github.com/rust-windowing/softbuffer/blob/master/src/backends/cg.rs)
