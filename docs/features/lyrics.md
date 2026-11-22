# Lyrics

Now playing's panel has two tabs, Up next and Lyrics (`Y` switches; from another page it
opens now playing on Lyrics). Lyrics come from a `.lrc` file beside the track with the same
name, else a `.txt` file of that name (what "Save as .txt" writes), else from its tags (LYRICS,
UNSYNCEDLYRICS, USLT, ©lyr), else, when Settings > Lyrics > Look up
lyrics online is on (it is off by default), from LRCLIB (lrclib.net).
Nothing is stored in the database. Synced lyrics (LRC time stamps, several per line, `[offset:]`, the
enhanced format's word stamps and voice tags stripped) light up the line being sung and keep
it in view (not for a few seconds after scrolling by hand); a click, or Enter on a line,
plays from there. Lyrics without stamps show as plain text. `library::lyrics`.

**Timing.** Synced lyrics early or late for a file: the row under them (Timing − +0.3 s + Reset)
moves them for the track playing, 0.1 s a click (held, every 70 ms after the first 450 ms), up to
60 s either way; `G` and `H` do the same from anywhere (remappable, said in the pill). It applies at
once to the line lit, the scrolling and a click on a line (which plays from where the line now
shows), and is kept per track by its path in `lyrics-offsets.json` in the data folder (0 is not
kept; a file moved or renamed outside the app starts again at 0). Hidden for plain lyrics.
`app/src/lyrics/mod.rs`; the maths, `library::lyrics::shifted` and `current_shifted`.

**Keeping them** (the tab's "⋯", or the Menu key on the lyrics; `app/src/lyrics/save.rs`):

- *Copy lyrics*: the words, a line each, no time stamps.
- *Save as .lrc* (synced) or *Save as .txt* (plain): beside the track as `<its name>.lrc` / `.txt`,
  UTF-8 without a BOM, the name the app reads first next time. An `.lrc` has `[ti:]`, `[ar:]` and
  `[al:]`, then `[mm:ss.xx]` stamps (centiseconds, what every LRC reader takes) with the offset in
  them (clamped at 0, no `[offset:]` tag), so other players show them as this one does. A file of
  that name already there is never replaced without asking: the tab says so, with its path, and
  offers Replace (written beside it, then moved over it). The note under the lyrics gives the full
  path of what was written. A track that plays from the server has no folder: refused, said why.
  After a save the lyrics are read again; when the file is what the track now reads, they show as
  its own (no longer "from LRCLIB") and the offset goes back to 0, being in the stamps.
- *Save into the file's tags…* (only with Settings > Library > Edit tags on, never for a server's
  track): asked first under the lyrics, then the same text goes in as the file's lyrics item
  (Vorbis LYRICS, or the UNSYNCEDLYRICS it already has; ID3v2 USLT, language kept or `XXX`; MP4
  ©lyr; a WAV or AIFF with only INFO or text chunks gets ID3v2.3 as for any edit) through the tag
  editor's own checked save (see [Tag editing](metadata.md#tag-editing): the copy checked before it replaces the file, the
  journal, Undo in View metadata's Edit history, the rescan). View metadata still only shows lyrics.
  A `.lrc` or `.txt` beside the track is still read first, and the note says so.

The app writes nothing beside a track otherwise. Bench builds write sidecars only where the editor's
write guard allows (its marked fixtures in the temporary folder).

Online (`crates/lrclib`, `app/src/lyrics/mod.rs`): local lyrics always win; a track without any is
looked up 2.5 s after it starts (skipping through tracks sends nothing), and only if it is a
library track, or a file from outside with artist and title tags of its own. What is sent is
its artist, title, album and length, to `lrclib.net`, with the client name
`MarqueePlayer/<version> (desktop music player)`; with the setting off nothing is sent (and cached
answers are not shown either). The exact lookup (`/api/get`, all four) comes first; when it
finds nothing, the search (`/api/search`, title and artist) is used, and a result is taken
only when its artist and title match the track's after the same folding search uses (case,
accents, kana, width) and its length is within 2 s. Synced lyrics are preferred; an
instrumental track says so. Every answer is cached in `lyrics\` in the cache folder, one
small JSON file per track, keyed by the folded artist, title, album and the length in whole
seconds (a retag or another edit of the song is a new entry), so each track is looked up
once and its lyrics work offline afterwards. "Not found" is asked again after 14 days;
network errors and service errors are not cached; a 429 or 503 pauses lookups for its
`Retry-After`. The Lyrics tab says what is happening: looking up, the lyrics (with "Lyrics
from LRCLIB · Wrong lyrics?" under them), not found, could not be reached, or an error, the
last three with "Try again". "Wrong lyrics?" removes them from the cache and never looks the
track up again by itself; "Look up again" then asks once more, refusing the record that was
rejected. All network work runs on its own thread; playback, track changes and exit never
wait for it, and an answer for a track no longer playing is cached but not shown.
`crates/lrclib/tests/mock.rs` runs it against a local mock of the API, and
`cargo run --release -p lrclib --example probe -- "<artist>" "<title>" "<album>" <seconds>`
looks one track up and prints the answer.

A track only the server has has no file to read: its lyrics are the server's
(`getLyricsBySongId`, synced ones first, the offset applied as LRC's), asked once and kept in
`lyrics\server\` in the cache folder (none found is asked again after 14 days; network errors are
not kept), then, without any, the online lookup as for a file. The server is asked whatever the
online setting (it is the user's own, and the track comes from it anyway). A track that is both a
local file and on the server is a local file here: `.lrc`, tags, then online.

## Streaming

A track only the server has plays at original quality by default and downloads into the stream
cache while it plays. In **Settings > Playback > Music from the server**, **Streaming quality**
can instead request MP3 at 96, 128, 192 or 320 kbps. This is a target average bitrate. A complete
cached original or a source already at or below the target plays in its original format.
Tracks already prepared for playback keep their quality. Reporting a play to the server follows the
rules under “What goes back to the server” in the [Navidrome guide](navidrome.md#navidrome).

- **Starting.** FLAC, MP3, WAV and an MP4 with `moov` first play once their first packets are in.
  An Ogg file (Opus, Vorbis) needs its last page, and an MP4 with `moov` after its audio needs that
  box, before anything plays: the downloader asks for that end with a range request as soon as the
  engine waits for it, then fills in the rest. Against the mock, a 20 s file on a link that takes
  6 s for it: first sound after 0.47 s (FLAC), 0.10 s (MP3), 1.7 s (Opus), 1.2 s (M4A with `moov`
  at the end) (`MP_BENCH=streaming`).
- **Loading.** While a server track waits for its bytes (not started yet, or the download behind
  the playing position), the play button has a breathing ring, the position holds, and after a
  second the pill says "Loading from the server…"; play/pause pauses it. Waiting is not listening:
  play counts, scrobbles and lyrics go by the position, which holds; Discord keeps what it was told
  until it plays on; the media controls (the Windows overlay, MPRIS) show it paused, at the
  position held (at the track's start before it has started), and playing again once it plays:
  their Play lets it go on, play/pause from a media key pauses it as the button does; the
  visualizer rests.
- **Seeking.** A seek past the download, and a resumed position (see [Resuming](playback.md#resuming)), are range
  requests where the engine waits: an MP4 goes straight there, FLAC and Ogg bisect over the file
  in a few small ranges. An MP3's seek reads every frame up to the place (the decoder's accurate
  seek), so a seek past an MP3's download waits until the download gets there.
- **Next.** The queue's next track (the engine's gapless next) starts downloading as it becomes
  next, so gapless and crossfade work between server tracks; the one after it once nothing else is
  downloading. Nothing is fetched for a track merely listed on a page.
- **Failures.** When an original download loses its connection, Marquee retries from where it
  stopped after 0.5, 1, 2, 4 and 8 s. After six attempts without progress, or as soon as the
  server says the file is gone or refuses the sign-in, the pill
  says why ("the server cannot be reached", "the file is no longer on the server", "signed out of
  the server") and the queue moves on as for a damaged file; two tracks in a row failing the same
  way stop playback there. A server that ignores ranges is read in order (Ogg and an MP4 with `moov`
  at the end then start only once they are whole). Signed out, or not connected yet, a server track
  not in the cache cannot play: reached by the queue it is skipped with a word in the pill, and the
  next one that cannot play either stops playback there; chosen, it says why and stays.
- **The cache.** Original files are named by song ID and source size. Encoded copies also include
  their target bitrate. A file being written has a `.partial` sidecar listing what is on disk,
  made before the file and removed only once it is whole and synced. Partial originals resume
  on the next play. Partial MP3 transcodes start again because a new encode can produce different
  bytes. A whole cached file plays offline as a local file. Settings > Playback > Music from
  the server (shown once there is a server or a cache): how much to keep (2 GB by default, 512 MB to
  64 GB; the least recently played go first, never the track playing or next, nor one being
  written), the space used, and Clear (which keeps the tracks playing and next). Signing out clears
  it.
- **Stopping.** A download stops at its next chunk when its track is skipped (the engine lets go
  of it and the queue no longer has it current or next), on sign-out (its track fails with "signed
  out of the server") and at exit.

The account side calls `streaming::set_client` when it connects, signs
in or signs out, and `streaming::clear_cache()` when it signs out.
