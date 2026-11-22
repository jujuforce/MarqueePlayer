//! settings.json: library folders, playback preferences, and the home shelves.
//! Every field has a default, so an older or hand-edited file always loads.
//!
//! `nav` is Settings' sections and its "Find a setting" box.

pub(crate) mod nav;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use library::ShelfQuery;
use library::query::AlbumSort;
use player_core::{GainConfig, GainMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShelfLayout {
    #[default]
    Row,
    Grid,
    Hero,
}

/// One home shelf: what it shows, how, and how many.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShelfConfig {
    pub query: ShelfQuery,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub layout: ShelfLayout,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// The grid layout: how many rows of covers it shows. Each row is filled to the width, so
    /// the number of albums follows from the window and the zoom, and no row ends short.
    #[serde(default = "default_rows")]
    pub rows: usize,
    #[serde(default = "yes")]
    pub visible: bool,
}

fn default_limit() -> usize {
    18
}

fn default_rows() -> usize {
    2
}

/// The most rows a grid shelf shows.
pub const GRID_ROWS_MAX: usize = 6;

fn yes() -> bool {
    true
}

impl ShelfConfig {
    pub fn new(query: ShelfQuery, layout: ShelfLayout, limit: usize) -> Self {
        Self { query, title: None, layout, limit, rows: default_rows(), visible: true }
    }

    pub fn title(&self) -> String {
        self.title.clone().unwrap_or_else(|| self.query.default_title())
    }

    /// The albums to ask for: the limit, or for a grid its rows of `grid_cols` covers (the
    /// columns the page's width holds, `views::grid_cols`), so that it builds no tile it hides.
    pub fn fetch_limit(&self, grid_cols: usize) -> usize {
        match self.layout {
            ShelfLayout::Grid => self.rows.clamp(1, GRID_ROWS_MAX) * grid_cols.max(1),
            _ => self.limit,
        }
    }

    /// Of `n` albums found for this shelf, those it shows: all of them, or for a grid whole
    /// rows of `grid_cols` only (one short row when there are fewer than a row holds).
    pub fn shown(&self, n: usize, grid_cols: usize) -> usize {
        let cols = grid_cols.max(1);
        match self.layout {
            ShelfLayout::Grid if n > cols => self.rows.clamp(1, GRID_ROWS_MAX).min(n / cols) * cols,
            _ => n,
        }
    }
}

/// The home page as it comes: each shelf named for what it holds, and no two neighbours
/// alike. What is new first, as the big card (the newest album, the next eleven in its
/// strip), then what was being played, a random row to rediscover, the most played, the
/// latest releases and the favourites. A shelf with nothing in it (no plays yet, no
/// favourites) is not shown, so a new library starts with what was added, a random row and
/// the releases.
pub fn default_shelves() -> Vec<ShelfConfig> {
    vec![
        ShelfConfig::new(ShelfQuery::RecentlyAdded, ShelfLayout::Hero, 12),
        ShelfConfig::new(ShelfQuery::RecentlyPlayed, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::Random, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::MostPlayed, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::RecentlyReleased, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::Favourites, ShelfLayout::Row, 20),
    ]
}

/// The defaults of earlier versions: a random "Featured" hero, whose
/// row of the rest was titled "More", then "Explore" (random again), recently added,
/// recently released, recently played, most played, favourites. A settings file that still
/// has exactly these was never customised: it gets the current defaults (`Settings::load`).
fn previous_default_shelves() -> Vec<ShelfConfig> {
    let titled = |q: ShelfQuery, layout, limit, title: &str| ShelfConfig { title: Some(title.into()), ..ShelfConfig::new(q, layout, limit) };
    vec![
        titled(ShelfQuery::Random, ShelfLayout::Hero, 20, "Featured"),
        titled(ShelfQuery::Random, ShelfLayout::Row, 20, "Explore"),
        ShelfConfig::new(ShelfQuery::RecentlyAdded, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::RecentlyReleased, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::RecentlyPlayed, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::MostPlayed, ShelfLayout::Row, 20),
        ShelfConfig::new(ShelfQuery::Favourites, ShelfLayout::Row, 20),
    ]
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GainSetting {
    Off,
    #[default]
    Track,
    Album,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatSetting {
    #[default]
    Off,
    All,
    One,
}

/// What the window's close button (and Alt+F4) does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseAction {
    /// Hide to the tray; the music goes on. Quit is in the tray's menu.
    #[default]
    Hide,
    Quit,
}

/// What a click on the tray icon does (Settings > Window; `tray/mod.rs`). The middle button,
/// the wheel and the menu are the same either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayClick {
    /// Play or pause; a double click shows or hides the window.
    #[default]
    PlayPause,
    /// Show the window (as before the click played).
    ShowWindow,
}

