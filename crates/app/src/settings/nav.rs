//! Settings' sections and its "Find a setting" box. The page shows one section at a time
//! (`SettingsView.section`); which one is part of the Settings history entry
//! (`nav::Route::Settings`), so Back leaves Settings and returns to the section it left,
//! and opening Settings again shows the section last shown. The filter finds a setting by
//! its name or what it is about, across sections, and goes to it. Pure and unit-tested;
//! `views` shows the results.

use library::fold::fold;
use slint::ComponentHandle;

use crate::keys::map::Action;

/// The sections, in the order the page lists them (pages/settings/settings.slint, `SettingsLayout`).
pub const SECTIONS: [&str; 11] = ["Library", "Playback", "Equalizer", "Output", "Lyrics", "Visualizer", "Home", "Window", "Connections", "Keys", "About"];

pub const LIBRARY: usize = 0;
pub const PLAYBACK: usize = 1;
pub const EQUALIZER: usize = 2;
pub const OUTPUT: usize = 3;
pub const LYRICS: usize = 4;
pub const VISUALIZER: usize = 5;
pub const HOME: usize = 6;
pub const WINDOW: usize = 7;
pub const CONNECTIONS: usize = 8;
pub const KEYS: usize = 9;
pub const ABOUT: usize = 10;

/// Every setting the filter knows: its name as the page shows it (a row's label, so the
/// row can be outlined when the filter goes to it), its section, and other words someone
/// might look for it by.
const SETTINGS: &[(&str, usize, &str)] = &[
    ("Add folder", LIBRARY, "music folders library directory location path remove folder"),
    ("Rescan", LIBRARY, "scan scanning refresh update library status progress unreadable files skipped broken"),
    ("Remove missing tracks", LIBRARY, "held back missing deleted guard prune clean"),
    ("Server", LIBRARY, "plex token navidrome subsonic opensubsonic server remote network nas home sign in sign out log in connect address url user password account sync catalog streaming"),
    ("Sync favourites with the server", LIBRARY, "navidrome subsonic server favourite favorite heart hearts star starred two-way both ways write back"),
    ("Report plays to the server", LIBRARY, "navidrome subsonic server plays play count counts history now playing scrobble scrobbling"),
    ("Scrobble server tracks from this app", LIBRARY, "plex navidrome subsonic server last.fm lastfm last fm listenbrainz listen brainz listens scrobbling twice double duplicate"),
    ("Edit tags", LIBRARY, "tag editing editor write change modify fix metadata title artist album cover art picture retag tagger"),
    ("Undo history", LIBRARY, "tag editing undo revert journal edits saves clear disk space size"),
    ("ReplayGain", PLAYBACK, "volume levelling leveling loudness normalize normalise gain track album r128 lufs"),
    ("Preamp", PLAYBACK, "gain db decibel louder quieter replaygain"),
    ("Clipping protection", PLAYBACK, "peak clip clipping distortion replaygain"),
    ("Limit loud peaks", PLAYBACK, "limiter crackle distortion clipping loud mastering"),
    ("Crossfade", PLAYBACK, "cross fade fading blend mix overlap transition dj between tracks gap gapless"),
    ("Crossfade length", PLAYBACK, "cross fade seconds duration overlap how long"),
    ("Keep albums gapless", PLAYBACK, "gapless album live mix continuous cross fade"),
    ("Crossfade when skipping", PLAYBACK, "skip next previous manual cross fade"),
    ("Streaming quality", PLAYBACK, "plex navidrome subsonic server music bitrate kbps mp3 bandwidth remote mobile data hotspot"),
    ("Keep downloaded tracks", PLAYBACK, "plex navidrome subsonic server music from the server streaming stream cache downloads downloaded offline disk space size limit cap"),
    ("Downloaded tracks", PLAYBACK, "plex navidrome subsonic server music from the server streaming stream cache downloads clear delete free disk space size"),
    ("Equalizer", EQUALIZER, "eq equaliser equalization equalisation sound tone bass treble presets graphic parametric sliders bands autoeq headphones correction"),
    ("Equalizer preamp", EQUALIZER, "eq equaliser headroom clipping gain auto"),
    ("Separate setting for this device", EQUALIZER, "eq equaliser headphones speakers per device output device own preset"),
    ("Copy equalizer settings", EQUALIZER, "eq equaliser clipboard copy share send export equalizer apo autoeq peace graphiceq wavelet easyeffects text"),
    ("Paste equalizer settings", EQUALIZER, "eq equaliser clipboard paste import equalizer apo autoeq parametriceq fixedbandeq graphiceq wavelet room eq wizard rew easyeffects pulseeffects peace headphone correction preset file"),
    ("Output device", OUTPUT, "speakers headphones dac sound card audio device default wasapi"),
    ("Exclusive mode", OUTPUT, "exclusive bit-perfect bitperfect wasapi sample rate mixer audiophile"),
    ("Look up lyrics online", LYRICS, "lrclib internet network online privacy lyrics synced now playing"),
    ("Visualizer", VISUALIZER, "spectrum bars now playing frequency animation analyser analyzer on off"),
    ("Presets", VISUALIZER, "visualizer look preset classic prism skyline calm bonfire halo saturn galaxy swarm eclipse reset"),
    ("Your presets", VISUALIZER, "visualizer look preset save mine own rename update delete share send copy paste clipboard chat export import file mpviz"),
    ("Style", VISUALIZER, "visualizer bars line ring radial circle halo cover jacket ripples sparks embers bonfire fire saturn dust orbit galaxy spiral swarm whirl eclipse flare variant kind"),
    ("Where", VISUALIZER, "visualizer placement behind panel tab full page fullscreen full screen position mode shift+v"),
    ("Colours", VISUALIZER, "visualizer colors colours gradient palette accent cover rainbow prism classic custom"),
    ("Reflection", VISUALIZER, "visualizer mirror reflection reflex"),
    ("Detail", VISUALIZER, "visualizer bars resolution width count fine coarse"),
    ("Peak caps", VISUALIZER, "visualizer peaks caps hold"),
    ("Speed", VISUALIZER, "visualizer motion smoothing smooth snappy fast slow decay"),
    ("Sensitivity", VISUALIZER, "visualizer gain level db range height loud quiet"),
    ("Around the cover", VISUALIZER, "visualizer ring kind variant ripples saturn galaxy swarm eclipse halo circle"),
    ("Pulse the cover", VISUALIZER, "visualizer ring cover jacket artwork zoom pulse beat kick bounce"),
    ("Intensity", VISUALIZER, "visualizer ring halo swing peaks motion movement bigger smaller ripples"),
    ("Opacity", VISUALIZER, "visualizer prominence opacity faint strong strength transparency transparent see-through dim behind the page"),
    ("Frame rate", VISUALIZER, "visualizer fps frames 30 60 battery laptop cpu power"),
    ("Home shelves", HOME, "home page shelves edit order hide recently added random explore favourites"),
    ("Interface size", WINDOW, "zoom zoom in zoom out scale scaling bigger smaller larger text size font size magnify ui dpi display pinch ctrl plus minus 100% accessibility"),
    ("Close button", WINDOW, "tray quit exit minimize close window background"),
    ("Click on the tray icon", WINDOW, "tray notification area taskbar icon click double click play pause show hide window middle wheel scroll volume next"),
    // Its name is the platform's (`find_in`'s `named`).
    ("Start with Windows", WINDOW, "startup autostart auto start login sign in boot launch tray minimized"),
    ("Default app for music files", WINDOW, "default apps associations file types open with mp3 flac register choose"),
    ("Animated tray icon", WINDOW, "tray notification area taskbar icon animation moving bars"),
    ("Auto-hide player bar", WINDOW, "player bar hide controls bottom"),
    ("Show technical details", WINDOW, "format codec bitrate kbps sample rate khz bit depth 24-bit hi-res lossless flac mp3 channels quality audio properties info"),
    ("Middle-click scrolling", WINDOW, "autoscroll auto scroll middle mouse button wheel click pan browser"),
    ("Last.fm", CONNECTIONS, "scrobble scrobbling account api key secret connect disconnect lastfm last fm services accounts"),
    ("ListenBrainz", CONNECTIONS, "listenbrainz listen brainz musicbrainz metabrainz listens scrobble scrobbling history account user token connect disconnect services accounts"),
    ("ListenBrainz server address", CONNECTIONS, "listenbrainz self-hosted self hosted own server compatible maloja api url address http"),
    ("Love favourites on ListenBrainz", CONNECTIONS, "listenbrainz love loved like liked feedback favourite favorite heart hearts musicbrainz recording"),
    ("Show on Discord", CONNECTIONS, "discord rich presence rpc status activity listening to profile now playing services accounts"),
    ("Application ID", CONNECTIONS, "discord rich presence rpc client id app developer portal own name custom"),
    ("Reset all keys", KEYS, "shortcuts hotkeys keyboard remap bindings defaults"),
    ("Version", ABOUT, "about version number build release update copy bug report system windows renderer edition portable installed copyright"),
    ("Report a problem", ABOUT, "diagnostics copy diagnostics crash crashed closed problem bug report support help log send error"),
    ("Settings", ABOUT, "where files data folder location appdata settings.json config configuration open folder"),
    ("Library database", ABOUT, "where files data folder location appdata library.db sqlite schema backup plays favourites playlists disk space size"),
    ("Art cache", ABOUT, "where files cache covers artwork images thumbnails localappdata disk space size delete"),
    ("Lyrics cache", ABOUT, "where files cache lrclib localappdata disk space size delete"),
    ("Log", ABOUT, "where files marquee-player.log logging diagnostics problem bug report"),
    ("Albums", ABOUT, "library statistics stats count tracks total length duration hours size how many"),
    ("Built with", ABOUT, "acknowledgements credits open source licences licenses third-party slint symphonia opus sqlite skia"),
];

