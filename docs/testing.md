# Testing

The bench runner, the scripted benches and the environment variables for measurement and tests.

## The bench runner

`cargo xtask bench` (on Linux inside a desktop session or
`packaging/linux/headless-session.sh`) builds the app with the bench harness, the fixture tools and
`crates/bench-runner` at below-normal priority (in `target/bench` by default), then runs every
bench mode in sequence and prints one pass/fail table; the arguments after `--` go to the
runner, and it exits with the runner's exit code:

    cargo xtask bench                       # everything (about half an hour)
    cargo xtask bench -- --only skip,gapless
    cargo xtask bench -- --list
    cargo xtask bench --profile fast -- --quick   # while working

While working on a change, use the `fast` profile (`Cargo.toml`; `--profile fast`, and
`cargo build --profile fast` for the app alone): the same
optimization without the whole-program link step, so a one-line change rebuilds in 10 to 20
seconds where the release profile takes 3 to 5 minutes. `--quick` leaves out the modes that are
long by design (`soak`, `bigscan`, `slowscan`). What a mode checks is the same on either profile;
frame and CPU timings, the exe's size and the bit-exact audio tests are only meaningful on the
release profile, which the full pass and `cargo xtask release` use.

The runner (`crates/bench-runner`, never shipped):

- **Fixtures.** `cargo run --release -p library --example bench_fixtures -- <dir>` makes every set
  any mode needs, in one folder marked `.mp-bench-fixtures` inside the temporary folder (it refuses
  any other place): `core` (2,550 tracks: the albums the first benches name, "Lemonade", "Night
  Harbour", "Sampler", "Groove", 300 grid albums with covers, a 5.1 AAC track and a damaged FLAC
  in albums of their own, unreadable files), `formats` (the technical details' albums), `words` (the
  lyrics album), and through the other tools `viz`, `messy` (240 albums), `messy120` (the smart
  playlist bench's 120), `meta` and `edit` (marked `.mp-edit-fixtures`). The runner makes them in
  `%TEMP%\mp-bench-fixtures` the first time, and `big` (about 29,000 tracks, `messy_library` with
  2,600 albums, 600 MB) the first time `bigscan` runs.
- **Each mode** gets a fresh copy of its sets (the editing and metadata benches change theirs), a
  fresh data folder with a connected Last.fm session, and local mocks of LRCLIB (the "Words" album's
  titles say what it answers) and Last.fm, at `MP_LRCLIB_API` and `MP_LASTFM_API`: nothing leaves
  the machine. Always `MP_VOLUME=0` and `MP_WINDOW=secondary`; bench builds open no native dialog
  and never touch the real clipboard (`explorer::copy_text` keeps the text for the bench).
- **Verdicts.** Every bench module counts its failed checks in one place (`bench::count_fail`, and
  `bench::verdict` for the modes that only reported text before); the app exits with code 3 when any
  failed. The runner: exit 0 and no `[FAIL]` line is PASS, 3 is FAIL, 101 or "panicked" in the log
  is PANIC, any other code CRASH, a run outliving its limit HANG (stopped by its handle). Reports,
  snapshots, the log and stderr of each mode are kept in `%TEMP%\mp-bench-runs\<mode>`.
- **`playlist`** (`bench/mod.rs`, then `bench/playlist.rs` and `bench/selection.rs`; the `core` set): a playlist of the library's
  first 3,000 tracks (2,550), how fast it opens, its scroll kept through Back; then Shuffle on it
  and playlist name clashes (`bench/playlist.rs`); then its selection
  by clicks with Ctrl and Shift held (key events of their own, then the click, all through the
  window's own dispatch), rows found by right clicks down the page: ranges, toggles, the moving
  anchor, Ctrl+A and Shift+End on every row (timed), Esc, the arrows; the menu of several tracks
  and each of its actions; a real drag of two chosen rows within the playlist; Delete; the
  selection gone when the list or the page changes and a menu opened before then doing nothing;
  the album page's list; the queue's Delete, block move and Esc. Snapshots `select-*.png`.
- **`server`** (`bench/server.rs`, the `formats` set): a Navidrome server against a stand-in the app
  starts on 127.0.0.1 (`subsonic::mock`): sign-in, the sync and its covers, two albums matched by
  their files, the server's own tracks, favourites and plays written back (never a rating, and none
  with the switches off), Stop, the server gone, the shrink guard, a refused sign-in, signing
  out, the cloud on the server's music, the top right of Home for every sync and rescan, an
  empty library's Home (see [Measurement and test hooks](#measurement-and-test-hooks)).
- **`listenbrainz`** (`bench/listenbrainz.rs`, the `core` set): Settings > Connections > ListenBrainz against a
  stand-in the app starts on 127.0.0.1 (`listenbrainz::mock`), reached through the card's own
  server address (plain http, noted): "Find a setting", the token page asked for and not opened, a
  token refused, the right one (the field emptied, `listenbrainz.json` without it), now playing
  and the listen of a track played, the server down then back (one `import`), a 429 waited out,
  the token refused while connected (the section marked, the queue kept) and connecting again,
  Copy diagnostics' line, love on and off, Disconnect, the log without the token; every request
  checked against the documented rules. Snapshots `lb-*.png`.
- **`streaming`** (`bench/streaming.rs`, no set): server-only tracks against a local mock of a Navidrome
  server inside the app (see [Measurement and test hooks](#measurement-and-test-hooks)).
- **`lyrics`** (`bench/lyrics.rs`, the `core` set): the Lyrics tab's timing (the buttons' callback and
  `G` through the window's key dispatch, the line lit, a click on a line, kept across a change of
  track and in `lyrics-offsets.json`), its menu with tag editing off and on, Copy (to the bench's
  stand-in clipboard), Save as .lrc over Night Harbour's `.lrc` (asked, untouched, then replaced
  with the offset in its stamps), Save as .txt for Paper Moons' plain lyrics, Save into the tags
  (one save in the journal, the `.lrc` still read first), and a server's track refused. It marks
  its own copy of the set `.mp-edit-fixtures` (only inside the temporary folder), so the write
  guard lets it write there and nowhere else.
- **`viz-look`** (`bench/viz_look.rs`, the `viz` set): the halo, the pinned preview (and the
  equalizer's curve), `Shift+V`, opacity and the user's presets (see [Measurement and test
  hooks](#measurement-and-test-hooks)). The runner makes the `viz` set again, in place, when it is one from before its bright
  and monochrome covers ("Paper", "Graphite").
- **`row-hover`**, **`row-hover-125`**, **`row-hover-150`** (`bench/row_hover.rs`, the `core` set; scale 1,
  1.25 and 1.5 through `SLINT_SCALE_FACTOR`): the pointer resting on a track row's play button
  on an album page, moved a quarter pixel back and forth through the window's own dispatch, the
  row drawn after each move (`take_snapshot`): it must stay hovered (no change in eight moves)
  and a click there must play that row's track, on six rows. It fails on the old row, where
  the button, a sibling over the row's TouchArea, took the hover from the row that showed it.
  **`row-hover-z70`**, **`row-hover-z200`**: the same at the interface size's ends
  (`MP_BENCH_ZOOM`; a window too short for the album's rows scrolls the page and tests those
  wholly shown). Each then, where the window is frameless (Linux), sizes the window down and
  checks its edge line is drawn on the left, right and bottom edges.
- **`output-default`** (`bench/output_default.rs`, the `core` set; Linux): PipeWire's default moved between
  the headless session's two null sinks with `wpctl set-default` before anything played,
  while playing, paused and stopped; each time Settings > Output's list, the equalizer's device
  and its setting, the stream's real link (`pw-dump`) and "Playing on ..." must agree with the
  default PipeWire names. It changes the default only between "Test Speakers" and "Test
  Headphones" and puts the first one back; elsewhere (a real desktop) it checks the start only.
  On Windows it says so and ends.
- **`eq-paste`** (`bench/eq_paste.rs`, the `messy` set): the equalizer's Copy and Paste, every format, the
  preview applied and cancelled (see [Measurement and test hooks](#measurement-and-test-hooks)).
- **`zoom`** (`bench/zoom.rs`, the `core` set, two starts): the interface size at 70%, 100%, 150% and
  200% (the scale factor applied, the caption buttons and the player bar inside the window, the
  grids' covers at the size drawn; `zoom-070.png`, `zoom-100.png`, `zoom-150.png`,
  `zoom-200.png`), its keys through the window's key dispatch (remapped and reset in the key
  editor), Settings' row and "Find a setting", Ctrl and the wheel and a pinch through their
  handlers (the pinch's first finger pressing Back: not clicked), another monitor's scale factor,
  the minimum size at 200% and 100%; then `zoom-check`, the next start on the same data folder:
  150% restored as the window comes up (`zoom-restored-150.png`).
- **`home-shelves`** (`bench/home_shelves.rs`, the `core` set): Home's hero and grid shelves by the
  pointer and the keys through the window's dispatch. The hero's play button: not drawn without
  the pointer or with it elsewhere on the card, drawn with it on the cover and on every move
  resting on the button and across its edge, a click there playing the album and one elsewhere
  on the cover opening it; the strip's single click featuring a cover and playing nothing, its
  double click (at once, and 150 ms apart) playing the cover clicked. The shelf editor's rows,
  clicked up to 6 and down to 1 and held there. Grid shelves of 1, 2 and 6 rows at window widths
  from 960 to 1920 and at 70%, 100% and 150%: the page as tall as the rows wanted (its furthest
  scroll), the first and last rows full; a shelf of fewer albums than a row holds in one row;
  End and the arrows stopping at the last album shown. Snapshots `hs-*.png`.
- **`xfade-rates`** (`bench/xfade_rates.rs`, the `formats` set): the 48 kHz Opus track crossfading into a
  44.1 kHz FLAC album in the real app: the device not reopened, the details, Settings > Output and
  the player bar saying the track is converted from 44.1 kHz, the album's next track gapless and
  still converted, and after a pause the track at its own 44.1 kHz with the details saying so.
- **Modes beyond `MP_BENCH`'s own:** `skip-damaged` (the skip bench on the damaged FLAC), `session`
  (session-save, the history track's folder moved, a second folder added, session-check), `guard`
  (a scan, then nineteen in twenty files deleted), `soak` (the whole library on repeat, a track change
  every 1.5 s for 3 minutes: memory, handles, GDI and USER objects, UI stalls), `bigscan` (the first
  scan of `big`, quit 3 s in, then started again to finish: progress, UI stalls, the app's own
  writes waiting on the scan, when albums were first on the screen, memory, snapshots as it goes
  and at the minimum window size), `slowscan` (the same on the messy library with a slow disk
  simulated, `MP_SLOW_DISK_MS=8`: quit 40% through, then started again, which must say it picks
  the scan up and finish it), and the
  scenarios of the app's own files misbehaving, checked by the runner itself: `data-files`
  (settings.json with values it cannot read, damaged lastfm.json and scrobbles.json, a broken
  theme.toml, a junk tag-edit record), `db-garbage`, `db-newer`, `db-flipped` (a library damaged
  inside: found by the start-up check), `data-readonly` (Windows: the data folder denied writing by
  an ACL, removed again after), `two-instances` and `crash` (a deliberate panic on the UI thread and
  on another: the message, the backtrace in the log, the log kept as the previous one).

## Measurement and test hooks

Run `cargo xtask bench -- --only playback-cost` to measure ordinary playback. It plays the
same stereo FLAC on Home, on Now Playing with the visualizer, with the window hidden, and
after showing Home again. Each sample covers ten seconds after three seconds of warm-up.
`bench-playback-cost.txt` reports CPU as a percentage of one core and process memory in MiB.
On macOS the memory figure is physical footprint, which includes graphics allocations.
Keep the window unobstructed during the visible phases and compare the same window size,
renderer, audio device and saved settings between builds.

For a representative Now Playing measurement, `viz-idle` can also play a saved queue.
Use an isolated `MP_DATA_DIR` containing a database backup and its artwork cache, set
`MP_BENCH_SAVED_QUEUE=1`, and give the profile the window dimensions, panel pins, and
visualizer preset you want to reproduce. After a three-second warm-up the report includes
audio progress, spectrum updates, process CPU and memory. Compositor interpolation can
produce additional display frames between spectrum updates. Use an OS window recording
when measuring the animation as displayed.

To isolate the renderer on macOS, build once in release mode with the `bench` feature.
Make two copies of the same saved profile before launching either. Run `viz-idle` once
with `SLINT_BACKEND=winit-skia` and once with `SLINT_BACKEND=winit-software`.
Set `MP_BENCH_SAVED_QUEUE=1`, `MP_BENCH_VIZ_SECS=32`, and point `MP_DATA_DIR` and
`MP_BENCH_OUT` at the corresponding copy for each run. Check the window-surface log
and the audio-progress check in `bench-viz-idle.txt`. For Ring → Ripples, repeat at
Full and Half. When testing the Panel placement interactively, select its Visualizer
tab before measuring.

To repeat the check in a production build, start a fresh instance with
`--resume-playback`. It opens Now Playing and plays the restored queue at its
saved position. Audio files passed on the command line take precedence.

To check memory across track changes, use a pre-scanned queue with distinct 1200 px
album covers. Start each build hidden with the same menu-bar settings. Let the queue
advance, then compare the steady footprint on the first and last tracks. A single-track
loop can reuse the same image throughout and miss artwork retained by the system media controls.
The `covers` bench also checks that Now Playing follows rapid skips and clears its artwork.

Environment variables, for development only. `MP_BENCH` and its companions, and the three that
send requests to another host (`MP_LASTFM_API`, `MP_LISTENBRAINZ_API`, `MP_LRCLIB_API`), need a build with the `bench`
feature (`--features bench`) or a test build; the rest work in every build:

| Variable | Effect |
| --- | --- |
| `MP_DATA_DIR=<dir>` | Keep config, database and cache in one folder instead of `%APPDATA%` / `%LOCALAPPDATA%` |
| `MP_MUSIC=<dir>` | Add a library folder at start |
| `MP_VOLUME=0` | Start at this volume |
| `--minimized` (argument) | Start in the tray, the window made but not shown ("Start with Windows" passes it). Ignored without a tray |
| `--version` (argument) | Print `Marquee Player <version>` and exit (the AppImage's launcher uses it to check the libraries load) |
| `MP_BENCH=grid\|sidebar\|tour\|gapless\|theme\|skip\|session-save\|session-check\|guard\|idle-paused\|idle-stopped\|covers\|playlist\|phase4\|phase5\|cleanup\|visualizer\|playback-cost\|viz-idle\|settings\|autoscroll\|tray\|details\|metadata\|about\|home\|home-shelves\|library\|editing\|smart\|eq\|eq-paste\|viz-styles\|viz-cost\|viz-look\|server\|listenbrainz\|soak\|bigscan\|xfade-rates\|streaming\|zoom\|zoom-check` | Run a scripted measurement, write results and PNG snapshots to `MP_BENCH_OUT`, then quit (see `bench/mod.rs`). The frame timings (`grid`, `sidebar`, `playlist`) need `SLINT_BACKEND=winit-skia-opengl` and a build with `--features bench-opengl` |
| `MP_BENCH_GRID_PAGE=genres` | With `grid`: scroll the Genres page instead of Albums (the same grid with other tiles, to compare; it needs a library with a few hundred genres to have anywhere to scroll) |
| `MP_BENCH_GRID_SIZE=<w>x<h>` | With `grid`: resize the window's client area to this many physical pixels first (`2560x1392`: a maximized window on a 1440p monitor), for the columns and rows of a monitor larger than the one the bench's window opens on. The slow frames of a grid grow with the tiles on screen, so a stutter on a large window can be invisible at 1080p |
| `MP_BENCH_IDLE_SECS=<s>` | How long `idle-paused` / `idle-stopped` stay idle, hidden, before quitting (default 90) |
| `MP_WINDOW=secondary` | Open the window maximized on a monitor other than the primary one, without ever taking the foreground or keyboard focus (also when shown again from the tray); click into it to use it. One monitor: normal placement, still never activated. Bench runs honour it too | On Linux (no monitor to keep clear under a headless compositor): the window opens maximized and never asks for the focus, and the benches size it themselves |
| `MP_AUDIO=alsa` | Linux: play through ALSA's `default` device instead of PipeWire (the fallback where there is no PipeWire) |
| `MP_BENCH_PART=open` | With `phase5`: only files from outside (command line, a second launch, posted `WM_DROPFILES` of `MP_BENCH_DROP`, `;`-separated paths) |
| `MP_LASTFM_API=<url>` | Bench and test builds only: send Last.fm calls to this API root instead (a local mock). A release build ignores it, so nothing in the environment can send the session key elsewhere |
| `MP_LISTENBRAINZ_API=<url>` | Bench and test builds only: send every ListenBrainz request to this API root instead, whatever Settings' server address says (a local mock). A release build ignores it: the user's own "Server address" is the supported way to send elsewhere |
| `MP_LRCLIB_API=<url>` | Bench and test builds only: send lyrics lookups to this API root instead of `https://lrclib.net` (a local mock) |
| `MP_BENCH_VIZ_SECS=<s>` | How long `viz-idle` lets the visualizer run before pausing and hiding (default 20; then `MP_BENCH_IDLE_SECS`, default 60) |
| `MP_BENCH_SAVED_QUEUE=1` | With `MP_DATA_DIR`, use that profile's restored queue for `viz-idle`. The default uses the Spectrum fixture album. |
| `MP_BENCH_EDITION=installed\|installed-all` | Bench builds only: act as the installed edition (for you, for everyone) without any registration: the installer's mutex and quit event, the Default apps button, About's line |
| `MP_BENCH_DETAILS=off` | With `details`: the same views with the setting off (`off-*` snapshots), and no name prompts |
| `MP_BENCH_TRAY_SECS=<s>` | How long `tray` plays with the tray animation off, then on (default 30 each) |
| `MP_VIZ_DRAW=none\|minimal` | Measurement only: analyse without drawing, or redraw a 1 px rectangle per frame instead of the bars |
| `MP_BENCH_VIZ_PART=<parts>` | With `viz-styles`: only these parts (comma-separated: presets, colours, min, settings, menu, moments, sheet, idle; `none` for just the start) |
| `MP_BENCH_VIZ_CASES=<words>` | With `viz-cost`: only the cases whose name contains one of these (comma-separated) |
| `MP_BENCH_LOOK_PART=<parts>` | With `viz-look`: only these parts (comma-separated: halo, pin, keys, opacity, presets) |
| `MP_HALO_SHAPE=square` | Measurement and comparison only: the halo follows the cover's rounded square instead of a circle (the cover drawn square), as tried and left out. Works in every build |
| `MP_REDUCED_MOTION=1\|0` | Stand in for Windows' "Show animations in Windows" (off: the visualizer's default look is Calm). Works in every build |
| `MP_BENCH_SKIP_ALBUM=<title>` | With `skip`: the album to play (default: the first of four tracks or more) |
| `MP_BENCH_MOVED=<text>` | With `session-check`: the moved history track's path must contain this |
| `MP_BENCH_SOAK_SECS=<s>` | How long `soak` plays (default 120) |
| `MP_BENCH_QUIT_AFTER=<s>` | With `bigscan`: quit this many seconds in, scan or not |
| `MP_BENCH_QUIT_AT=<fraction>` | With `bigscan`: quit once the scan is this far through (`0.4`) |
| `MP_BENCH_STOP_AT=<fraction>` | With `bigscan`: press Settings > Library's Stop once the scan is this far through, check it stopped with Resume offered (`bigscan-stopped.png`), press Resume and check it picks the scan up |
| `MP_SLOW_DISK_MS=<ms>` | Simulate a slow disk: every folder the scanner lists and file it opens takes this much longer, one at a time (see [Scanning](features/library.md#scanning)). Works in every build |
| `MP_SCAN_TIMING=1` | Print when each part of a scan ended, and how long each batch held the database (stderr) |
| `MP_BENCH_ANSWER=yes` | A bench run's answer to the "start with a new library?" question for a damaged database (none: no) |
| `MP_BENCH_REAL_CLIPBOARD=1` | Linux only, with `eq-paste`: also copy and paste through the toolkit's real clipboard (for the private headless session, whose clipboard is nobody's; never set it on a desktop someone uses) |
| `MP_LOG=debug` | More logging, and once a minute a `wake-ups a second` line (`wakes.rs`): the process's CPU as a share of one core, and per second the output thread's loop turns, a pull device's callbacks (Core Audio, PipeWire; each wakes the output thread), UI ticks, visualizer frames, tray icon frames, the window's winit events and the redraws asked of winit (not those Slint's macOS display link draws by itself). The window being covered or seen again (macOS, X11) is logged as it happens |

Only one instance runs per data folder: launching the app again brings the running
one's window back (from the tray too) and exits. An instance on another `MP_DATA_DIR` is a
separate one, so test and bench runs never touch, nor are stopped by, the real one.

`MP_BENCH=metadata` runs on the metadata viewer's fixtures (FLAC with a full Picard tag set,
MP3 with ID3v2.4 and with ID3v2.3 plus ID3v1, Opus with R128, M4A with iTunes atoms, an
untagged WAV, an inconsistent album, a long comment with 3,000 extra items and a large
picture, a 400-track album; it deletes and breaks one file each in the folder after the
scan). Make them in an empty folder (never a music folder) and point `MP_MUSIC` at it:

    cargo run --release -p library --example meta_fixtures -- %TEMP%\mp-meta\music
    cargo run --release -p library --example meta_dump -- "<a file>"

`meta_dump` prints what the viewer reads from any file (read-only).

`MP_BENCH=editing` runs on the tag editor's fixtures (the metadata viewer's set, plus Ogg
Vorbis, ALAC, M4B, MP2, MP3 with three tag blocks and with unusual frames, WAV and AIFF with
and without their own text tags, untagged files, raw AAC and a 20-second FLAC to save while it
plays), made in a folder marked `.mp-edit-fixtures` inside the temporary folder (the tool refuses
any other place; every bench build refuses to write anything outside such a folder):

    cargo run --release -p library --example edit_fixtures -- %TEMP%\mp-edit\music

It checks the read-only viewer with editing off, turns editing on in Settings, edits a track by
the keyboard (a title, a second artist moved first, an invalid number, "1/7"), reviews, saves and
undoes it (the file checked on disk, its audio unchanged), edits by the pointer, asks before
closing with a draft, applies an album's fixes, an album field and a table row, replaces the
album's cover from an enormous picked image (the bench supplies the pick: no dialog), saves seven
files and checks the library after the rescan, stops a save at a read-only file and undoes what it
wrote, saves a track while it plays, and snapshots each state (`e-*.png`, also at the minimum
window size). The fixtures are changed by the run: make them again before the next.

`MP_BENCH=home` and `MP_BENCH=library` run on a messy fixture library: 2,754 short tracks in
mixed formats, most tagged in full, many not, the way real libraries are not (compilations
without an album artist, untagged "01 - Title" files, case and spacing variants, disc numbers
on some tracks only, "A1" numbers, two spellings of a title, loose singles, featured artists in
titles and artist fields, multi-valued tags in each format's own way, a 5.1 AAC file). Make it
in an empty folder (never a music folder):

    cargo run --release -p library --example messy_library -- %TEMP%\mp-messy\music

`library` checks and snapshots the compilation, the regrouped albums, the artist pages, the
disc headers, the album page at the minimum width with the sidebar pinned, View metadata of
the odd files and the 5.1 AAC note; `home` snapshots the home page at 1920 x 1080 and at the
minimum size.

`MP_BENCH=smart` runs on the messy library too (made with any number of albums, e.g. `--
%TEMP%\mp-smart-bench\music 120`), with favourites and plays seeded in its own
database (`bench/smart.rs`): it builds a smart playlist in the editor by the keyboard (the name,
Tab to the first rule, its field from the list by typing and Enter, its value, "Add
rule" by Enter) and by the pointer (an entry clicked in a list), a genre picked by typing, a
value typed wrong and explained, a save refused and fixed, a group, the limit; saves it and
checks the page against the rules; checks live updating (a "never played" list loses a track
once its play counts while the queue keeps it, a "most played this year" list holds what its
rules do, a favourites list follows `L`), Reshuffle, a rule from a newer build kept and
shown, a playlist that cannot name itself, Delete's hint, "Add to playlist" offering ordinary
playlists only, a home shelf, Save as ordinary playlist, Export, the Playlists page and the
sidebar; then search operators (`plays:>=1 -is:remote` typed into the search field: tracks only,
all played; the hint for `re:`; the syntax help; `year:` listing albums too; a plain query
still plain; both paths timed; Save as smart playlist opening the editor with the rules, and the
saved playlist holding what the search found); `s-*.png` at 1920 x 1080 and 960 x 620, and a count of failed checks at the end of
`bench-smart.txt`.

`MP_BENCH=viz-styles` and `viz-cost` (`bench/viz_styles.rs`) run on the visualizer's fixtures: three
albums of synthetic audio, each with its own cover (so accents and palettes differ): the test
signals `MP_BENCH=visualizer` and `viz-idle` use ("Spectrum": sweeps at 48 and 96 kHz, three
tones, pink noise, a groove that drops 20 dB after 10 s), and mixes with the spectrum of real
music ("Night Shift": house, hip-hop, a quiet intro, build, drop and breakdown; "Daylight":
rock, a quiet ballad, ambient 30 dB down), each a kick-like low pulse, bass, pads or chords
and hi-hat-like noise bursts, synthesised in Rust and encoded to FLAC by ffmpeg. Make them in
an empty folder:

    cargo run --release -p library --example viz_fixtures -- %TEMP%\mp-viz\music

`viz-styles` snapshots every preset in every placement at 1920 x 1032 and at 960 x 620 (with
the sidebars pinned, with auto-hide on), the colour schemes by height and across, moments of
the realistic fixtures in three looks, a strip of twelve consecutive frames for three looks
(`vs-sheet-*.png`, one frame each 17 ms), Now playing's menu (by the keyboard, into Style),
`V`, `C` and `Y`, the panel's tabs, Settings > Visualizer (live, still, scrolled, by the
keyboard, Reset, at the minimum size) and whether anything runs when it should not; `vs-*.png`
and a count of failed checks at the end of `bench-viz-styles.txt`. `viz-cost` measures every
style in every placement (`MP_BENCH_VIZ_SECS`, default 5, a case) and two at 30 frames a
second.

`MP_BENCH=viz-look` (`bench/viz_look.rs`) runs on the visualizer's fixtures, which now have two more
albums of one track each under a bright cover ("Paper": yellow, coral and sky on cream) and a
nearly monochrome one ("Graphite": cool greys), beside the dark "Night Shift": the halo in each
placement under the three covers at 1920 x 1080 and 960 x 620 (`vl-halo-*.png`; with
`MP_HALO_SHAPE=square`, `vl-halo-square-*`), its edge moving and closed; the pinned preview
beside the column and held at the top of a narrow one, scrolled, by Tab, paused (the still
frame), under "Find a setting" (none), on entering the section (its top), and the equalizer's
curve the same way; `Shift+V` through the three placements (the panel's tab chosen), with the
visualizer off and on another page, `V` alone, Settings > Keys and the key editor; opacity by
the keyboard and the pointer, behind the page and elsewhere apart, the quick menu's Opacity ▸
by the keyboard, saved; the user's presets: Save as by the keyboard, the picker, `C` into them,
Rename by the pointer (a built-in name refused), Update, Copy (the menu, `Ctrl+C`), Paste of
the same preset ("Use it"), of a friend's line inside a message (shown in the preview, added by
Enter), of a taken name (Keep both, Replace), refused pastes (no preset, a newer format, an
unknown style, 40 KB), Export, Delete, Import (Esc, then Add by the pointer), a binary file
refused, the quick menu's Your presets ▸, and settings.json read back after each change.
`vl-*.png` and a count of failed checks at the end of `bench-viz-look.txt`. It never touches
the real clipboard and opens no file dialog.

`MP_BENCH=server` (`bench/server.rs`) runs on any library of two albums of two tracks or more (the
runner gives it the `formats` set). It starts a Navidrome stand-in on 127.0.0.1 itself
(`subsonic::mock`; no request leaves the machine) serving two of the library's albums as the same
files under other titles, and 30 albums of its own (26 with covers), and drives Settings > Library
> Server: the signed-out form, "Find a setting" by "navidrome", the plain http note, a wrong
password refused in words, the right one (the field emptied, `navidrome.json` without the password
and, on Windows, without the token readable), the sync's progress (sidebar and Settings, the
stand-in slowed), the two albums one each with the files' titles, the covers in the cache, the log
and Copy diagnostics naming neither the address nor the user, no Show in Explorer and View
metadata "From the server" for the server's tracks, an M3U export leaving them out, About's
counts, Sync now listing again, a start's sync asking two questions (its scan, its stars), what
goes back to the server (the three switches, a heart sent at once and never a rating, a play
reported with its time, the server's heart here after Sync now, both switches off: not one
write, on again: what changed meanwhile sent, the Last.fm switch), Stop, the server gone (a
quiet line), the shrink guard held then confirmed, a refused sign-in (signed out, marked, the
music kept), signing in again and out. On the way: the cloud on a server album's page and its
tracks, not on a local album the server also has, on the server's tiles only in Albums, and on a
playlist's server tracks only (`sv-icon-dark.png`, and `sv-icon-light.png` in light colour
tokens); the tooltip's host without the scheme; the top right of Home (`activity::take_log`) for
the start-up scan, the sync after signing in, Sync now, a start's sync, one back from the tray, a
rescan of the scanned library and a rescan with a sync right after it, each at least 700 ms,
ending at full, one after the other without a gap. Then the folders are removed: an empty Home
offers both sources, Connect Navidrome opens Settings > Library with the sign-in focused, and the
first sync of an empty library (102 albums, the stand-in slowed) shows on Home in its large words,
its albums coming in before it ends. `sv-*.png` and a count of failed checks at the end of
`bench-server.txt`.

`MP_BENCH=eq` runs on any library of two albums of three tracks or more (the messy fixture
library will do): Settings > Equalizer off, on, with a preset, by the keyboard (Tab to the
sliders, Right, Up, Page Up, Home, Delete), the parametric view, an AutoEq file imported (its
preview, then applied) and a file of IIR coefficients refused, the preset menu, "Save as
preset…" typed, export as Equalizer APO text and as this app's own file read back, a track
playing at volume 0 (the details naming the equalizer, a device's own setting on and off), the
player bar's indicator on and off and its button, the crossfade settings and the transitions
handed to the engine (an album in order gapless, the next album crossfading, repeat one
gapless); `q-*.png` at the window's size and at 960 x 620, and a count of failed checks at the
end of `bench-eq.txt`. It writes its AutoEq fixture and export into `MP_BENCH_OUT`.

`MP_BENCH=eq-paste` (`bench/eq_paste.rs`, any library) drives Copy and Paste: Copy by its button and
by `Ctrl+C` on the page, "Copy as" GraphicEQ by the keyboard and EasyEffects by the pointer,
each read back; Paste of each format (AutoEq's FiiO FT1 files: ParametricEQ by
`Ctrl+V`, FixedBandEQ imported as a file, GraphicEQ fitted; Room EQ Wizard's export,
EasyEffects with a band left out, PulseEffects, this app's own file with every kind) with the
preview checked, applied by Enter and by the pointer, cancelled by Esc and by Cancel (the
equalizer untouched), shown again before the last preview had gone; text that is no equalizer
(the pill) and IIR coefficients (refused in the preview); the graphic view of a setting it
cannot show (nothing changes); a band's kind from its menu by the keyboard and the pointer,
stepped; "Find a setting". `p-*.png` at 1920 x 1080 and 960 x 620, failed checks at the end of
`bench-eq-paste.txt`. It never touches the real clipboard (the stand-in, `explorer::
bench_set_clipboard`); on Linux, `MP_BENCH_REAL_CLIPBOARD=1` adds a round trip through the
toolkit's clipboard, for the private headless session only.

`MP_BENCH=streaming` (`bench/streaming.rs`) needs no music folder: it makes its own fixtures with ffmpeg
(20 s of FLAC, MP3, Opus and an M4A with `moov` at the end, and two FLAC halves of one 8 s signal,
in `%TEMP%\mp-stream-fixtures`), starts a local mock of a Navidrome server on 127.0.0.1 in the app
(`server/streammock.rs`: the original file with ranges, a throttled link, stalls, drops, 503s, 404s), writes
a small catalog of them into the library as server-only tracks (`library::remote::sync`) and signs
the streamer in to the mock. It times the first sound of each format on a link that takes 6 s for
the file (each well before its download ends; Opus and the M4A by a range for their end), checks the
loading ring and, after a second, the pill, a seek far past the download (a range there), a resumed
position, two halves gapless (the second fetched as it became next, never waited for), a cached file
played with no request at all with its server lyrics synced and the details saying "from the
server", a song gone from the server (said, not retried, the queue on), signed out (one skipped,
then stopped with one reason; a track chosen not skipped), the cap (the playing track kept), a skip
from a download under way (its thread and connection gone within a moment), Settings > Playback's
card and quitting with nothing left running; `st-*.png` and a count of failed checks at the end of
`bench-streaming.txt`. Nothing leaves the machine.

To time smart playlists at scale without a music folder, make a fixture database (20,000 tracks
by default, a play history, the starting points and a smart playlist naming another) and time
every smart playlist in it and a set of heavier rule sets:

    cargo run --release -p library --example smart_fixture -- %TEMP%\mp-smart\library.db 20000
    cargo run --release -p library --example smart_time -- %TEMP%\mp-smart\library.db

`browse_time` times the browse paths (the Albums page in its orders, the Artists, Genres and
Labels indexes, a genre shelf, an artist's page, a 3,000-track playlist, search) on a library
database; `artist_report` prints every album artist's page, for comparing a library before
and after a migration:

    cargo run --release -p library --example browse_time -- <library.db> [artist]
    cargo run --release -p library --example artist_report -- <library.db>

To time search without a music folder, make a fixture database of mixed Latin, accented,
kana and featured-artist names and time queries on it:

    cargo run --release -p library --example search_fixture -- %TEMP%\fixture.db 4000
    cargo run --release -p library --example search_time -- %TEMP%\fixture.db beyonce kameria "kasane teto"

`cargo test -p player-core --release` checks gapless and seek against generated
fixtures. With `MP_REAL_ALBUM=<folder of .opus/.flac>` it also decodes a real album
and compares it, sample by sample, to ffmpeg's libopus decode.