/// An equalizer band's kind, as saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqKindSetting {
    #[default]
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
    AllPass,
}

/// How a band's `q` is meant: a Q (as every band before the other widths existed, and
/// left out of the file then, so a band saved before reads the same and a band with a Q
/// is written as before), a bandwidth in octaves, or a shelf's slope S.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqWidthSetting {
    #[default]
    Q,
    Octaves,
    Slope,
}

impl EqWidthSetting {
    fn is_q(&self) -> bool {
        *self == EqWidthSetting::Q
    }
}

fn default_q() -> f32 {
    1.41
}

/// One equalizer band as saved.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqBandSetting {
    #[serde(default)]
    pub kind: EqKindSetting,
    pub freq: f32,
    #[serde(default)]
    pub gain: f32,
    #[serde(default = "default_q")]
    pub q: f32,
    #[serde(default, skip_serializing_if = "EqWidthSetting::is_q")]
    pub width: EqWidthSetting,
}

/// An equalizer's setting (`eq.rs`): for every device, or for one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqCurve {
    pub enabled: bool,
    /// The preset it is, while unchanged ("Flat", a user preset's name); `None` once edited.
    pub preset: Option<String>,
    pub bands: Vec<EqBandSetting>,
    /// Preamp: down by the curve's largest boost (auto), or `preamp_db`.
    pub preamp_auto: bool,
    pub preamp_db: f32,
}

impl Default for EqCurve {
    fn default() -> Self {
        Self { enabled: false, preset: Some("Flat".into()), bands: crate::eq::flat_bands(), preamp_auto: true, preamp_db: 0.0 }
    }
}

/// A preset the user saved (or imported).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserPreset {
    pub name: String,
    pub bands: Vec<EqBandSetting>,
    pub preamp_auto: bool,
    pub preamp_db: f32,
}

impl Default for UserPreset {
    fn default() -> Self {
        Self { name: String::new(), bands: Vec::new(), preamp_auto: true, preamp_db: 0.0 }
    }
}

/// The equalizer of one output device, used whenever it plays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceEq {
    /// The device's name, to show.
    pub name: String,
    pub curve: EqCurve,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqSettings {
    /// What plays on every device without one of its own.
    pub all: EqCurve,
    pub presets: Vec<UserPreset>,
    /// By device id (as `output_device`).
    pub devices: BTreeMap<String, DeviceEq>,
}

/// Settings > Playback > Crossfade (`crossfade.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrossfadeSettings {
    pub enabled: bool,
    pub secs: f32,
    /// Never crossfade an album's tracks played in its order: they stay gapless.
    pub album_gapless: bool,
    /// Skipping (next, previous, choosing a track) crossfades too, briefly.
    pub on_skip: bool,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self { enabled: false, secs: 6.0, album_gapless: true, on_skip: true }
    }
}

/// Settings > Connections > Discord (`connections/presence.rs`): what plays, on the user's Discord
/// profile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordSettings {
    pub enabled: bool,
    /// The user's own Discord application (its ID is not a secret).
    pub app_id: String,
}

