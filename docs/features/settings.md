# Settings and appearance

The Settings page, the interface size, the accent colour, the glass panels and Settings > About.

## Settings

Settings shows one section at a time: Library, Playback, Equalizer, Output, Lyrics,
Visualizer, Home, Window, Connections (Last.fm, ListenBrainz and Discord), Keys and About, listed beside a column at most 700 px wide. Both sit at the page
margin, under the title, so a setting's name and its control are never more than the
column apart, however wide the window. The list's rows shrink (40 px to 32) on a window too
short for all eleven above the player bar. On a narrow page (under 700 px: a small window, or
the sidebar and queue pinned) the list becomes a strip of tabs above the column. Only the
column scrolls; the title, the list and the filter box stay put.

Each setting is a row of a card: its name, one short line saying what it does (a few have
a "More" with the rest), and its control at the row's right edge. Two-state settings are
switches (the whole row toggles), small choices are segmented controls with every option
visible (ReplayGain, the close button), the preamp is a stepper (`widgets.slint`: `Switch`,
`Segmented`, `Stepper`, `SettingCard`, `SettingRow`, `SwitchRow`, `StatusNote`). A section's
state is at its top: the scan status and unreadable files, and under the folders the
music server's sign-in and sync (Library), the device playing
(Output), each service's connection (Connections). Missing tracks held back, a device not
there or an exclusive-mode fallback, a Last.fm session or a ListenBrainz token to connect again, a Discord application
ID that was refused, and a server that held back removals or needs signing in again also put a
dot on their section in the list.

A section can pin a card that stays in view while its column scrolls: Visualizer its preview,
Equalizer its curve. With room right of the column (at least 320 px: a 1080p window, not a
1280 px one), the card sits there, level with the section's first card, up to 560 px wide.
Otherwise it sits under the section's title and, once the column scrolls past it, is held at
the column's top, the column cut 12 px under it so the settings scroll under it, not behind it
(`SettingsView.pin-*`; `views::reveal` keeps a control the Tab key reaches below it). It is
drawn once either way, holds nothing focusable but a pasted preset's question, and goes with
the section (the filter's results have none).

The list is one Tab stop: Up and Down (Left and Right in the strip), Home and End show
another section. Tab order is the filter box, the list, then the section. "Find a setting"
finds settings and key actions by name or by what they are about (`exclusive`, `tray`,
`normalise`, `mute`), across sections, only those shown (the server's stream cache, Playback >
Music from the server, once there is a server or a cache); Enter or a click goes to one (its row
is outlined for a moment), Esc clears the box. A section is not a history entry: choosing one replaces
the Settings entry, so Back leaves Settings, and Back to it returns to the section (and
scroll) that was left; opening Settings again shows the section last shown.

## Interface size

Settings > Window > Interface size makes everything in the window larger or smaller together:
text, icons, covers, spacing, the caption and its buttons, menus, dialogs and tooltips. It goes
from 70% to 200% in a browser's steps (70, 80, 90, 100, 110, 125, 150, 175, 200), by the row's
− and + (Reset: 100%), `Ctrl+=` or `Ctrl++` and `Ctrl+-` (`Ctrl+0`: 100%; remappable in
Settings > Keys), Ctrl and the mouse wheel (a step a notch), or two fingers pinched on a
touchscreen (in steps of 5% as the fingers move, kept when they lift; anything between two
steps goes to the next one by the keys). The pill says the size at each change. It is kept in
`settings.json` (`"zoom": 110`) and applies as the window opens.

How (`zoom.rs`): the zoom is the window's scale factor. Slint draws every logical pixel at the
scale factor the system gives the window (the monitor's: 1.0 at 96 DPI, 1.5 at 144); zoomed,
the app gives it the monitor's times the zoom, as a DPI change would, so the markup never
knows. Moved to a monitor with another scaling, the window keeps the zoom on top of that
monitor's. What works in physical pixels follows: the caption buttons' hit testing on Windows
(the maximize button's snap layouts flyout included; the top resize border stays the system's
thickness), the window's minimum size (960 x 620 logical, made smaller only when the zoomed
size would not fit the monitor, so the window can always be on the screen: at 200% on a 1080p
display it is 952 x 500; see below for a window smaller than it), and the covers of the grids and shelves: drawn larger than the art
cache's 360 px thumbnails, they are decoded from the full image at the size drawn (rounded up
to 60 px), so they stay sharp; at most a little larger, the thumbnails serve. The tray's menu is
the system's own and keeps the system's size. Nothing runs while the zoom is still: it costs
nothing between changes.

