# Marquee Player

**A music player that puts your album art first.** It plays the music in your own folders and
on a Plex or Subsonic server as one library, for Windows, Linux and macOS.

[**Download the latest release**](https://github.com/OffbeatDX/MarqueePlayer/releases/latest) ·
[Documentation](docs/README.md) · [Android app](android/README.md)

![The Home page: a recently added album, recently played and random albums](docs/images/home.webp)

| Now playing, with synced lyrics and the visualizer | The equalizer, with a pasted AutoEq-style setting |
| --- | --- |
| ![Now playing](docs/images/nowplaying.webp) | ![Equalizer](docs/images/equalizer.webp) |

## Why you might like it

- **Your library, as it is.** Point it at your folders and play within seconds while the rest is
  scanned. Moved and renamed files keep their plays, favourites and playlist places.
- **Music from your server.** Sign in to Plex in your browser and choose a music library on
  your server, or connect a Navidrome or Subsonic server. Browse its albums alongside your folders
  and stream at original quality or a lower MP3 bitrate. Marquee caches tracks for replay. Subsonic servers can also sync
  favourites and play history.
- **Sound left alone.** Gapless playback, ReplayGain, crossfade that keeps albums gapless, and
  bit-perfect output (WASAPI exclusive on Windows, native rates on PipeWire).
- **A real equalizer.** Up to 32 bands, a setting per device, and import from Equalizer APO,
  AutoEq, Room EQ Wizard and EasyEffects.
- **Covers lead the interface.** The accent colour follows the playing album, and a visualizer
  draws from what you actually hear.
- **Playlists that think.** Smart playlists by rules, search operators like
  `year:1990s -is:remote`, M3U import and export.
- **The rest you expect.** Synced lyrics, Last.fm and ListenBrainz scrobbling, Discord
  presence, safe tag editing with undo, remappable keys, screen-reader support.
- **Private by default.** No network request is made for a service you have not set up.

Plays Opus, Ogg Vorbis, FLAC, MP3, WAV, AIFF, AAC and ALAC.
Each feature is described in [docs/features/](docs/README.md).

## Install

Get the package for your system from the
[releases page](https://github.com/OffbeatDX/MarqueePlayer/releases/latest):

| System | Package |
| --- | --- |
| Windows | `-setup.exe` (installer) or `-portable.zip` (runs from any folder or USB stick) |
| Linux | `.AppImage` (`chmod +x`, then run it) |
| macOS (Apple Silicon) | `.dmg`; the app is not notarised, so the first launch needs [one extra step](docs/macos.md#first-launch) |

On first start, add a music folder or connect a music server. Your music files are only
ever read, unless you turn on tag editing.

> The macOS version is new and has had far less testing than Windows and Linux:
> [what to expect](docs/macos.md).

## Build from source

You need Rust (rustup picks the version from `rust-toolchain.toml`) and CMake; Linux and macOS
need a few system packages. [docs/building.md](docs/building.md) has the details.

```sh
cargo run -p marquee-player --release   # run it
cargo xtask check                       # the checks to run before proposing a change
cargo xtask release                     # build the packages for this system, in dist/
```

The desktop app is Rust and [Slint](https://slint.dev), in `crates/`
([architecture](docs/architecture.md)). The Android client for Navidrome is a separate Gradle
project in [android/](android/README.md).

## Contributing

Bug reports and pull requests are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md). To report a
security problem, see [SECURITY.md](SECURITY.md).

## Licence

Copyright © 2026 Offbeat. Marquee Player is free software under the
[GNU General Public License, version 3 or later](LICENSE), and comes with no warranty.
Third-party components keep their own licences, listed in the `THIRD-PARTY-NOTICES.txt` that
ships with each package.