/// The window's restored rectangle (Win32 `WINDOWPLACEMENT.rcNormalPosition`, physical
/// pixels, workspace coordinates) and whether it was maximized. See `placement.rs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPlacement {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub maximized: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub folders: Vec<PathBuf>,
    pub volume: f32,
    /// Muted keeps `volume`, so unmuting restores it.
    pub muted: bool,
    pub gain: GainSetting,
    pub preamp_db: f32,
    pub prevent_clipping: bool,
    /// Limit peaks above full scale (hot masters, positive gain). On by default.
    pub limit_peaks: bool,
    pub sidebar_pinned: bool,
    pub queue_pinned: bool,
    pub bar_autohide: bool,
    pub shelves: Vec<ShelfConfig>,
    /// Play modes, kept across restarts (the queue itself is in library.db).
    pub shuffle: bool,
    pub repeat: RepeatSetting,
    /// Where the window was; `None` until the first run ends.
    pub window: Option<WindowPlacement>,
    /// The Albums page's order (its quick filter is not kept).
    pub albums_sort: AlbumSort,
    pub albums_desc: bool,
    pub close_action: CloseAction,
    pub tray_click: TrayClick,
    /// The output device by its id; `None` plays on the system default (and follows it).
    pub output_device: Option<String>,
    /// The chosen device's name when it was chosen, to name it while it is not connected.
    pub output_device_name: Option<String>,
    /// Exclusive (bit-perfect) mode on the output device.
    pub exclusive: bool,
    /// Settings > Keys: the actions whose keys differ from the defaults, by action id,
    /// one string per key slot ("" for none), e.g. `"mute": ["K"]` (`keys/map.rs`). Read
    /// leniently: a malformed value here is ignored, never the whole file.
    #[serde(deserialize_with = "lenient_shortcuts")]
    pub shortcuts: BTreeMap<String, Vec<String>>,
    /// Look up lyrics on LRCLIB for tracks without local ones (`lyrics/mod.rs`). Off unless
    /// the user turns it on: it sends the track's artist, title, album and length.
    pub lyrics_online: bool,
    /// The spectrum visualizer on the now playing page (`visualizer/mod.rs`).
    pub visualizer: bool,
    /// Its look and where it shows (`visualizer/look.rs`). `None` until the user chooses anything:
    /// then the default follows Windows' "Show animations" (Classic, or Calm with it off).
    pub visualizer_look: Option<crate::visualizer::look::VizSettings>,
    /// The visualizer presets the user made (`visualizer/presets.rs`), in the order made. Apart from
    /// `visualizer_look`, so its Reset keeps them; read entry by entry (`UserPresets`).
    #[serde(skip_serializing_if = "crate::visualizer::look::UserPresets::is_empty")]
    pub visualizer_presets: crate::visualizer::look::UserPresets,
    /// Middle-click autoscroll (`autoscroll.rs`).
    pub autoscroll: bool,
    /// The tray icon's bars move while music plays (`tray/anim.rs`).
    pub tray_animation: bool,
    /// Settings > Window > Show technical details: format, bit depth, sample rate and
    /// bitrate in the track lists, the player bar and now playing (`metadata/details.rs`). Off
    /// unless turned on.
    pub show_details: bool,
    /// Settings > Library > Edit tags: View metadata can change the files' tags and front
    /// covers (`metadata/tagedit.rs`, `library::edit`). Off unless the user turns it on: with it off
    /// the app only ever reads music files.
    pub tag_editing: bool,
    /// The equalizer, its presets and the devices that have their own (`eq.rs`). Off
    /// unless turned on.
    pub equalizer: EqSettings,
    /// Crossfading between tracks (`crossfade.rs`). Off unless turned on.
    pub crossfade: CrossfadeSettings,
    /// Settings > Connections > Discord (`connections/presence.rs`). Off unless turned on.
    pub discord: DiscordSettings,
    /// Settings > Playback > Music from the server: the most the stream cache keeps, in MB
    /// (`server/streaming.rs`; least recently played files go first).
    pub stream_cache_mb: u64,
    /// Target bitrate for new server streams. An absent value plays the original file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_bitrate_kbps: Option<u32>,
    /// Settings > Library > Server: favourites of the server's tracks kept in step
    /// with the server, both ways (`server/write.rs`). On unless turned off.
    pub server_marks: bool,
    /// Identifies this installation to Plex across browser sign-ins, including after sign-out.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub plex_client_id: String,
    /// Plays of the server's tracks reported to it (`scrobble`). On unless turned off.
    pub server_plays: bool,
    /// The app's own scrobbles and "now playing", to Last.fm and to ListenBrainz, of tracks
    /// played from the server (and, while plays are reported to the server, of the files the
    /// server also has). Off when the server scrobbles itself. On unless turned off. (Its name is
    /// from when it was Last.fm's alone; kept, so the value set then still holds.)
    pub server_lastfm: bool,
    /// Settings > Connections > ListenBrainz: a track made a favourite here is loved on
    /// ListenBrainz (and no longer loved when it stops being one), when it has a MusicBrainz
    /// recording id. Off unless turned on.
    pub listenbrainz_love: bool,
    /// The portable edition only: the root of the drive (Linux: the mount point) the app ran
    /// from last time, so that library folders on that drive follow it to a new letter
    /// (`edition::follow_drive`). Never written by the other editions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portable_volume: Option<String>,
    /// Settings > Window > Interface size: the whole window's zoom, percent (`zoom.rs`; 70 to
    /// 200, a value outside them clamped when it applies).
    pub zoom: u32,
}

