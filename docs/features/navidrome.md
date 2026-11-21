# Music servers

The desktop player connects to one music server at a time. Open **Settings > Library > Server**
or choose **Connect a server** on an empty Home page.

## Plex Media Server

1. Choose **Plex**. On the **Sign in with Plex** tab, select **Continue in browser**.
2. In your browser, sign in to the Plex account that can play your music and authorize Marquee Player.
3. Return to Marquee. If your account has one server, Marquee connects to it automatically.
   If you have several, choose **Connect** beside the server that hosts your music. If a server
   is unreachable, you can retry it or choose another from the list.
4. If the server shares several music libraries with you, choose **Use library** beside the
   one you want. Marquee selects a sole music library automatically. The selected library's
   albums and tracks appear in search and alongside your local music.

For a manual connection, open the **Connect manually** tab. Enter the address of the Plex Media Server, such as
`http://192.168.1.10:32400`, and paste a Plex access token in the separate token field.
Choose **Connect**, then choose a music library if prompted.
[How to find a Plex access token](https://support.plex.tv/articles/204059436-finding-an-authentication-token-x-plex-token/).

Marquee streams the original audio file by default. For remote listening, open
**Settings > Playback > Music from the server > Streaming quality** and choose an MP3
target of 96, 128, 192 or 320 kbps. This setting also applies to Subsonic servers.
Matched files play from your folders. Favourites, playlists and play counts
are kept in Marquee. Use **Sync now** to pick up changes, or choose a different library
under **Music library** to change what Marquee syncs. Marquee remembers your choice after
a restart. **Sign out** removes the connection, downloaded tracks and server-only entries from Marquee.

## Navidrome

Settings > Library > Server adds the music on a Navidrome server (or another Subsonic or
OpenSubsonic server) to the library, beside the music folders: folders only (as before), the
server only, or both as one library, each album and track listed once. It is the app's own
library all the same: favourites, plays and playlists are the app's, as they are for the
folders' music. The server's music files are never changed. What the app writes back is your
favourites and your plays, each behind its own switch (see "What goes back to the server");
those calls (`star`, `unstar`, `scrobble`) change only the server's own database. Ratings are
neither read from the server nor sent to it. Nothing else is ever sent: no playlist, no delete,
no tag, no scan. The sync, the covers and streaming use a read-only client
(`subsonic::Client::new(.., true)`).

**Signing in.** The address the server opens at in a browser (`http://` is allowed, with a note
that the connection is not encrypted; fine on a home network), the user and the password, then
Connect. The password makes the token that signs every request and is dropped at once: it is
never kept, and neither the address nor the user is written to the log. The salt and token are
kept in `navidrome.json` (see [Files](../architecture.md#files)), sealed with DPAPI for the current Windows user (on Linux
the file is the user's alone, mode 0600). The portable edition's drive taken to another PC (or
another Windows account) carries a sign-in that cannot be opened there: Settings asks to sign in
again, and the server's music stays in the library meanwhile. A wrong address, no answer,
something that is not a Subsonic server, a refused password or an account that cannot use token
sign-in (LDAP) are each said in their own words under the fields.

**One library** (`library::remote`, whose module documentation has every rule):
- a track the server has and a file also has is one row: the file's, with its tags (local tags
  win; the server never changes what a file says, or a local album's dates);
- the strongest sign that the server's song is the folder's file is the file itself: the same
  size and a path ending in the same folder and file name (case and separators aside). Then
  MusicBrainz ids, then album artist and title with the year and the track list agreeing. An
  album whose matched files are all one local album's is that album, even with a title spelled
  differently. When in doubt, two rows: a wrong merge would hand one album's history to another;
- a track only the server has is a row of its own, shown, searched, sorted and grouped as any
  other (its album dated by when the server first had it, for Recently added);
- when a file is deleted and the server has it, the track stays, as the server's (its plays,
  favourite and playlist places with it); a file that appears for a server track becomes
  one row with it (after every scan: `remote::reconcile`).

**Syncing** (`server/sync.rs`): at start (a few seconds after the window is up), after signing in,
on Sync now, and when the window comes back from the tray after three hours or more away; never
on a timer. Each first asks the server whether it has scanned since the last sync that saw
everything, and lists nothing when it has not (sign-in and Sync now list anyway). A listing is
the album list, 500 a page, then each album with its songs, three at a time, written every 60
albums: the first albums are on Home within seconds of signing in, and open pages take them in
as they do a scan's batches (also on a Home that was empty: it fills while the sync runs). Progress
shows at the foot of the sidebar, in Settings and in the top right of Home, as a scan's does ("What
it says", under [Scanning](library.md#scanning); on an empty home, in its large words), and Stop keeps what was written
(the next sync goes on from there). A scan and a sync take turns writing
the library. Only a listing that read every page and every album without an error removes what
the server no longer lists; when that is most of it at once (a server missing a drive: at least
25 tracks, fewer than one in ten left), Settings says so and removes them only when asked. A
server that does not answer is a quiet line in Settings and the library stays as last synced; a
sign-in the server no longer takes marks Settings' Library section and asks to sign in again.

**Covers.** An album only the server has gets its cover from the server, into the art cache
(the same sizes, backdrop and accent as a local cover; see [Art cache](library.md#art-cache)): newest albums first,
three at a time, after each sync, and first of all for a tile on screen without one. An album
with a file of its own keeps the art the scan found. Each album records which cover it fetched in
which cache format (`albums.art_checked`), so a pass interrupted goes on where it was, a cover
the server does not have is not asked for again, a cover changed on the server is fetched again,
and a new cache format fetches every server cover again once, newest first, the old images
showing meanwhile.

**Marked with a cloud.** What plays from the server (a track only the server has, an album none of
whose tracks has a file here) has a small cloud after its title: album tiles (Albums, Home's
shelves, an artist's page, search), the album page's header, Home's featured album, and every
track list (album, playlist, smart playlist, search, the queue and Up next). A local track the
server also has shows nothing: it plays from its file. Hovering the cloud names the server, its
address without the scheme (`https://music.example.org/nd` as `music.example.org/nd`;
`Server.host`, one property for every row, also while signed out with its music still shown); a
screen reader hears "from the server". The cloud takes no click and changes no row's height.

**Connect a server** on the empty home page (shown while no server is signed in) opens Settings >
Library with the sign-in card in view and the address field focused.

**What a track only on the server cannot do** (there is no file here): Show in Explorer is not
offered for it or its album; View metadata shows what the library knows, "From the server",
instead of the file's tags, and offers no editing (it names the server's file, and for an album
only the server has, the server's album and no folder, never a file or folder here); an M3U export leaves it out and says how many
it left out; Last.fm gets its plays without a recording id read from a file, and ListenBrainz with the ids the server gave (its recording, artists and release). Settings > About
counts the tracks only in the folders, only on the server and in both; Copy diagnostics names
the server software and those counts, never its address or the user.

**Signing out** asks nothing: the server's music leaves the library (tracks only it had go,
with their plays; the folders' tracks only forget the server), the sign-in is forgotten, and the
folders' music is as it was. Nothing of the sign-in is kept: `navidrome.json` is deleted (with any
`.tmp` or `.bad` copy beside it), the address and user name leave their fields, the plays not yet
reported and the stream cache go. Only a sign-in the server stopped accepting, or one that
cannot be opened here, keeps the address and user in the fields, to sign in again with.

**Signing in under another name** (after a lost sign-in, say: the server's other address, a LAN
address for a domain, http for https, another port, the user name spelt otherwise, or another
user) loses nothing by itself. Once the new server's albums are listed, and before anything is
written, their ids are compared with the kept catalog's (a few albums are read to compare their
songs' ids when the album ids differ, as after a server upgrade): most of them known, it is the
same server, and its music stays with every play, favourite, playlist place and play not yet
reported, under the new name. As another user of it, the favourites agreed with the first user
are forgotten, so the next sync keeps every favourite of either side as a first sync does; the
plays not yet reported go to the user signed in to now. Another server's catalog stays as it was
and nothing is sent to the new server (its plays would be refused as unknown songs, and lost):
Settings says how many tracks, and plays not yet reported, would leave the library, and "Remove
it from the library" replaces it, as signing out would, with the new server's music.

**What goes back to the server** (`app/src/server/write.rs`, `library::marks`). Three switches,
below the sign-in, all on by default:
- **Sync favourites with the server**: both ways, for every track the server has (its own
  tracks, and files matched to its songs). A heart given here goes to the server at once, off
  the UI thread; given there, it comes here at the next sync. A sync that lists nothing (the
  server has not scanned) still reads the server's stars (`getStarred2`, one request). Per
  song, the favourite last agreed with the server is kept (`remote_marks`, schema 13), so each
  sync tells which side changed: only the server's, it is taken; only here, it is sent; both
  (to different values), the later change wins when both times are known (the server's star
  time, the app's own change time), else the one made here. The first sync after this came
  (nothing agreed yet) keeps every favourite of either side (starred on one, starred on both).
  A value no side changed is never written. An answer that would unstar more than 25
  favourites at once, and more than half of those agreed starred, is taken for one that lost its
  stars (a server that did not say them, an answer cut short), not for the user's doing: none
  of those unstars is taken, nothing is sent over them either, and the log says so; a few
  unstarred on the server go as ever. A star whose time does not read is still a star, and
  `getStarred2` answering without its stars is an error, never "nothing starred". Offline, a change waits: a value here that differs
  from the agreed one is what is still to send, so the next sync sends it, also after a restart.
  Album favourites stay the app's own. A rating changed in an earlier version and not yet sent
  is dropped, never sent (its columns in `remote_marks` are kept, unused).
- **Report plays to the server**: each play counted here of a track the server has is reported
  (`scrobble`, with the time it was counted), and "now playing" when one starts. The play is
  queued in the library with the play itself (`remote_plays`, one transaction), sent oldest first,
  and off the queue once the server took it (or no longer has the song); offline, it waits for
  the next sync. Turned off, the plays not yet sent are forgotten. The server's own play counts
  are not taken into the library: the app's plays are what it reports, so they would count
  twice.
- **Scrobble server tracks from this app** (Last.fm and ListenBrainz alike; shown greyed with a
  word while neither is connected; see [Last.fm](scrobbling.md#lastfm) and [ListenBrainz](scrobbling.md#listenbrainz)): off, the app's own now
  playing and scrobble or listen are skipped, for both services, for a track played from the
  server, and for a file the server also has while plays are reported to it (the server hears of
  that play too). For a server that scrobbles itself (Navidrome can, to Last.fm and to
  ListenBrainz), so plays are not counted twice; the plays switch's hint points at it. A track the
  server does not have is always scrobbled. (Until ListenBrainz came this switch was Last.fm's
  alone, "Scrobble server tracks to Last.fm from this app"; its value in `settings.json`,
  `server_lastfm`, is kept and now covers both.)

With the first two off, the client may send no write at all (`subsonic::Writes::NONE`: the
guard refuses each before a request is made). The writes run on one thread of their own
(`server/write.rs`), the only one with a client that may write, so a change made here and a sync
never cross. Signing out forgets what was agreed and the plays not yet sent.

**Playing from the server** (the stream cache, loading, prefetch, lyrics from the server): see
[Streaming](lyrics.md#streaming), under [Lyrics](lyrics.md#lyrics).