A window smaller than its new minimum (zoomed in, or put back to a smaller size as it is
unmaximized) is held to it: Windows and X11's window managers grow it themselves, and on Wayland,
where a minimum only bounds the user's own resizes, the window resizes itself after every zoom
and every size the compositor chooses. Where it cannot grow (maximized, fullscreen, or tiled:
the compositor decides its size, and on Wayland the resize simply does not take) the logical
minimum shrinks to what the window has, so the layout always fits the window. The size kept for
the next start is in the monitor's own logical pixels, before the zoom (1600 x 1000 at 150% comes
back 1600 x 1000, on X11, Weston and KWin at 1.25 and 1.5 alike).

A pinch: one finger on the window is the page's as ever (it presses, scrolls and drags); when a
second comes down, the first one's press is undone without a click and, until every finger is
up, the page sees none of their touches, so a pinch never scrolls a list or presses what is
under it. The zoom follows once the fingers' distance has changed by 16 px.

Where it works: the keys and Settings everywhere. Ctrl and the wheel on Windows and Linux.
A touchscreen pinch where winit reports touches (Windows' touch input; X11's and Wayland's touch
events on Linux). A precision touchpad's pinch on Windows arrives as Ctrl and the wheel (Windows
does that for applications that do not ask for touchpad gestures), so it zooms in steps. Linux
touchpad pinches are not reported by the toolkit (winit has no gesture events there), so they do
nothing. Not tried on a real touchscreen or touchpad yet: the bench drives the pinch through its
handler.

## Accent

The accent (buttons, the playing track, links on hover, the seek bar thumb, focus
borders, the current sidebar item) follows the playing track's cover and drifts to the
next one over `[accent] transition_ms`. The colour is the cover's most prominent vivid
hue, not its average: near-black, near-white and grey pixels are ignored and the rest
vote by hue, weighted by colourfulness (`crates/library/src/accent.rs`). It is then
clamped in OKLCH (lightness 0.74-0.86, chroma 0.07-0.16) so it stays legible on the dark
background and under dark button text. It is computed once per cover and cached as
`<stem>_a.txt` beside the art. Greyscale covers, no art and nothing playing use
`[accent] fallback`. `[accent] source = "fixed"` pins `[colors] accent` instead.
`cargo run -p library --release --example accent_sheet -- <art dir> <out.png>` renders a
contact sheet of covers and their accents.

## Glass

The sidebar, player bar, queue panel and navigation pill blur the page behind them
live (Slint `backdrop-blur`, Skia on wgpu / Direct3D 12). `[glass]` in `theme.toml` sets the
strength (`blur`, 0 turns it off) and the tint. With blur off, or if Skia cannot start
(the app then uses the software renderer), panels draw near-opaque and the player bar
shows the pre-blurred album art as before. The page backdrop (album art behind pages)
is still the pre-blurred image: it is a decoration, not a panel.

## About

Settings > About (`about.rs`, `AboutSection` in `ui/pages/settings/about.slint`): the app's icon, name and
version with Copy (one line for a bug report: name, version, Windows version and build,
x64, the renderer), where the app keeps its files (settings, theme, the database with its
schema, the art and lyrics caches, the log; each with its path, size and Open folder), the
library's totals (albums, tracks, length, size of the music files; with a music server, how
many tracks are only in the folders, only on the server and in both), and the open-source
components it is built on, with Slint's `AboutSlint` badge. Under the copyright line, the
licence (GPL-3.0 or later) and, once `repository` is set in the workspace's Cargo.toml, the
address of the source code (`about::legal`). Slint is used under GPL-3.0, one of its three
licences, which asks for no badge: the badge stays as a credit. The credits list is
checked against `cargo metadata` by a unit test. Sizes and totals are measured when the
section is shown, on a thread of their own with a read-only connection, and the work stops
when the section or the page is left. Nothing goes online.