/// `shortcuts` as written, or whatever of it makes sense: each value a list of strings
/// (or one string); anything else is dropped with a warning.
fn lenient_shortcuts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Vec<String>>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    let mut out = BTreeMap::new();
    let serde_json::Value::Object(map) = v else {
        if !v.is_null() {
            log::warn!("settings.json: shortcuts is not an object; using the default keys");
        }
        return Ok(out);
    };
    for (id, keys) in map {
        // A key of an action this version no longer has: gone, and not written back.
        if crate::keys::map::RETIRED.contains(&id.as_str()) {
            continue;
        }
        let keys = match keys {
            serde_json::Value::String(s) => Some(vec![s]),
            serde_json::Value::Array(a) => a.into_iter().map(|k| k.as_str().map(str::to_owned)).collect(),
            _ => None,
        };
        match keys {
            Some(k) => {
                out.insert(id, k);
            }
            None => log::warn!("settings.json: shortcuts.{id} is not a list of keys; ignored"),
        }
    }
    Ok(out)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            volume: 0.8,
            muted: false,
            gain: GainSetting::Track,
            preamp_db: 0.0,
            prevent_clipping: true,
            limit_peaks: true,
            sidebar_pinned: false,
            queue_pinned: false,
            bar_autohide: false,
            shelves: default_shelves(),
            shuffle: false,
            repeat: RepeatSetting::Off,
            window: None,
            albums_sort: AlbumSort::Artist,
            albums_desc: false,
            close_action: CloseAction::Hide,
            tray_click: TrayClick::PlayPause,
            output_device: None,
            output_device_name: None,
            exclusive: false,
            shortcuts: BTreeMap::new(),
            lyrics_online: false,
            visualizer: true,
            visualizer_look: None,
            visualizer_presets: crate::visualizer::look::UserPresets::default(),
            autoscroll: true,
            tray_animation: true,
            show_details: false,
            tag_editing: false,
            equalizer: EqSettings::default(),
            crossfade: CrossfadeSettings::default(),
            discord: DiscordSettings::default(),
            stream_cache_mb: crate::server::streaming::DEFAULT_CAP_MB,
            stream_bitrate_kbps: None,
            server_marks: true,
            plex_client_id: String::new(),
            server_plays: true,
            server_lastfm: true,
            listenbrainz_love: false,
            portable_volume: None,
            zoom: crate::zoom::DEFAULT,
        }
    }
}

