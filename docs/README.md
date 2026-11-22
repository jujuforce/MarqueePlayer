# Documentation

How the desktop app behaves, and how it is built, released and tested. The Android client has
its own documents in [android/](../android/README.md).

Building and maintaining:

- [building.md](building.md): building and running, `cargo xtask`, the checks before a push and CI, the Slint pin.
- [linux.md](linux.md): the Linux build: packages, what it needs at run time, what differs from Windows, and how it was tested.
- [macos.md](macos.md): the macOS build: building, the app bundle and disk image, first launch, what differs, and what is not yet verified on a Mac.
- [releases.md](releases.md): version numbers, `cargo xtask release`, the release workflow and its draft GitHub release, the editions (portable, installed, AppImage, disk image) and the exe's size.
- [architecture.md](architecture.md): the crates and the app's source files, and the files and folders the app keeps.
- [testing.md](testing.md): the bench runner and its modes, the fixtures, and the environment variables for measurement and tests.
- [troubleshooting.md](troubleshooting.md): what the app does when the machine, the library or its own files misbehave.

Features:

- [features/library.md](features/library.md): scanning, moves and missing folders, artists and albums from imperfect tags, the art cache, Recently added, the home shelves and the formats played.
- [features/navidrome.md](features/navidrome.md): connecting Plex, Navidrome and other Subsonic music servers, syncing their libraries and playing tracks.
- [features/playback.md](features/playback.md): resuming, ReplayGain, the equalizer, crossfade and the converter, the output device and exclusive mode.
- [features/scrobbling.md](features/scrobbling.md): Last.fm, ListenBrainz and Discord.
- [features/lyrics.md](features/lyrics.md): lyrics from files, tags, the server and LRCLIB, their timing and saving them, and streaming the server's tracks.
- [features/visualizer.md](features/visualizer.md): the visualizer's styles, colours, beat tracking, placements, presets and cost.
- [features/navigation.md](features/navigation.md): the sidebars and queue, browsing, search operators, playlists, selecting several tracks, smart playlists, the keys, middle-click scrolling, the tray icon and files opened from outside.
- [features/playlists.md](features/playlists.md): M3U import and export.
- [features/settings.md](features/settings.md): the Settings page, the interface size, the accent colour, the glass panels and Settings > About.
- [features/metadata.md](features/metadata.md): technical details, View metadata and tag editing.