/// Settings shown only in some editions or on some systems (`platform::integration`).
fn offered(label: &str) -> bool {
    let edition = crate::edition::get();
    match label {
        "Start with Windows" => crate::platform::integration::autostart(edition).is_some(),
        "Default app for music files" => crate::platform::integration::default_apps_available(edition),
        "Animated tray icon" => crate::platform::system::TRAY_ANIMATES,
        "Sync favourites with the server" | "Report plays to the server" => {
            crate::state::with(|st| st.app.upgrade().is_none_or(|app| !app.global::<crate::Server>().get_plex())).unwrap_or(true)
        }
        // Settings > Playback > Music from the server, shown once there is a server or a cache.
        "Streaming quality" | "Keep downloaded tracks" | "Downloaded tracks" => crate::server::streaming::settings_shown(),
        _ => true,
    }
}

/// A setting found: its name, its section.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub label: String,
    pub section: usize,
}

/// The settings `query` finds: each of its words somewhere in a setting's name, section
/// or other words (case, accents and width ignored). Settings whose name matches come
/// first, then the rest; the keys' actions (Keys) are found by what they do.
pub fn find(query: &str) -> Vec<Hit> {
    find_in(query, offered)
}

/// `find` among the settings `offered` says are shown.
fn find_in(query: &str, offered: impl Fn(&str) -> bool) -> Vec<Hit> {
    let words: Vec<String> = fold(query).split_whitespace().map(str::to_owned).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let actions = Action::ALL.iter().map(|a| (a.describe(), KEYS, "key shortcut"));
    // The exclusive setting's name is the platform's ("Bit-perfect playback" on Linux), and so
    // is starting with the system's ("Start with Windows", "Open at login" on macOS).
    let named = |(label, section, other): (&'static str, usize, &'static str)| {
        let words = crate::platform::system::words();
        let label = match label {
            "Exclusive mode" => words.exclusive_title,
            "Start with Windows" => words.autostart,
            _ => label,
        };
        (label, section, other)
    };
    let all: Vec<(&str, usize, &str)> = SETTINGS.iter().copied().filter(|(label, ..)| offered(label)).map(named).chain(actions).collect();
    let mut by_name = Vec::new();
    let mut by_other = Vec::new();
    for (label, section, other) in all {
        let name = fold(label);
        let hay = format!("{name} {} {}", fold(SECTIONS[section]), fold(other));
        if !words.iter().all(|w| hay.contains(w.as_str())) {
            continue;
        }
        let hit = Hit { label: label.to_owned(), section };
        if words.iter().all(|w| name.contains(w.as_str())) {
            by_name.push(hit);
        } else {
            by_other.push(hit);
        }
    }
    by_name.extend(by_other);
    by_name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(q: &str) -> Vec<String> {
        find(q).into_iter().map(|h| h.label).collect()
    }

    #[test]
    fn finds_by_name_and_by_what_it_is_about() {
        assert_eq!(find("exclusive"), vec![Hit { label: crate::platform::system::words().exclusive_title.into(), section: OUTPUT }]);
        assert_eq!(labels("Lyrics online"), vec!["Look up lyrics online"]);
        assert_eq!(labels("normalise"), vec!["ReplayGain"]);
        // Only where the system has it (Windows), and the default apps only when installed
        // (never in a test run).
        let autostart = crate::platform::integration::autostart(crate::edition::get()).is_some();
        let start = crate::platform::system::words().autostart;
        // The moving tray icon only where the tray takes one (not macOS).
        let mut tray = vec!["Click on the tray icon", "Animated tray icon", "Close button", start];
        tray.retain(|&l| (l != "Animated tray icon" || crate::platform::system::TRAY_ANIMATES) && (l != start || autostart));
        assert_eq!(labels("tray"), tray);
        assert_eq!(labels("tray click"), vec!["Click on the tray icon"]);
        assert_eq!(labels("double click"), vec!["Click on the tray icon"]);
        assert_eq!(labels("autostart").len(), usize::from(autostart));
        // Found under the platform's name, the label its row shows.
        if autostart {
            assert_eq!(labels("autostart"), vec![start]);
        }
        assert!(labels("associations").is_empty());
        assert_eq!(labels("autoscroll"), vec!["Middle-click scrolling"]);
        assert_eq!(labels("middle mouse"), vec!["Middle-click scrolling"]);
        assert_eq!(labels("tray animation").len(), usize::from(crate::platform::system::TRAY_ANIMATES));
        assert_eq!(labels("scrobble"), vec!["Scrobble server tracks from this app", "Report plays to the server", "Last.fm", "ListenBrainz"], "the server's switch by its name, then by what each is about");
        assert_eq!(labels("double scrobble"), vec!["Scrobble server tracks from this app"]);
        assert_eq!(labels("listenbrainz"), vec!["ListenBrainz", "ListenBrainz server address", "Love favourites on ListenBrainz", "Scrobble server tracks from this app"]);
        assert_eq!(labels("self-hosted"), vec!["ListenBrainz server address"]);
        assert_eq!(labels("love"), vec!["Love favourites on ListenBrainz"]);
        assert_eq!(find("listen brainz token")[0], Hit { label: "ListenBrainz".into(), section: CONNECTIONS });
        assert_eq!(labels("favourites server"), vec!["Sync favourites with the server"]);
        assert_eq!(labels("plex token"), vec!["Server"]);
        assert_eq!(labels("hearts navidrome"), vec!["Sync favourites with the server"]);
        assert_eq!(find("play counts")[0], Hit { label: "Report plays to the server".into(), section: LIBRARY });
        assert_eq!(labels("discord"), vec!["Show on Discord", "Application ID"]);
        assert_eq!(labels("rich presence"), vec!["Show on Discord", "Application ID"]);
        assert_eq!(labels("bitrate"), vec!["Show technical details"]);
        assert!(labels("sample rate").contains(&"Show technical details".to_owned()), "and Exclusive mode, also about it");
        assert_eq!(find("flac")[0], Hit { label: "Show technical details".into(), section: WINDOW });
        assert_eq!(labels("technical"), vec!["Show technical details"]);
        assert_eq!(find("version")[0], Hit { label: "Version".into(), section: ABOUT });
        assert_eq!(labels("licences"), vec!["Built with"]);
        assert_eq!(labels("licenses"), vec!["Built with"]);
        assert_eq!(labels("bug report"), vec!["Version", "Report a problem", "Log"]);
        assert_eq!(labels("crash"), vec!["Report a problem"]);
        assert_eq!(labels("disk space"), vec!["Undo history", "Library database", "Art cache", "Lyrics cache"]);
        assert_eq!(labels("tag editing"), vec!["Review and save tag edits (View metadata, with tag editing on)", "Edit tags", "Undo history"], "the setting, its history and its key");
        assert_eq!(labels("how many tracks"), vec!["Albums"]);
        assert_eq!(labels("appdata"), vec!["Settings", "Library database", "Art cache", "Lyrics cache"]);
    }

    /// The server's stream cache is found once its card is shown (a server or a cache), and
    /// not before.
    #[test]
    fn the_stream_cache_is_found_when_its_card_is_shown() {
        let shown = |q: &str| find_in(q, |_| true).into_iter().map(|h| (h.label, h.section)).collect::<Vec<_>>();
        assert_eq!(shown("stream cache"), vec![("Keep downloaded tracks".to_owned(), PLAYBACK), ("Downloaded tracks".to_owned(), PLAYBACK)]);
        assert!(shown("navidrome").iter().any(|(l, _)| l == "Server"), "the server first of all");
        assert!(shown("disk space").contains(&("Downloaded tracks".to_owned(), PLAYBACK)));
        assert!(shown("bitrate").contains(&("Streaming quality".to_owned(), PLAYBACK)));
        let hidden = |q: &str| find_in(q, |l| !matches!(l, "Streaming quality" | "Keep downloaded tracks" | "Downloaded tracks"));
        assert!(hidden("bitrate").iter().all(|hit| hit.label != "Streaming quality"));
        assert!(hidden("stream cache").is_empty());
    }

    #[test]
    fn names_come_before_other_words() {
        // "Visualizer" by name, then the V key's action (it says "Visualizer" too), and
        // nothing that only mentions it among other words comes first.
        let v = labels("visualizer");
        assert_eq!(v[0], "Visualizer");
        assert!(v.contains(&"Visualizer on now playing on / off".to_owned()));
    }

    #[test]
    fn finds_key_actions_in_keys() {
        let hits = find("mute");
        assert_eq!(hits, vec![Hit { label: "Mute / unmute".into(), section: KEYS }]);
        assert!(find("shortcut").iter().all(|h| h.section == KEYS));
    }

    #[test]
    fn every_word_must_match_and_case_and_accents_are_ignored() {
        assert!(find("exclusive tray").is_empty());
        assert_eq!(labels("PREAMP"), vec!["Preamp", "Equalizer preamp"]);
        assert_eq!(labels("préamp"), vec!["Preamp", "Equalizer preamp"]);
        assert!(find("   ").is_empty());
    }

    #[test]
    fn every_section_has_a_setting() {
        for (s, name) in SECTIONS.iter().enumerate() {
            assert!(SETTINGS.iter().any(|(_, sec, _)| *sec == s), "section {name}");
        }
    }
}