/// Replace `path` with `bytes`: written to `<path>.tmp`, flushed to the disk, renamed over it.
/// A crash or power cut at any moment leaves the old file or the new one, whole.
pub fn write_durably(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        Self::load_with_problems(path).0
    }

    /// The settings, what of them could not be read (empty when all was well), and whether the
    /// file may be written over. A file that cannot be read in full keeps every setting that can
    /// be (one damaged or unexpected value, a newer build's, costs that setting, not the music
    /// folders with it), and is kept as `settings.json.bad` before anything is written over it.
    /// One that is there but cannot be read at all (held by another program, no access) is
    /// copied there too if it can be; if not, it is not to be written over this run.
    pub fn load_with_problems(path: &Path) -> (Self, Vec<String>, bool) {
        let keep = || match std::fs::copy(path, path.with_extension("json.bad")) {
            Ok(_) => true,
            Err(e) => {
                log::error!("settings.json cannot be copied to settings.json.bad ({e}); it is not saved over this run");
                false
            }
        };
        let (mut s, problems, replaceable) = match std::fs::read(path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let (s, problems) = Self::from_text(&text);
                let mut replaceable = true;
                if !problems.is_empty() {
                    log::error!("settings.json: {}; the file is kept as settings.json.bad", problems.join("; "));
                    replaceable = keep();
                }
                (s, problems, replaceable)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), Vec::new(), true),
            Err(e) => {
                log::error!("settings.json cannot be read ({e}); using the defaults");
                (Self::default(), vec![format!("the file cannot be read ({e})")], keep())
            }
        };
        s.upgrade_shelves();
        (s, problems, replaceable)
    }

    /// Settings from the file's text, setting by setting when it is not valid as a whole.
    pub fn from_text(text: &str) -> (Self, Vec<String>) {
        if text.trim().is_empty() {
            return (Self::default(), vec!["the file is empty".into()]);
        }
        let e = match serde_json::from_str::<Self>(text) {
            Ok(s) => return (s, Vec::new()),
            Err(e) => e,
        };
        let Ok(serde_json::Value::Object(theirs)) = serde_json::from_str::<serde_json::Value>(text) else {
            return (Self::default(), vec![format!("the file is damaged ({e}); every setting is back to its default")]);
        };
        let Ok(serde_json::Value::Object(mut kept)) = serde_json::to_value(Self::default()) else {
            return (Self::default(), vec![format!("{e}")]);
        };
        let mut problems = Vec::new();
        for (key, value) in theirs {
            let mut trial = kept.clone();
            trial.insert(key.clone(), value);
            if serde_json::from_value::<Self>(serde_json::Value::Object(trial.clone())).is_ok() {
                kept = trial;
            } else {
                problems.push(format!("\u{201c}{key}\u{201d} could not be read and is back to its default"));
            }
        }
        let s = serde_json::from_value(serde_json::Value::Object(kept)).unwrap_or_default();
        (s, problems)
    }

    /// Home shelves never customised (exactly an earlier version's defaults) become the
    /// current defaults; any change the user made (an order, a title, a layout, a limit, a
    /// shelf hidden, added or removed) keeps theirs as they are.
    pub fn upgrade_shelves(&mut self) -> bool {
        if self.shelves == previous_default_shelves() {
            log::info!("settings.json: the home shelves were the earlier defaults; using the current ones");
            self.shelves = default_shelves();
            return true;
        }
        false
    }

    /// Write the settings: to a temporary file, flushed to the disk, then renamed over the old
    /// one, so a crash or a power cut leaves the old file or the new, never an empty one. False
    /// (and logged) when it could not be written: a read-only file or folder, a full disk.
    pub fn save(&self, path: &Path) -> bool {
        let text = match serde_json::to_string_pretty(self) {
            Ok(t) => t,
            Err(e) => {
                log::error!("saving settings: {e}");
                return false;
            }
        };
        match write_durably(path, text.as_bytes()) {
            Ok(()) => true,
            Err(e) => {
                log::error!("settings.json cannot be saved ({e}): {}", path.display());
                false
            }
        }
    }

    pub fn gain_config(&self) -> GainConfig {
        GainConfig {
            mode: match self.gain {
                GainSetting::Off => GainMode::Off,
                GainSetting::Track => GainMode::Track,
                GainSetting::Album => GainMode::Album,
            },
            preamp_db: self.preamp_db,
            limit_peaks: self.limit_peaks,
            prevent_clipping: self.prevent_clipping,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_bad_value_costs_that_setting_not_the_folders() {
        // A wrong type (a hand edit, a newer build's format) beside the music folders.
        let (s, problems) = Settings::from_text(r#"{ "folders": ["M:/Music", "N:/More"], "volume": "loud", "exclusive": true, "crossfade": 7 }"#);
        assert_eq!(s.folders, vec![PathBuf::from("M:/Music"), PathBuf::from("N:/More")]);
        assert!(s.exclusive);
        assert_eq!(s.volume, Settings::default().volume);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("volume")) && problems.iter().any(|p| p.contains("crossfade")));
        // Damaged beyond reading, or empty: the defaults, said.
        let (s, problems) = Settings::from_text(r#"{ "folders": ["M:/Mus"#);
        assert!(s.folders.is_empty() && problems.len() == 1);
        assert_eq!(Settings::from_text("").1.len(), 1);
        // Unknown keys (a newer build's settings) are no problem at all.
        let (s, problems) = Settings::from_text(r#"{ "folders": ["M:/Music"], "a_newer_setting": { "x": 1 } }"#);
        assert!(problems.is_empty() && s.folders.len() == 1);
    }

    #[test]
    fn a_damaged_file_is_kept_and_a_read_only_one_is_said() {
        let dir = std::env::temp_dir().join(format!("mp-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{ "folders": ["M:/Music"], "volume": [] }"#).unwrap();
        let (s, problems, _) = Settings::load_with_problems(&path);
        assert_eq!(s.folders.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(path.with_extension("json.bad").exists());
        // Saved durably, and read back the same.
        assert!(s.save(&path));
        assert_eq!(Settings::load(&path).folders, s.folders);
        assert!(!dir.join("settings.json.tmp").exists());
        // Read-only: the save says it failed and the file is as it was.
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&path, perm.clone()).unwrap();
        let mut changed = s.clone();
        changed.volume = 0.123;
        if cfg!(windows) {
            assert!(!changed.save(&path), "a read-only settings.json cannot be replaced on Windows");
            assert_eq!(Settings::load(&path).volume, s.volume);
        }
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&path, perm).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_not_written_over() {
        // A settings.json that is there but cannot be read (held by another program, no
        // access; here a folder of that name): the defaults are used, and nothing may be saved
        // over it, since no copy of it could be kept.
        let dir = std::env::temp_dir().join(format!("mp-settings-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&path).unwrap();
        let (s, problems, replaceable) = Settings::load_with_problems(&path);
        assert!(s.folders.is_empty() && problems.len() == 1, "{problems:?}");
        assert!(!replaceable, "an unreadable file of which no copy is kept is not to be replaced");
        // Damaged but read, and copied aside: it may be replaced.
        let other = dir.join("other.json");
        std::fs::write(&other, r#"{ "volume": [] }"#).unwrap();
        let (_, problems, replaceable) = Settings::load_with_problems(&other);
        assert!(problems.len() == 1 && replaceable && other.with_extension("json.bad").exists());
        // Missing, or well: no problem, and it may be written.
        assert!(Settings::load_with_problems(&dir.join("none.json")).2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_file_loads_with_defaults() {
        let s: Settings = serde_json::from_str(r#"{ "volume": 0.5, "shelves": [ { "query": { "kind": "genre", "name": "Hardcore" } } ] }"#).unwrap();
        assert_eq!(s.volume, 0.5);
        assert!(!s.muted, "settings written before mute existed load unmuted");
        assert!(s.limit_peaks, "and with the peak limiter on");
        assert_eq!(s.gain, GainSetting::Track);
        assert_eq!(s.shelves.len(), 1);
        assert_eq!(s.shelves[0].limit, 18);
        assert!(s.shelves[0].visible);
        assert_eq!(s.shelves[0].title(), "Hardcore");
        assert!(!s.shuffle && s.repeat == RepeatSetting::Off && s.window.is_none(), "play modes and window default");
        assert_eq!((s.albums_sort, s.albums_desc), (AlbumSort::Artist, false), "the Albums page as it always was");
        assert_eq!(s.close_action, CloseAction::Hide, "closing still hides to the tray");
        assert_eq!(s.tray_click, TrayClick::PlayPause, "a click on the tray icon plays or pauses");
        assert!(s.output_device.is_none() && !s.exclusive, "the system default, shared, as before");
        assert!(s.shortcuts.is_empty(), "the default keys");
        assert!(!s.lyrics_online, "nothing is looked up online unless turned on");
        assert!(s.server_marks && s.server_plays && s.server_lastfm, "a server signed in to syncs favourites, reports plays, and Last.fm hears of them as before");
        assert!(!s.listenbrainz_love, "nothing is loved on ListenBrainz unless turned on");
        assert!(s.visualizer, "the visualizer is on");
        assert!(s.visualizer_look.is_none(), "its look not chosen yet");
        assert!(s.autoscroll && s.tray_animation, "middle-click scrolling and the animated tray icon are on");
        assert!(!s.tag_editing, "music files are only read unless tag editing is turned on");
        assert!(!Settings::default().tag_editing);
        assert!(!s.equalizer.all.enabled && s.equalizer.all.preamp_auto && s.equalizer.all.bands.len() == 10, "the equalizer is off, flat");
        assert!(!s.crossfade.enabled && s.crossfade.album_gapless, "no crossfade; albums gapless when it is on");
        assert_eq!(s.stream_cache_mb, 2048, "the stream cache keeps 2 GB");
        assert_eq!(s.stream_bitrate_kbps, None, "existing settings keep original-quality playback");
        let s: Settings = serde_json::from_str(r#"{ "albums_sort": "most_played", "albums_desc": true, "close_action": "quit" }"#).unwrap();
        assert_eq!((s.albums_sort, s.albums_desc, s.close_action), (AlbumSort::MostPlayed, true, CloseAction::Quit));
    }

    #[test]
    fn shortcuts_load_leniently() {
        // A malformed shortcuts entry loses only itself: the rest of the file stands.
        let s: Settings = serde_json::from_str(r#"{ "volume": 0.3, "shortcuts": { "mute": ["K"], "shuffle": "J", "repeat": 5, "next": ["F9", 2] } }"#).unwrap();
        assert_eq!(s.volume, 0.3);
        assert_eq!(s.shortcuts.get("mute"), Some(&vec!["K".to_string()]));
        assert_eq!(s.shortcuts.get("shuffle"), Some(&vec!["J".to_string()]), "one string is one key");
        assert!(!s.shortcuts.contains_key("repeat") && !s.shortcuts.contains_key("next"));
        let s: Settings = serde_json::from_str(r#"{ "volume": 0.3, "shortcuts": [1, 2] }"#).unwrap();
        assert_eq!((s.volume, s.shortcuts.len()), (0.3, 0));
        let s: Settings = serde_json::from_str(r#"{ "shortcuts": null }"#).unwrap();
        assert!(s.shortcuts.is_empty());
        // And through the keymap: the valid parts apply, the defaults fill the rest.
        let s: Settings = serde_json::from_str(r#"{ "shortcuts": { "mute": ["K"], "shuffle": ["Tab"], "bogus": ["X"] } }"#).unwrap();
        let (km, problems) = crate::keys::map::Keymap::from_overrides(&s.shortcuts);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(km.action_for("k", false, false, false), Some(crate::keys::map::Action::Mute));
        assert_eq!(km.action_for("s", false, false, false), Some(crate::keys::map::Action::Shuffle));
        // A shortcut saved for rating (an earlier version's): loaded without a problem and
        // gone, so the file is written back without it; the rest stands.
        let s: Settings = serde_json::from_str(r#"{ "volume": 0.3, "shortcuts": { "rate_5": ["Q"], "rate_0": [""], "mute": ["K"] } }"#).unwrap();
        assert_eq!(s.volume, 0.3);
        assert_eq!(s.shortcuts.keys().collect::<Vec<_>>(), ["mute"]);
        let (km, problems) = crate::keys::map::Keymap::from_overrides(&s.shortcuts);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(km.action_for("q", false, false, false), None);
        assert!(!serde_json::to_string(&s).unwrap().contains("rate_"));
    }

    #[test]
    fn untouched_old_default_shelves_become_the_new_ones() {
        // As an earlier version wrote them: the defaults, untouched.
        let old = serde_json::json!({ "volume": 0.4, "shelves": previous_default_shelves() }).to_string();
        let mut s: Settings = serde_json::from_str(&old).unwrap();
        assert!(s.upgrade_shelves());
        assert_eq!(s.shelves, default_shelves());
        assert_eq!(s.volume, 0.4, "nothing else changes");
        // Customised in any way: kept.
        for change in 0..4 {
            let mut shelves = previous_default_shelves();
            match change {
                0 => shelves[1].title = Some("Discover".into()),
                1 => shelves.swap(2, 3),
                2 => shelves[4].visible = false,
                _ => shelves[0].limit = 6,
            }
            let mut s = Settings { shelves: shelves.clone(), ..Settings::default() };
            assert!(!s.upgrade_shelves());
            assert_eq!(s.shelves, shelves, "change {change} kept");
        }
        // The new defaults name every shelf for what it holds, and no two neighbours alike.
        let d = default_shelves();
        assert!(d.iter().all(|s| !s.title().is_empty() && s.title() != "More"));
        assert!(d.windows(2).all(|w| w[0].query != w[1].query));
        assert_eq!(d.iter().filter(|s| s.layout == ShelfLayout::Hero).count(), 1);
        let titles: Vec<String> = d.iter().map(ShelfConfig::title).collect();
        let mut unique = titles.clone();
        unique.dedup();
        assert_eq!(unique, titles);
    }

    #[test]
    fn a_grid_shelf_asks_for_its_rows_of_covers() {
        let mut s = ShelfConfig::new(ShelfQuery::RecentlyAdded, ShelfLayout::Row, 18);
        assert_eq!((s.rows, s.fetch_limit(9), s.shown(18, 9)), (2, 18, 18), "a row shelf: its limit, all shown");
        s.layout = ShelfLayout::Grid;
        assert_eq!(s.fetch_limit(9), 18);
        assert_eq!(s.fetch_limit(26), 52, "as many columns as the width holds, past 20 too");
        assert_eq!(s.fetch_limit(0), 2, "never less than one column");
        s.rows = 99;
        assert_eq!(s.fetch_limit(9), GRID_ROWS_MAX * 9, "never more than the most rows");
        s.rows = 0;
        assert_eq!(s.fetch_limit(9), 9, "never less than one row");
        // Whole rows only, or one short row.
        s.rows = 2;
        assert_eq!(s.shown(18, 9), 18);
        assert_eq!(s.shown(13, 9), 9, "a second row would be short: one");
        assert_eq!(s.shown(5, 9), 5, "fewer than a row: one short row");
        assert_eq!(s.shown(0, 9), 0);
        // A settings file from before has no `rows`: two.
        let mut json = serde_json::to_value(ShelfConfig { rows: 5, ..s.clone() }).unwrap();
        json.as_object_mut().unwrap().remove("rows");
        let old: ShelfConfig = serde_json::from_value(json).unwrap();
        assert_eq!(old.rows, 2);
    }

    #[test]
    fn round_trips() {
        let mut s = Settings {
            shuffle: true,
            repeat: RepeatSetting::One,
            window: Some(WindowPlacement { left: -1900, top: 20, right: -100, bottom: 1000, maximized: true }),
            albums_sort: AlbumSort::Released,
            albums_desc: true,
            close_action: CloseAction::Quit,
            tray_click: TrayClick::ShowWindow,
            output_device: Some("{0.0.0.00000000}.{5b038}".into()),
            output_device_name: Some("Speakers (USB)".into()),
            exclusive: true,
            lyrics_online: true,
            visualizer: false,
            visualizer_look: Some(crate::visualizer::look::VizSettings { preset: "custom".into(), placement: crate::visualizer::look::Placement::Panel, ..Default::default() }),
            crossfade: CrossfadeSettings { enabled: true, secs: 4.0, album_gapless: false, on_skip: false },
            discord: DiscordSettings { enabled: true, app_id: "1234567890123456789".into() },
            ..Settings::default()
        };
        s.equalizer.all.enabled = true;
        s.equalizer.all.bands[3].gain = 4.5;
        s.equalizer.presets.push(UserPreset { name: "Mine".into(), bands: vec![EqBandSetting { kind: EqKindSetting::LowShelf, freq: 105.0, gain: 6.0, q: 0.7, width: EqWidthSetting::Q }, EqBandSetting { kind: EqKindSetting::LowPass, freq: 15_000.0, gain: 0.0, q: 0.6, width: EqWidthSetting::Octaves }], preamp_auto: false, preamp_db: -6.0 });
        s.equalizer.devices.insert("{dev}".into(), DeviceEq { name: "Headphones".into(), curve: EqCurve { enabled: true, preset: Some("Mine".into()), ..EqCurve::default() } });
        s.shortcuts.insert("mute".into(), vec!["K".into()]);
        s.shortcuts.insert("search".into(), vec![String::new(), "Ctrl+F".into()]);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }
}

#[cfg(test)]
mod eq_tests {
    use super::*;

    #[test]
    fn a_partial_equalizer_loads_with_defaults() {
        let s: Settings = serde_json::from_str(r#"{ "equalizer": { "all": { "enabled": true, "bands": [ { "freq": 100 } ] } }, "crossfade": { "enabled": true } }"#).unwrap();
        assert!(s.equalizer.all.enabled);
        assert_eq!(s.equalizer.all.bands, vec![EqBandSetting { kind: EqKindSetting::Peak, freq: 100.0, gain: 0.0, q: 1.41, width: EqWidthSetting::Q }]);
        assert!(s.equalizer.all.preamp_auto);
        assert_eq!(s.crossfade, CrossfadeSettings { enabled: true, ..CrossfadeSettings::default() });
    }

    /// A settings file from before the other filter kinds: every band reads as it was, and
    /// written again it is the same text (no width for a band with a Q).
    #[test]
    fn an_earlier_equalizer_loads_and_saves_unchanged() {
        let before = r#"{"equalizer":{"all":{"enabled":true,"preset":null,"bands":[{"kind":"low_shelf","freq":105.0,"gain":6.0,"q":0.7},{"kind":"peak","freq":1000.0,"gain":-3.0,"q":2.0}],"preamp_auto":true,"preamp_db":0.0},"presets":[{"name":"Mine","bands":[{"kind":"high_shelf","freq":8000.0,"gain":-2.0,"q":0.71}],"preamp_auto":false,"preamp_db":-4.0}],"devices":{}}}"#;
        let s: Settings = serde_json::from_str(before).unwrap();
        let b = &s.equalizer.all.bands;
        assert_eq!(b[0], EqBandSetting { kind: EqKindSetting::LowShelf, freq: 105.0, gain: 6.0, q: 0.7, width: EqWidthSetting::Q });
        assert_eq!(s.equalizer.presets[0].bands[0].kind, EqKindSetting::HighShelf);
        let again: serde_json::Value = serde_json::from_str(&serde_json::to_string(&s.equalizer).unwrap()).unwrap();
        let was: serde_json::Value = serde_json::from_str::<serde_json::Value>(before).unwrap()["equalizer"].clone();
        assert_eq!(again, was, "written back the same");
        // The new kinds and widths round trip.
        let band = EqBandSetting { kind: EqKindSetting::LowShelf, freq: 300.0, gain: 5.0, q: 0.9, width: EqWidthSetting::Slope };
        let text = serde_json::to_string(&band).unwrap();
        assert!(text.contains("\"width\":\"slope\""), "{text}");
        assert_eq!(serde_json::from_str::<EqBandSetting>(&text).unwrap(), band);
        let notch: EqBandSetting = serde_json::from_str(r#"{"kind":"notch","freq":50,"q":30}"#).unwrap();
        assert_eq!((notch.kind, notch.width), (EqKindSetting::Notch, EqWidthSetting::Q));
    }
}
