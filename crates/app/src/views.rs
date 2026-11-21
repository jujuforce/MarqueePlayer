//! Navigation and filling the page stores from the library.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use library::query::{AlbumSort, Group, SearchKind, ShelfOpts};
use library::{Album, ShelfQuery, query};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::covers::{self, AlbumModel, Kind};
use crate::settings::{self, ShelfLayout};
use crate::nav::Route;
use crate::state::State;
use crate::{AlbumPage, ArtistPage, Backdrop, Browse, Grid, Home, Library, Nav, Page, Player, PlaylistRow, Playlists, Search, SettingHit, SettingsView, ShelfEdit, ShelfEditor, ShelfPage, ShelfView, Theme, TrackPage};

// ---- navigation ------------------------------------------------------------
//
// Every place the user can be is a `Route`, so back and forward restore the album,
// artist or playlist that was open, not just the kind of page.

/// Go somewhere new: the current place goes on the back stack, forward is cleared.
pub fn navigate(st: &Rc<State>, to: Route) {
    let current = st.history.borrow().current.clone();
    if current == to {
        // Same place again (sidebar item for the page already shown): refresh only.
        show(st, &to, app_scroll(st));
        return;
    }
    let scroll = app_scroll(st);
    if show(st, &to, 0.0) {
        st.history.borrow_mut().push(to, scroll);
        transition(st, 1);
    }
    sync_nav(st);
}

/// Start a page-change transition: +1 forward or somewhere new, -1 back.
fn transition(st: &Rc<State>, dir: i32) {
    if let Some(app) = st.app.upgrade() {
        let nav = app.global::<Nav>();
        nav.set_transition_dir(dir);
        nav.set_transition_seq(nav.get_transition_seq().wrapping_add(1));
    }
}

/// How far the current page is scrolled (`Nav.page-scroll`, a content offset, <= 0).
fn app_scroll(st: &Rc<State>) -> f32 {
    st.app.upgrade().map_or(0.0, |app| app.global::<Nav>().get_page_scroll())
}

pub fn go(st: &Rc<State>, page: Page) {
    if page == Page::Settings {
        // Settings opens on the section shown last.
        let section = st.app.upgrade().map_or(0, |app| app.global::<SettingsView>().get_section().max(0) as usize);
        navigate(st, Route::Settings(section));
        return;
    }
    navigate(st, Route::Page(page));
}

// ---- Settings ------------------------------------------------------------------------

/// Show a Settings section (chosen in its list): the Settings entry now names it, the
/// column starts at its top, and the filter is cleared.
pub fn settings_section(st: &Rc<State>, section: usize) {
    let section = section.min(settings::nav::SECTIONS.len() - 1);
    let on_settings = matches!(st.history.borrow().current, Route::Settings(_));
    if !on_settings {
        navigate(st, Route::Settings(section));
        return;
    }
    // A key button waiting for a key goes with the Keys section; About's measuring with
    // About.
    crate::keys::edit::leave(st);
    crate::about::leave();
    st.history.borrow_mut().replace(Route::Settings(section));
    let Some(app) = st.app.upgrade() else { return };
    let v = app.global::<SettingsView>();
    v.set_filter("".into());
    v.set_hits(ModelRc::default());
    v.set_highlight("".into());
    v.set_section(section as i32);
    app.global::<Nav>().set_page_scroll(0.0);
    if section == settings::nav::ABOUT {
        crate::about::refresh(st);
    }
}

/// What "Find a setting" finds for `text` (the field holds the text itself).
pub fn settings_filter(st: &Rc<State>, text: &str) {
    let Some(app) = st.app.upgrade() else { return };
    let v = app.global::<SettingsView>();
    if v.get_filter().as_str() != text {
        v.set_filter(text.into());
    }
    let hits: Vec<SettingHit> = settings::nav::find(text)
        .into_iter()
        .map(|h| SettingHit { label: h.label.into(), section: h.section as i32, section_name: settings::nav::SECTIONS[h.section].into() })
        .collect();
    v.set_hits(ModelRc::new(VecModel::from(hits)));
    app.global::<Nav>().set_page_scroll(0.0);
    // The filter's results replace the section: its preview shows, or no longer does.
    crate::visualizer::wake(st);
}

thread_local! {
    static HIGHLIGHT_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Go to a setting the filter found: its section, its row scrolled into view and
/// outlined for a moment.
pub fn settings_jump(st: &Rc<State>, section: usize, label: &str) {
    settings_section(st, section);
    let Some(app) = st.app.upgrade() else { return };
    app.global::<SettingsView>().set_highlight(label.into());
    let generation = HIGHLIGHT_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let weak = st.app.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(2400), move || {
        if HIGHLIGHT_GEN.with(|g| g.get()) == generation
            && let Some(app) = weak.upgrade()
        {
            app.global::<SettingsView>().set_highlight("".into());
        }
    });
}

pub fn back(st: &Rc<State>) -> bool {
    step(st, true)
}

pub fn forward(st: &Rc<State>) -> bool {
    step(st, false)
}

/// Walk the history one place, skipping places that no longer exist (a deleted playlist).
/// Returns false, and says so briefly in the navigation pill, when there is nowhere to go.
fn step(st: &Rc<State>, back: bool) -> bool {
    let can = {
        let h = st.history.borrow();
        if back { h.can_back() } else { h.can_forward() }
    };
    if !can {
        flash(st, if back { "Nothing to go back to" } else { "Nothing to go forward to" });
        return false;
    }
    loop {
        let scroll = app_scroll(st);
        let next = {
            let mut h = st.history.borrow_mut();
            if back { h.back(scroll) } else { h.forward(scroll) }
        };
        match next {
            Some((route, saved)) if show(st, &route, saved) => {
                transition(st, if back { -1 } else { 1 });
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    sync_nav(st);
    true
}

thread_local! {
    static FLASH_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A short note in the navigation pill, cleared after a moment.
pub fn flash(st: &Rc<State>, text: &str) {
    flash_for(st, text, 1600);
}

/// A note that should be read, not just glimpsed (a track that could not be played).
pub fn flash_long(st: &Rc<State>, text: &str) {
    flash_for(st, text, 4500);
}

/// A change the user made that the library did not take (a full disk, the database held by
/// another program): logged, and said in the pill. The caller leaves the page as the library has
/// it.
pub fn not_written(st: &Rc<State>, what: &str, e: &dyn std::fmt::Display) {
    log::warn!("{what}: {e}");
    flash_long(st, &format!("{what}: the library cannot be written now"));
}

/// A note that stays until replaced or cleared (`flash_clear`): a server track loading.
pub fn flash_hold(st: &Rc<State>, text: &str) {
    let Some(app) = st.app.upgrade() else { return };
    app.global::<Nav>().set_flash(text.into());
    // A shorter note's timer before it must not clear it.
    FLASH_GEN.with(|g| g.set(g.get() + 1));
}

/// Clear the pill if it still says `text`.
pub fn flash_clear(st: &Rc<State>, text: &str) {
    let Some(app) = st.app.upgrade() else { return };
    let nav = app.global::<Nav>();
    if nav.get_flash() == text {
        nav.set_flash("".into());
    }
}

fn flash_for(st: &Rc<State>, text: &str, ms: u64) {
    let Some(app) = st.app.upgrade() else { return };
    app.global::<Nav>().set_flash(text.into());
    let generation = FLASH_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let weak = st.app.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
        if FLASH_GEN.with(|g| g.get()) == generation
            && let Some(app) = weak.upgrade()
        {
            app.global::<Nav>().set_flash("".into());
        }
    });
}

fn sync_nav(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let h = st.history.borrow();
    let nav = app.global::<Nav>();
    nav.set_can_back(h.can_back());
    nav.set_can_forward(h.can_forward());
    nav.set_title(h.current.title().into());
}

/// Fill the page for `route` and show it, scrolled to `scroll`. False if it no longer
/// exists.
fn show(st: &Rc<State>, route: &Route, scroll: f32) -> bool {
    let Some(app) = st.app.upgrade() else { return false };
    // A key button of Settings > Keys still waiting goes with its page (its focus-lost is
    // not guaranteed when the page is destroyed). About stops measuring.
    crate::keys::edit::leave(st);
    crate::about::leave();
    // A selection belongs to the page it was made on.
    crate::selected::clear_all(st);
    let page = match route {
        Route::Album(id) => {
            if !fill_album(st, *id) {
                return false;
            }
            Page::Album
        }
        Route::Artist(name) => {
            fill_artist(st, name);
            Page::Artist
        }
        Route::Playlist(id) => {
            // A smart playlist has a page of its own (playlists/smart.rs).
            if crate::playlists::smart::is_smart(st, *id) {
                if !crate::playlists::smart::fill_page(st, *id) {
                    return false;
                }
                Page::Smart
            } else {
                if !fill_playlist(st, *id) {
                    return false;
                }
                Page::Playlist
            }
        }
        Route::SmartEdit(id) => {
            if !crate::playlists::smart::fill_editor(st, *id) {
                return false;
            }
            Page::SmartEdit
        }
        Route::Shelf { query, title, seed } => {
            fill_shelf(st, query, title, *seed);
            Page::Shelf
        }
        Route::SearchAll { kind, query } => fill_search_all(st, *kind, query),
        Route::Settings(section) => {
            crate::output::rescan_devices();
            let v = app.global::<SettingsView>();
            v.set_section(*section as i32);
            v.set_filter("".into());
            v.set_hits(ModelRc::default());
            v.set_highlight("".into());
            if *section == settings::nav::ABOUT {
                crate::about::refresh(st);
            }
            Page::Settings
        }
        Route::Page(p) => {
            match p {
                Page::Albums => ensure_grid(st),
                Page::Artists => fill_artists(st),
                Page::Genres => fill_groups(st, false),
                Page::Labels => fill_groups(st, true),
                Page::Playlists => refresh_playlists(st),
                Page::Shelves => refresh_shelf_editor(st),
                // Plays and favourites change while away; random shelves keep their
                // shuffle (see shelf_seed).
                Page::Home => refresh_home(st),
                _ => {}
            }
            *p
        }
    };
    // The scroll goes in before the page switch, so the page is created already there.
    app.global::<Nav>().set_page_scroll(scroll);
    app.global::<Nav>().set_page(page);
    update_backdrop(st);
    true
}

/// The open album's art on its page; otherwise the playing album's.
pub fn update_backdrop(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let page = app.global::<Nav>().get_page();
    let stem = if page == Page::Album {
        st.open_album.get().and_then(|id| query::album(&st.db, id).ok().flatten()).and_then(|a| a.art)
    } else {
        st.queue.borrow().current().and_then(|i| i.track.art.clone())
    };
    let weak = st.app.clone();
    match stem {
        Some(stem) => covers::with_image(&stem, Kind::Blur, move |img| {
            if let Some(app) = weak.upgrade() {
                let b = app.global::<Backdrop>();
                b.set_image(img);
                b.set_visible(true);
            }
        }),
        None => app.global::<Backdrop>().set_visible(false),
    }
}

// ---- home --------------------------------------------------------------------

fn slint_layout(l: ShelfLayout) -> crate::ShelfLayout {
    match l {
        ShelfLayout::Row => crate::ShelfLayout::Row,
        ShelfLayout::Grid => crate::ShelfLayout::Grid,
        ShelfLayout::Hero => crate::ShelfLayout::Hero,
    }
}

thread_local! {
    /// The home page's shelves as shown, for a hero shelf to change the album it features.
    static HOME: std::cell::RefCell<Option<Rc<VecModel<ShelfView>>>> = const { std::cell::RefCell::new(None) };
    /// The album each hero shelf features (by the shelf's place in the settings), kept while
    /// the page is refreshed.
    static HERO_AT: std::cell::RefCell<std::collections::HashMap<usize, usize>> = std::cell::RefCell::new(std::collections::HashMap::new());
    /// The art stems of each hero shelf's albums, for the blur behind the one featured.
    static HERO_ART: std::cell::RefCell<std::collections::HashMap<usize, Vec<Option<String>>>> = std::cell::RefCell::new(std::collections::HashMap::new());
    /// The covers a row of Home's grid shelves holds, as the page last reported it (0: not yet).
    static GRID_COLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The covers a row of Home's grid shelves holds: as the page reported it, or until then as
/// the window's width would hold them (lists.slint, `CoverGrid`).
fn grid_cols(st: &State) -> usize {
    let reported = GRID_COLS.with(|c| c.get());
    if reported > 0 {
        return reported;
    }
    let Some(app) = st.app.upgrade() else { return 1 };
    let theme = app.global::<Theme>();
    let width = app.window().size().to_logical(app.window().scale_factor()).width;
    let gap = 24.0;
    ((width - 2.0 * theme.get_page_margin() + gap) / (theme.get_cover_size() + gap)).floor().max(1.0) as usize
}

/// Home's grid shelves now hold `cols` covers a row (the window's width or the interface size
/// changed): each grid shelf takes exactly its rows of them, in place; nothing else changes.
pub fn home_grid_cols(st: &Rc<State>, cols: usize) {
    let cols = cols.max(1);
    if GRID_COLS.with(|c| c.replace(cols)) == cols {
        return;
    }
    let Some(vm) = HOME.with(|h| h.borrow().clone()) else { return };
    let shelves = st.settings.borrow().shelves.clone();
    let models = st.models.home.borrow().clone();
    for (row, m) in models.iter().enumerate().take(vm.row_count()) {
        let Some(v) = vm.row_data(row) else { continue };
        let Some(s) = shelves.get(v.index as usize).filter(|s| s.layout == ShelfLayout::Grid) else { continue };
        match shelf_albums(st, v.index as usize, s, s.fetch_limit(cols), cols) {
            Ok(albums) if !albums.is_empty() => m.update(albums),
            Ok(_) => {}
            Err(e) => log::warn!("shelf {:?}: {e}", s.query),
        }
    }
}

/// A shelf's albums, `limit` of them asked for, then those it shows (whole rows of `cols`
/// for a grid).
fn shelf_albums(st: &Rc<State>, index: usize, s: &crate::settings::ShelfConfig, limit: usize, cols: usize) -> rusqlite::Result<Vec<Album>> {
    let random = s.query == ShelfQuery::Random;
    let opts = ShelfOpts {
        seed: if random { shelf_seed(st, index) } else { 0 },
        // The big card wants art: a random hero shelf only features albums with an
        // image.
        art_only: random && s.layout == ShelfLayout::Hero,
    };
    // A smart playlist's shelf is worked out off the UI thread (playlists/smart.rs): what was last
    // worked out shows, and the shelf comes in when it is first ready.
    let mut albums = match &s.query {
        ShelfQuery::Playlist { id } if crate::playlists::smart::is_smart(st, *id) => {
            crate::playlists::smart::shelf_albums(st, *id, limit).unwrap_or_default().into_iter().take(s.fetch_limit(cols)).collect()
        }
        _ => query::shelf_with(&st.db, &s.query, s.fetch_limit(cols), opts)?,
    };
    albums.truncate(s.shown(albums.len(), cols));
    Ok(albums)
}

/// The visible shelves that have albums: (place in the settings, the shelf, its albums).
fn home_shelves(st: &Rc<State>) -> Vec<(usize, crate::settings::ShelfConfig, Vec<Album>)> {
    let shelves = st.settings.borrow().shelves.clone();
    let cols = grid_cols(st);
    // Shelves of one smart playlist share its worked-out albums: asked for with the most any
    // of them shows, so that shelves of different lengths do not ask again in turn.
    let mut smart_limit: HashMap<i64, usize> = HashMap::new();
    for s in shelves.iter().filter(|s| s.visible) {
        if let ShelfQuery::Playlist { id } = s.query {
            let l = smart_limit.entry(id).or_default();
            *l = (*l).max(s.fetch_limit(cols));
        }
    }
    let mut out = Vec::new();
    for (index, s) in shelves.into_iter().enumerate().filter(|(_, s)| s.visible) {
        let limit = match s.query {
            ShelfQuery::Playlist { id } => smart_limit.get(&id).copied().unwrap_or(s.fetch_limit(cols)),
            _ => s.fetch_limit(cols),
        };
        match shelf_albums(st, index, &s, limit, cols) {
            Ok(a) if !a.is_empty() => out.push((index, s, a)),
            Ok(_) => {}
            Err(e) => log::warn!("shelf {:?}: {e}", s.query),
        }
    }
    out
}

/// A shelf's row on the home page, its albums' model, and the art its hero card features.
fn shelf_view(st: &Rc<State>, index: usize, s: &crate::settings::ShelfConfig, albums: Vec<Album>) -> (ShelfView, Rc<AlbumModel>, Option<Option<String>>) {
    let hero = HERO_AT.with(|h| h.borrow().get(&index).copied().unwrap_or(0)).min(albums.len().saturating_sub(1));
    let mut stem = None;
    if s.layout == ShelfLayout::Hero {
        stem = Some(albums.get(hero).and_then(|a| a.art.clone()));
        HERO_ART.with(|a| a.borrow_mut().insert(index, albums.iter().map(|a| a.art.clone()).collect()));
    }
    let m = AlbumModel::new(albums);
    let view = ShelfView {
        title: shelf_title(st, s).into(),
        layout: slint_layout(s.layout),
        albums: ModelRc::from(m.clone()),
        hero: hero as i32,
        hero_blur: Default::default(),
        index: index as i32,
        random: s.query == ShelfQuery::Random,
        rows: s.rows.clamp(1, crate::settings::GRID_ROWS_MAX) as i32,
    };
    (view, m, stem)
}

pub fn refresh_home(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let mut models = Vec::new();
    let mut views = Vec::new();
    let mut hero_stems = Vec::new();
    for (index, s, albums) in home_shelves(st) {
        let (view, m, stem) = shelf_view(st, index, &s, albums);
        if let Some(stem) = stem {
            hero_stems.push((views.len(), stem));
        }
        views.push(view);
        models.push(m);
    }
    let (album_count, _) = query::counts(&st.db).unwrap_or((0, 0));
    let home = app.global::<Home>();
    home.set_empty(album_count == 0);
    let vm = Rc::new(VecModel::from(views));
    home.set_shelves(ModelRc::from(vm.clone()));
    *st.models.home.borrow_mut() = models;
    for (row, stem) in hero_stems {
        hero_blur(&vm, row, stem);
    }
    HOME.with(|h| *h.borrow_mut() = Some(vm));
}

/// The home page, open while a scan writes the library: each shelf keeps the albums it shows,
/// where they are (their covers come in as they are found), and takes new ones after them up to
/// its length; a shelf that gets its first albums comes in between the others. Nothing moves
/// under the pointer. (`grow`: false at the scan's end, when each shelf shows what it now holds,
/// a shelf only starting again when its albums changed places.)
fn refresh_home_live(st: &Rc<State>, grow: bool) {
    let Some(app) = st.app.upgrade() else { return };
    let home = app.global::<Home>();
    let Some(vm) = HOME.with(|h| h.borrow().clone()) else { return refresh_home(st) };
    if home.get_empty() || vm.row_count() != st.models.home.borrow().len() {
        return refresh_home(st);
    }
    let wanted = home_shelves(st);
    let mut models = st.models.home.borrow().clone();
    let mut row = 0;
    for (index, s, albums) in wanted.iter() {
        // Rows of shelves not wanted any more before this one: kept while growing, else gone.
        while row < vm.row_count() && vm.row_data(row).is_some_and(|v| (v.index as usize) < *index) {
            if grow {
                row += 1;
            } else {
                vm.remove(row);
                models.remove(row);
            }
        }
        if vm.row_data(row).is_some_and(|v| v.index as usize == *index) {
            let m = &models[row];
            let cols = grid_cols(st);
            let next = if grow || s.query == ShelfQuery::Random {
                let mut next = m.grown(albums.clone(), s.fetch_limit(cols), |id| query::album(&st.db, id).ok().flatten());
                next.truncate(s.shown(next.len(), cols));
                next
            } else {
                albums.clone()
            };
            m.update(next);
        } else {
            let (view, m, stem) = shelf_view(st, *index, s, albums.clone());
            vm.insert(row, view);
            models.insert(row, m);
            if let Some(stem) = stem {
                hero_blur(&vm, row, stem);
            }
        }
        row += 1;
    }
    if !grow {
        while row < vm.row_count() {
            vm.remove(row);
            models.remove(row);
        }
    }
    *st.models.home.borrow_mut() = models;
}

/// The blurred art behind a hero shelf, for the album it features (none: the card's own
/// colour).
fn hero_blur(vm: &Rc<VecModel<ShelfView>>, row: usize, stem: Option<String>) {
    let Some(stem) = stem else {
        if let Some(mut v) = vm.row_data(row) {
            v.hero_blur = Default::default();
            vm.set_row_data(row, v);
        }
        return;
    };
    let vm = vm.clone();
    let Some(want) = vm.row_data(row).map(|v| (v.index, v.hero)) else { return };
    covers::with_image(&stem, Kind::Blur, move |img| {
        // By the shelf, not the row: rows may be put in above it meanwhile (Home during a scan).
        let Some(row) = (0..vm.row_count()).find(|&r| vm.row_data(r).is_some_and(|v| v.index == want.0)) else { return };
        if let Some(mut v) = vm.row_data(row)
            && v.hero == want.1
        {
            v.hero_blur = img;
            vm.set_row_data(row, v);
        }
    });
}

/// A hero shelf (`index`, its place in the settings) features its album `album`.
pub fn hero_select(index: usize, album: usize) {
    HOME.with(|h| {
        let Some(vm) = h.borrow().clone() else { return };
        let Some(row) = (0..vm.row_count()).find(|&r| vm.row_data(r).is_some_and(|v| v.index == index as i32)) else { return };
        let Some(mut v) = vm.row_data(row) else { return };
        if album >= v.albums.row_count() || v.hero == album as i32 {
            return;
        }
        HERO_AT.with(|h| h.borrow_mut().insert(index, album));
        v.hero = album as i32;
        vm.set_row_data(row, v);
        let art = HERO_ART.with(|a| a.borrow().get(&index).and_then(|v| v.get(album).cloned()).flatten());
        hero_blur(&vm, row, art);
    });
}

/// The shuffle of the random shelf at `index`, picked once and kept until reshuffled.
fn shelf_seed(st: &State, index: usize) -> i64 {
    *st.shelf_seeds.borrow_mut().entry(index).or_insert_with(new_seed)
}

fn new_seed() -> i64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (t as i64) ^ ((t >> 64) as i64)
}

/// A new shuffle for the random shelf at `index`.
pub fn reshuffle_shelf(st: &Rc<State>, index: usize) {
    st.shelf_seeds.borrow_mut().insert(index, new_seed());
    HERO_AT.with(|h| h.borrow_mut().remove(&index));
    refresh_home(st);
}

/// "See all" on a home shelf: everything in that shelf's order, on its own page. A
/// playlist's shelf opens the playlist itself; a random shelf keeps its shuffle, so the
/// page starts with the albums the shelf showed.
pub fn open_shelf(st: &Rc<State>, index: usize) {
    let Some(s) = st.settings.borrow().shelves.get(index).cloned() else { return };
    if let ShelfQuery::Playlist { id } = s.query {
        open_playlist(st, id);
        return;
    }
    let seed = if s.query == ShelfQuery::Random { shelf_seed(st, index) } else { 0 };
    navigate(st, Route::Shelf { title: s.title(), query: s.query, seed });
}

fn fill_shelf(st: &Rc<State>, q: &ShelfQuery, title: &str, seed: i64) {
    let Some(app) = st.app.upgrade() else { return };
    let albums = query::shelf_with(&st.db, q, usize::MAX >> 1, ShelfOpts { seed, art_only: false }).unwrap_or_default();
    let p = app.global::<ShelfPage>();
    p.set_title(title.into());
    p.set_count(albums.len() as i32);
    p.set_random(*q == ShelfQuery::Random);
    st.models.shelf.set(albums);
    p.set_albums(ModelRc::from(st.models.shelf.clone()));
}

/// Reshuffle the open shelf page: a new seed for this place, back to the top.
pub fn reshuffle_shelf_page(st: &Rc<State>) {
    let route = {
        let mut h = st.history.borrow_mut();
        let Route::Shelf { seed, .. } = &mut h.current else { return };
        *seed = new_seed();
        h.current.clone()
    };
    show(st, &route, 0.0);
}

// ---- all albums ----------------------------------------------------------------

pub fn ensure_grid(st: &Rc<State>) {
    if st.grid_loaded.get() {
        return;
    }
    refresh_grid(st);
}

/// Read every album in the chosen order (settings), then show what the quick filter lets
/// through. The filter itself runs in memory (`filter_grid`), so typing never waits on the
/// database; only a new order, or a library change, reads it again.
pub fn refresh_grid(st: &Rc<State>) {
    let (sort, desc) = {
        let s = st.settings.borrow();
        (s.albums_sort, s.albums_desc)
    };
    let albums = query::all_albums(&st.db, sort, desc).unwrap_or_default();
    let keys = albums.iter().map(grid_key).collect();
    {
        let mut g = st.grid.borrow_mut();
        g.all = albums;
        g.keys = keys;
    }
    st.grid_loaded.set(true);
    show_grid(st);
}

/// What the quick filter looks in: the title and the artist, folded as search folds them.
fn grid_key(a: &Album) -> String {
    library::fold::fold(&format!("{}\u{1f}{}", a.title, a.artist))
}

/// The albums a filter lets through, as indices into `keys`: every word of it somewhere
/// in the title or the artist, folded (case, accents, kana and width ignored).
pub fn filter_albums(keys: &[String], filter: &str) -> Vec<usize> {
    let folded = library::fold::fold(filter);
    let words: Vec<&str> = folded.split_whitespace().collect();
    keys.iter().enumerate().filter(|(_, k)| words.iter().all(|w| k.contains(w))).map(|(i, _)| i).collect()
}

/// The Albums page's quick filter changed.
pub fn filter_grid(st: &Rc<State>, filter: &str) {
    st.grid.borrow_mut().filter = filter.to_owned();
    if let Some(app) = st.app.upgrade() {
        app.global::<Grid>().set_content_y(0.0);
    }
    show_grid(st);
}

fn show_grid(st: &Rc<State>) {
    show_grid_as(st, false);
}

/// The grid as `st.grid` has it; `calm`: in place (`AlbumModel::update`), the page as it is.
fn show_grid_as(st: &Rc<State>, calm: bool) {
    let Some(app) = st.app.upgrade() else { return };
    let (shown, total) = {
        let g = st.grid.borrow();
        let shown: Vec<Album> = if g.filter.trim().is_empty() {
            g.all.clone()
        } else {
            filter_albums(&g.keys, &g.filter).into_iter().map(|i| g.all[i].clone()).collect()
        };
        (shown, g.all.len())
    };
    let n = shown.len();
    let g = app.global::<Grid>();
    if calm {
        st.models.grid.update(shown);
    } else {
        st.models.grid.set(shown);
        g.set_albums(ModelRc::from(st.models.grid.clone()));
    }
    let s = st.settings.borrow();
    g.set_count(n as i32);
    g.set_total(total as i32);
    g.set_sort_label(s.albums_sort.label().into());
    g.set_descending(s.albums_desc);
}

/// The Albums page, open while a scan writes the library: the albums shown stay where they
/// are (their covers come in), new ones come after them, in the page's order among
/// themselves; the page takes its order again when it is next opened (or at the scan's end,
/// when it is at its top). An album gone meanwhile (a rescan regrouping) starts it again.
fn grid_live(st: &Rc<State>) {
    if !st.grid_loaded.get() {
        return refresh_grid(st);
    }
    let (sort, desc) = {
        let s = st.settings.borrow();
        (s.albums_sort, s.albums_desc)
    };
    let albums = query::all_albums(&st.db, sort, desc).unwrap_or_default();
    {
        let mut g = st.grid.borrow_mut();
        let mut by_id: std::collections::HashMap<i64, Album> = albums.iter().map(|a| (a.id, a.clone())).collect();
        if g.all.iter().any(|a| !by_id.contains_key(&a.id)) {
            drop(g);
            return refresh_grid(st);
        }
        let shown: HashSet<i64> = g.all.iter().map(|a| a.id).collect();
        let mut all: Vec<Album> = g.all.iter().map(|a| by_id.remove(&a.id).unwrap_or_else(|| a.clone())).collect();
        all.extend(albums.into_iter().filter(|a| !shown.contains(&a.id)));
        g.keys = all.iter().map(grid_key).collect();
        g.all = all;
    }
    show_grid_as(st, true);
}

/// A new order for the Albums page (from its sort menu): in that order's natural
/// direction, from the top.
pub fn sort_grid(st: &Rc<State>, sort: AlbumSort) {
    {
        let mut s = st.settings.borrow_mut();
        s.albums_sort = sort;
        s.albums_desc = sort.default_desc();
    }
    regrid(st);
}

/// The Albums page's direction button.
pub fn flip_grid(st: &Rc<State>) {
    {
        let mut s = st.settings.borrow_mut();
        s.albums_desc = !s.albums_desc;
    }
    regrid(st);
}

fn regrid(st: &Rc<State>) {
    st.save_settings_soon();
    refresh_grid(st);
    if let Some(app) = st.app.upgrade() {
        app.global::<Grid>().set_content_y(0.0);
    }
}

// ---- artists, genres, labels ---------------------------------------------------------

/// Browse tiles as albums, for the lazy cover model: the name as the title, the album
/// count as the artist line (`AlbumTile` with `group` shows it as plain text).
fn group_albums(groups: Vec<Group>) -> Vec<Album> {
    groups
        .into_iter()
        .enumerate()
        .map(|(i, g)| Album {
            id: i as i64,
            artist: match g.albums {
                0 => "On tracks".to_owned(),
                1 => "1 album".to_owned(),
                n => format!("{n} albums"),
            },
            title: g.name,
            date: None,
            genre: None,
            label: None,
            release_type: None,
            art: g.art,
            ..Default::default()
        })
        .collect()
}

thread_local! {
    /// The keys of the artist tiles shown (Artists, or a search's artists in full), by
    /// place: a tile opens its artist exactly, also when two share a name.
    static ARTIST_KEYS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Credits as links, by (kind, id, credit as shown): asked for by every row drawn.
    static LINKS: std::cell::RefCell<std::collections::HashMap<(bool, i32, String), Vec<crate::ArtistLink>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn show_artists(st: &Rc<State>, title: &str, groups: Vec<Group>) {
    let Some(app) = st.app.upgrade() else { return };
    let n = groups.len();
    ARTIST_KEYS.with(|k| *k.borrow_mut() = groups.iter().map(|g| g.key.clone()).collect());
    st.models.artists.set(group_albums(groups));
    let b = app.global::<Browse>();
    b.set_artists_title(title.into());
    b.set_artists_count(n as i32);
    b.set_artists(ModelRc::from(st.models.artists.clone()));
}

fn fill_artists(st: &Rc<State>) {
    let groups = query::album_artists(&st.db).unwrap_or_default();
    show_artists(st, "Artists", groups);
}

fn fill_groups(st: &Rc<State>, labels: bool) {
    let Some(app) = st.app.upgrade() else { return };
    let groups = query::album_groups(&st.db, labels).unwrap_or_default();
    let n = groups.len();
    st.models.groups.set(group_albums(groups));
    let b = app.global::<Browse>();
    b.set_groups_count(n as i32);
    b.set_groups(ModelRc::from(st.models.groups.clone()));
}

/// The Artists index or a Genres or Labels page, open while a scan runs: in place (tiles move
/// only when one comes in before them, which a library read in its folders' order rarely does).
fn browse_live(st: &Rc<State>, page: Page) {
    let Some(app) = st.app.upgrade() else { return };
    let b = app.global::<Browse>();
    match page {
        Page::Artists => {
            let groups = query::album_artists(&st.db).unwrap_or_default();
            ARTIST_KEYS.with(|k| *k.borrow_mut() = groups.iter().map(|g| g.key.clone()).collect());
            b.set_artists_count(groups.len() as i32);
            st.models.artists.update(group_albums(groups));
        }
        Page::Genres | Page::Labels => {
            let groups = query::album_groups(&st.db, page == Page::Labels).unwrap_or_default();
            b.set_groups_count(groups.len() as i32);
            st.models.groups.update(group_albums(groups));
        }
        _ => {}
    }
}

/// How often the open page takes in what a running scan has written: often enough to see
/// albums come in, seldom enough that the page is calm to read and the UI thread does little.
const LIVE_EVERY: std::time::Duration = std::time::Duration::from_millis(2500);
/// Smart playlists (worked out again from their rules) and the browse indexes, less often.
const LIVE_SLOW: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Default)]
struct LiveRefresh {
    timer: Option<slint::Timer>,
    last: Option<std::time::Instant>,
    last_slow: Option<std::time::Instant>,
    /// Smart playlists not worked out again since the library changed.
    smart_due: bool,
}

thread_local! {
    static LIVE: std::cell::RefCell<LiveRefresh> = std::cell::RefCell::default();
}

/// A running scan wrote to the library (a batch of albums, or covers). The first albums of an
/// empty library show at once; after that the open page takes the news in at most every
/// `LIVE_EVERY`, in place (`refresh_live`); pages not open take it in when they are next shown.
/// Nothing runs between batches.
pub fn library_changed_live(st: &Rc<State>) {
    let empty = st.app.upgrade().is_some_and(|app| app.global::<Home>().get_empty());
    LIVE.with(|l| l.borrow_mut().smart_due = true);
    if empty {
        LIVE.with(|l| l.borrow_mut().timer = None);
        refresh_live(st);
        return;
    }
    LIVE.with(|l| {
        let mut l = l.borrow_mut();
        if l.timer.as_ref().is_some_and(slint::Timer::running) {
            return;
        }
        let wait = l.last.map_or(std::time::Duration::ZERO, |t| LIVE_EVERY.saturating_sub(t.elapsed()));
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, wait.max(std::time::Duration::from_millis(50)), || {
            crate::state::with(|st| {
                // (Not after the job: its end refreshes everything.)
                if st.scanning.get() || crate::server::syncing() {
                    refresh_live(st);
                }
            });
        });
        l.timer = Some(t);
    });
}

fn refresh_live(st: &Rc<State>) {
    let now = std::time::Instant::now();
    let slow = LIVE.with(|l| {
        let mut l = l.borrow_mut();
        l.last = Some(now);
        let slow = l.last_slow.is_none_or(|t| now.duration_since(t) >= LIVE_SLOW);
        if slow {
            l.last_slow = Some(now);
        }
        slow
    });
    let Some(app) = st.app.upgrade() else { return };
    let page = app.global::<Nav>().get_page();
    // Every other page is read again when it is next shown.
    if page != Page::Albums {
        st.grid_loaded.set(false);
    }
    // Home, not open, has albums now: it is built when shown (and the next news waits its turn).
    if page != Page::Home && app.global::<Home>().get_empty() && query::counts(&st.db).is_ok_and(|c| c.0 > 0) {
        app.global::<Home>().set_empty(false);
    }
    match page {
        Page::Home => refresh_home_live(st, true),
        Page::Albums => grid_live(st),
        Page::Artists if st.history.borrow().current == Route::Page(Page::Artists) => {
            if slow {
                browse_live(st, page);
            }
        }
        Page::Genres | Page::Labels if slow => browse_live(st, page),
        _ => {}
    }
    if slow && LIVE.with(|l| std::mem::take(&mut l.borrow_mut().smart_due)) {
        forget_links();
        crate::playlists::smart::changed(st);
    }
}

/// The end of a scan that changed the library: what `refresh_all` does, but the page open
/// stays calm: it shows what the library holds now in place, and the Albums page takes its
/// order again only when it is at its top (else when it is next opened).
pub fn scan_finished(st: &Rc<State>) {
    LIVE.with(|l| *l.borrow_mut() = LiveRefresh::default());
    forget_links();
    crate::playlists::smart::changed(st);
    refresh_playlists(st);
    let Some(app) = st.app.upgrade() else { return };
    let page = app.global::<Nav>().get_page();
    match page {
        Page::Home => refresh_home_live(st, false),
        Page::Albums if app.global::<Grid>().get_content_y() >= -1.0 => refresh_grid(st),
        Page::Albums => {
            grid_live(st);
            // Its order, when it is next opened.
            st.grid_loaded.set(false);
        }
        Page::Artists if st.history.borrow().current == Route::Page(Page::Artists) => browse_live(st, page),
        Page::Genres | Page::Labels => browse_live(st, page),
        Page::Playlist | Page::Album => refresh_open_list(st, page),
        _ => {}
    }
    if page != Page::Albums && st.grid_loaded.get() {
        refresh_grid(st);
    }
    if page != Page::Home {
        // Home is worked out whenever it is shown.
        let (albums, _) = query::counts(&st.db).unwrap_or((0, 0));
        app.global::<Home>().set_empty(albums == 0);
    }
}

/// A genre's albums (or a label's, from the Labels page), as a shelf's full page.
pub fn open_group(st: &Rc<State>, name: &str) {
    let labels = st.app.upgrade().is_some_and(|app| app.global::<Nav>().get_page() == Page::Labels);
    let query = if labels { ShelfQuery::Label { name: name.to_owned() } } else { ShelfQuery::Genre { name: name.to_owned() } };
    navigate(st, Route::Shelf { query, title: name.to_owned(), seed: 0 });
}

// ---- search sections in full --------------------------------------------------------

/// A section in full asked of the search thread (search.rs), and what it found once it has.
struct SearchAll {
    kind: SearchKind,
    query: String,
    /// Where "See all" was clicked: the page opens there only if the user is still there.
    /// None: the page is already shown (back, forward, a refresh), and is filled in place.
    from: Option<Route>,
    results: Option<library::SearchResults>,
}

thread_local! {
    static SEARCH_ALL: std::cell::RefCell<Option<SearchAll>> = const { std::cell::RefCell::new(None) };
}

/// "See all" beside a search section: that section, all of it, on its own page, opened when
/// the search thread has it (an operator search can take a while: never on the UI thread).
pub fn open_search_all(st: &Rc<State>, kind: &str) {
    let Some(app) = st.app.upgrade() else { return };
    let kind = match kind {
        "albums" => SearchKind::Albums,
        "artists" => SearchKind::Artists,
        _ => SearchKind::Tracks,
    };
    let query = app.global::<Search>().get_query().trim().to_owned();
    if !query.is_empty() {
        st.searcher.request_section(&query, kind);
        let from = Some(st.history.borrow().current.clone());
        SEARCH_ALL.with(|p| *p.borrow_mut() = Some(SearchAll { kind, query, from, results: None }));
    }
}

/// Section `kind` of `q` in full, from the search thread: its page is opened (or, already
/// open, filled), unless a newer one was asked for or the user has gone elsewhere.
pub fn show_search_all(st: &Rc<State>, id: u64, kind: SearchKind, q: String, results: library::SearchResults) {
    if !st.searcher.is_latest_section(id) {
        return;
    }
    let Some(mut p) = SEARCH_ALL.with(|p| p.borrow_mut().take()) else { return };
    if p.kind != kind || p.query != q {
        return;
    }
    let to = Route::SearchAll { kind, query: q };
    let current = st.history.borrow().current.clone();
    if !search_all_opens(p.from.as_ref(), &current, &to) {
        return;
    }
    p.results = Some(results);
    SEARCH_ALL.with(|s| *s.borrow_mut() = Some(p));
    navigate(st, to);
}

/// Whether a section in full, arriving while the user is at `current`, is shown (`to`): where
/// "See all" was clicked (`from`), or on its own page already; not once the user has gone
/// somewhere else.
fn search_all_opens(from: Option<&Route>, current: &Route, to: &Route) -> bool {
    current == to || from == Some(current)
}

/// Fill a search section's full page: with what the search thread found for it, when that
/// is what opens it; otherwise (back, forward, a refresh) it is asked for again, and the
/// page filled when it comes (until then a page newly shown is empty, one already shown
/// stays as it was).
fn fill_search_all(st: &Rc<State>, kind: SearchKind, q: &str) -> Page {
    let found = SEARCH_ALL.with(|p| {
        let mut p = p.borrow_mut();
        match p.as_mut() {
            Some(s) if s.kind == kind && s.query == q && s.results.is_some() => {
                let r = s.results.take();
                *p = None;
                r
            }
            _ => None,
        }
    });
    let page = match kind {
        SearchKind::Albums => Page::Shelf,
        SearchKind::Artists => Page::Artists,
        SearchKind::Tracks => Page::Tracks,
    };
    let waiting = found.is_none();
    let r = match found {
        Some(r) => r,
        None => {
            st.searcher.request_section(q, kind);
            SEARCH_ALL.with(|p| *p.borrow_mut() = Some(SearchAll { kind, query: q.to_owned(), from: None, results: None }));
            let here = st.history.borrow().current == Route::SearchAll { kind, query: q.to_owned() };
            if here {
                return page;
            }
            library::SearchResults::default()
        }
    };
    let title = crate::nav::search_title(kind, q);
    let Some(app) = st.app.upgrade() else { return Page::Search };
    match kind {
        SearchKind::Albums => {
            let p = app.global::<ShelfPage>();
            p.set_title(title.into());
            p.set_count(r.albums.len() as i32);
            p.set_random(false);
            st.models.shelf.set(r.albums);
            p.set_albums(ModelRc::from(st.models.shelf.clone()));
            Page::Shelf
        }
        SearchKind::Artists => {
            // Album counts and covers from the artists' albums; an artist only on tracks
            // (a featured one) has neither.
            let all: std::collections::HashMap<String, Group> =
                query::album_artists(&st.db).unwrap_or_default().into_iter().map(|g| (g.key.clone(), g)).collect();
            let groups = r
                .artists
                .into_iter()
                .map(|a| all.get(&a.key).cloned().unwrap_or(Group { name: a.name, key: a.key, albums: 0, art: None }))
                .collect();
            show_artists(st, &title, groups);
            Page::Artists
        }
        SearchKind::Tracks => {
            let total: f64 = r.tracks.iter().map(|t| t.duration).sum();
            let n = r.tracks.len();
            let favs = favourite_set(st, r.tracks.iter().map(|t| t.id));
            st.lists.tracks.set(r.tracks, favs, false);
            st.lists.tracks.set_playing(playing_track_id(st));
            let p = app.global::<TrackPage>();
            p.set_title(title.into());
            p.set_info(if waiting { "Searching\u{2026}".to_owned() } else { format!("{n} {} · {}", if n == 1 { "track" } else { "tracks" }, duration_text(total)) }.into());
            p.set_tracks(ModelRc::from(st.lists.tracks.clone()));
            Page::Tracks
        }
    }
}

pub(crate) fn duration_text(total: f64) -> String {
    if total >= 3600.0 {
        format!("{} h {} min", (total / 3600.0) as u64, ((total % 3600.0) / 60.0) as u64)
    } else {
        format!("{} min", (total / 60.0).round() as u64)
    }
}

// ---- keyboard ----------------------------------------------------------------------

/// A control took the keyboard focus by Tab, at window `y` with height `h`: scroll the page
/// (`Nav.page-scroll`) just enough to show it between the top bar and the player bar.
/// Pages that scroll otherwise (the Albums grid) keep their header in view anyway.
pub fn reveal(st: &Rc<State>, y: f32, h: f32) {
    let Some(app) = st.app.upgrade() else { return };
    let nav = app.global::<Nav>();
    let page = nav.get_page();
    if matches!(page, Page::Albums | Page::NowPlaying) {
        return;
    }
    let theme = app.global::<Theme>();
    let height = app.window().size().to_logical(app.window().scale_factor()).height;
    // Below the pill (the search page's field; Settings' title, over its column), above
    // the player bar.
    let top = match page {
        Page::Search => theme.get_page_top() + 52.0,
        // Under a card held at the column's top (the visualizer's preview), if one is.
        Page::Settings => app.global::<SettingsView>().get_column_top() + app.global::<SettingsView>().get_pin_cover(),
        _ => theme.get_page_top(),
    };
    let bottom = height - theme.get_bar_height() - 16.0;
    if let Some(scroll) = reveal_scroll(nav.get_page_scroll(), y, h, top, bottom) {
        nav.set_page_scroll(scroll);
    }
}

/// The page scroll that shows `y..y+h` within `top..bottom` (window coordinates, `scroll` a
/// content offset <= 0), or `None` when it already shows. Too tall to fit: its top.
pub fn reveal_scroll(scroll: f32, y: f32, h: f32, top: f32, bottom: f32) -> Option<f32> {
    let to = if y < top || h > bottom - top {
        scroll + (top - y)
    } else if y + h > bottom {
        scroll - (y + h - bottom)
    } else {
        return None;
    };
    Some(to.min(0.0))
}

// ---- album -----------------------------------------------------------------------

/// Which of these tracks are favourites: one query, however many tracks.
pub(crate) fn favourite_set(st: &State, ids: impl Iterator<Item = i64>) -> HashSet<i64> {
    let all = query::favourite_track_ids(&st.db).unwrap_or_default();
    ids.filter(|id| all.contains(id)).collect()
}

pub fn playing_track_id(st: &State) -> i64 {
    st.queue.borrow().current().map_or(-1, |i| i.track.id)
}

pub fn open_album(st: &Rc<State>, id: i64) {
    navigate(st, Route::Album(id));
}

fn fill_album(st: &Rc<State>, id: i64) -> bool {
    let Some(app) = st.app.upgrade() else { return false };
    let Ok(Some(album)) = query::album(&st.db, id) else { return false };
    let tracks = query::album_tracks(&st.db, id).unwrap_or_default();
    // No disc number is disc 1 (the album's order puts it there).
    let discs: HashSet<u32> = tracks.iter().map(|t| t.disc_no.unwrap_or(1)).collect();
    let total: f64 = tracks.iter().map(|t| t.duration).sum();
    let mut info = Vec::new();
    if let Some(d) = &album.date {
        info.push(d.clone());
    }
    info.push(format!("{} {}", tracks.len(), if tracks.len() == 1 { "track" } else { "tracks" }));
    info.push(duration_text(total));
    // The album's genres (every genre of its tracks, the most carried first), a few.
    let genres = query::album_genre_names(&st.db, id).unwrap_or_default();
    if !genres.is_empty() {
        info.push(genres.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
    } else if let Some(g) = &album.genre {
        info.push(g.clone());
    }
    let favs = favourite_set(st, tracks.iter().map(|t| t.id));
    // The album's format in one line; its rows show theirs only where they differ.
    let summary = library::audio::album_summary(&tracks.iter().map(|t| (t.audio, t.duration)).collect::<Vec<_>>());
    st.lists.album.set_details_column(summary.column);
    let page = app.global::<AlbumPage>();
    page.set_heading_counts(crate::rows::album_heading_counts(&tracks, discs.len() > 1));
    st.lists.album.set(tracks, favs, discs.len() > 1);
    st.lists.album.set_playing(playing_track_id(st));

    page.set_album(covers::card(&album, None));
    page.set_info(info.join("  ·  ").into());
    page.set_details(summary.line.into());
    page.set_details_column(summary.column != library::audio::Column::Hidden);
    page.set_label(album.label.clone().unwrap_or_default().into());
    page.set_tracks(ModelRc::from(st.lists.album.clone()));
    page.set_favourite(query::is_favourite_album(&st.db, id).unwrap_or(false));
    page.set_full(Default::default());
    if let Some(stem) = &album.art {
        let weak = st.app.clone();
        covers::with_image(stem, Kind::Full, move |img| {
            if let Some(app) = weak.upgrade() {
                let p = app.global::<AlbumPage>();
                if p.get_album().id == id as i32 {
                    p.set_full(img);
                }
            }
        });
    }
    st.open_album.set(Some(id));
    true
}

// ---- artist ---------------------------------------------------------------------

/// An artist's page, by key ("mb:…", "n:…", from a link) or by name; a credit that names
/// several ("A feat. B", from a file outside the library) opens its first artist's.
pub fn open_artist(st: &Rc<State>, key_or_name: &str) {
    let found = query::find_artist(&st.db, key_or_name).ok().flatten().or_else(|| {
        library::names::credits(key_or_name).first().and_then(|c| query::find_artist(&st.db, &c.name).ok().flatten())
    });
    match found {
        Some(a) => navigate(st, Route::Artist(a.key)),
        None => flash(st, "Not in the library"),
    }
}

/// The artist tile at `index` of the artists shown.
pub fn open_artist_at(st: &Rc<State>, index: usize) {
    if let Some(key) = ARTIST_KEYS.with(|k| k.borrow().get(index).cloned()) {
        open_artist(st, &key);
    }
}

/// A credit as links: each credited artist where their name stands in it (`kind` "track"
/// or "album", `id` its id), the rest plain text. A file from outside the library, or a
/// credit whose names cannot all be found in it, lists the artists instead.
pub fn artist_links(st: &Rc<State>, kind: &str, id: i32, shown: &str) -> Vec<crate::ArtistLink> {
    let album = kind == "album";
    let key = (album, id, shown.to_owned());
    if let Some(v) = LINKS.with(|l| l.borrow().get(&key).cloned()) {
        return v;
    }
    let refs: Vec<query::ArtistRef> = match (album, id > 0) {
        (true, true) => query::album_artist_refs(&st.db, i64::from(id)).unwrap_or_default(),
        (false, true) => query::track_artists(&st.db, i64::from(id)).unwrap_or_default(),
        _ => Vec::new(),
    };
    let refs: Vec<(String, String)> = if refs.is_empty() {
        // Not in the library: the credit taken apart as the scanner would.
        library::names::credits(shown).into_iter().map(|c| (c.name.clone(), c.name)).collect()
    } else {
        refs.into_iter().map(|r| (r.key, r.name)).collect()
    };
    let names: Vec<&str> = refs.iter().map(|r| r.1.as_str()).collect();
    let link = |text: &str, key: &str| crate::ArtistLink { text: text.into(), key: key.into() };
    let out: Vec<crate::ArtistLink> = match library::names::link_parts(shown, &names) {
        Some(parts) => parts.iter().map(|(t, i)| link(t, i.map_or("", |i| refs[i].0.as_str()))).collect(),
        None => {
            let mut v = Vec::new();
            for (i, (k, n)) in refs.iter().enumerate() {
                if i > 0 {
                    v.push(link(", ", ""));
                }
                v.push(link(n, k));
            }
            v
        }
    };
    LINKS.with(|l| {
        let mut l = l.borrow_mut();
        if l.len() > 20_000 {
            l.clear();
        }
        l.insert(key, out.clone());
    });
    out
}

/// After a scan: credits may have changed.
pub fn forget_links() {
    LINKS.with(|l| l.borrow_mut().clear());
}

fn fill_artist(st: &Rc<State>, key: &str) {
    let Some(app) = st.app.upgrade() else { return };
    let artist = query::find_artist(&st.db, key).ok().flatten();
    let id = artist.as_ref().map_or(-1, |a| a.id);
    let (own, appears) = query::artist_albums(&st.db, id).unwrap_or_default();
    let top = query::artist_top_tracks(&st.db, id, 8).unwrap_or_default();
    let favs = favourite_set(st, top.iter().map(|t| t.id));
    st.lists.artist.set(top, favs, false);
    st.lists.artist.set_playing(playing_track_id(st));
    st.models.artist_albums.set(own);
    st.models.artist_appears.set(appears);
    let p = app.global::<ArtistPage>();
    p.set_name(artist.map_or_else(|| "Not in the library".to_owned(), |a| a.name).into());
    p.set_albums(ModelRc::from(st.models.artist_albums.clone()));
    p.set_appears(ModelRc::from(st.models.artist_appears.clone()));
    p.set_top(ModelRc::from(st.lists.artist.clone()));
}

// ---- search ---------------------------------------------------------------------

/// Search for `q`, off the UI thread (search.rs); the results land in `show_search`.
pub fn search(st: &Rc<State>, q: &str) {
    st.searcher.request(q);
}

/// Results of search request `id`, shown only if no newer request was made since.
pub fn show_search(st: &Rc<State>, id: u64, r: library::SearchResults) {
    if !st.searcher.is_latest(id) {
        return;
    }
    let Some(app) = st.app.upgrade() else { return };
    let favs = favourite_set(st, r.tracks.iter().map(|t| t.id));
    st.lists.search.set(r.tracks, favs, false);
    st.lists.search.set_playing(playing_track_id(st));
    st.models.search_albums.set(r.albums);
    let s = app.global::<Search>();
    s.set_albums(ModelRc::from(st.models.search_albums.clone()));
    s.set_tracks(ModelRc::from(st.lists.search.clone()));
    let artists: Vec<crate::ArtistLink> = r.artists.iter().map(|a| crate::ArtistLink { text: a.name.as_str().into(), key: a.key.as_str().into() }).collect();
    s.set_artists(ModelRc::new(VecModel::from(artists)));
    s.set_more_albums(r.more_albums);
    s.set_more_artists(r.more_artists);
    s.set_more_tracks(r.more_tracks);
    s.set_operators(r.operators);
    s.set_hint(if r.notes.is_empty() { "".into() } else { format!("Searched as text: {}", r.notes.join(" \u{b7} ")).into() });
}

// ---- playlists ------------------------------------------------------------------

pub fn refresh_playlists(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let lists = query::playlists(&st.db).unwrap_or_default();
    let rows: Vec<PlaylistRow> = crate::playlists::smart::playlist_rows(&lists);
    let model = ModelRc::new(VecModel::from(rows));
    app.global::<Playlists>().set_list(model.clone());
    app.global::<ShelfEditor>().set_playlists(model);
}

/// The smart playlists that name playlist `id` in their rules, as Delete says them: "“Best
/// of”", "“A” and “B”", "“A” and 3 more"; empty when none does.
pub fn used_by(st: &Rc<State>, id: i64) -> String {
    let names = library::query::smart_playlists_using(&st.db, id).unwrap_or_default();
    let q = |n: &String| format!("\u{201c}{n}\u{201d}");
    match names.as_slice() {
        [] => String::new(),
        [a] => format!("{} uses it", q(a)),
        [a, b] => format!("{} and {} use it", q(a), q(b)),
        [a, rest @ ..] => format!("{} and {} more use it", q(a), rest.len()),
    }
}

pub fn open_playlist(st: &Rc<State>, id: i64) {
    navigate(st, Route::Playlist(id));
}

/// Re-read the open playlist in place (after a rename or a removal), without history.
pub fn reload_playlist(st: &Rc<State>, id: i64) {
    if crate::playlists::smart::is_smart(st, id) {
        crate::playlists::smart::fill_page(st, id);
    } else {
        fill_playlist(st, id);
    }
    sync_nav(st);
}

/// A playlist's tracks or name changed: the lists of playlists, and what smart playlists
/// that name it hold.
pub fn playlists_changed(st: &Rc<State>) {
    refresh_playlists(st);
    crate::playlists::smart::changed(st);
}

/// Show `to` in place of the current place (no new history entry: the editor saved becomes
/// the playlist it saved).
pub fn replace(st: &Rc<State>, to: Route) {
    st.history.borrow_mut().replace(to.clone());
    show(st, &to, 0.0);
    transition(st, 1);
    sync_nav(st);
}

/// Albums, tracks or a queue row dropped on a playlist in the sidebar: appended to it.
pub fn drop_on_playlist(st: &Rc<State>, payload: crate::dnd::Payload, pid: i64) {
    if crate::playlists::smart::is_smart(st, pid) {
        crate::playlists::smart::hint(st);
        return;
    }
    // A dragged row carries the rows chosen with it (selected.rs).
    let tracks = match payload {
        crate::dnd::Payload::Album(id) => query::album_tracks(&st.db, id).unwrap_or_default(),
        crate::dnd::Payload::Track { ctx, index } => crate::selected::dragged(st, &ctx, index),
        crate::dnd::Payload::QueueRow(i) => crate::selected::dragged(st, crate::selected::QUEUE, i),
    };
    if tracks.is_empty() {
        return;
    }
    crate::selected::add_tracks_to_playlist(st, tracks, pid);
}

/// Move a row of the open playlist from `from` to the line before `at` (as its drop line
/// shows it), and show the playlist in its new order where it was scrolled.
/// The rows are addressed by the tracks shown, not by stored positions (a scan that removes
/// a track leaves a gap in them), and nothing moves if the playlist changed meanwhile.
pub fn move_in_playlist(st: &Rc<State>, from: usize, at: usize) {
    let Some(id) = st.open_playlist.get() else { return };
    let shown: Vec<i64> = st.lists.playlist.tracks().iter().map(|t| t.id).collect();
    let (order, _) = crate::selection::move_block(shown.len(), &[from], at);
    let new: Vec<i64> = order.iter().map(|&j| shown[j]).collect();
    if new == shown {
        return;
    }
    match query::rewrite_playlist(&st.db, id, &shown, &new) {
        Ok(true) => {}
        Ok(false) => flash(st, "The playlist changed meanwhile: nothing was moved"),
        Err(e) => return not_written(st, "Could not reorder the playlist", &e),
    }
    reload_playlist(st, id);
}

fn fill_playlist(st: &Rc<State>, id: i64) -> bool {
    let Some(app) = st.app.upgrade() else { return false };
    let Ok(Some(name)) = query::playlist_name(&st.db, id) else { return false };
    let tracks = query::playlist_tracks(&st.db, id).unwrap_or_default();
    let total: f64 = tracks.iter().map(|t| t.duration).sum();
    let n = tracks.len();
    let favs = favourite_set(st, tracks.iter().map(|t| t.id));
    st.lists.playlist.set(tracks, favs, false);
    st.lists.playlist.set_playing(playing_track_id(st));
    let g = app.global::<Playlists>();
    g.set_current_id(id as i32);
    g.set_current_name(name.into());
    g.set_used_by(used_by(st, id).into());
    g.set_current_info(
        format!(
            "{n} {} · {}",
            if n == 1 { "track" } else { "tracks" },
            duration_text(total)
        )
        .into(),
    );
    g.set_tracks(ModelRc::from(st.lists.playlist.clone()));
    st.open_playlist.set(Some(id));
    true
}

// ---- shelf editor -----------------------------------------------------------------

/// A shelf's title as shown: its own, else what it holds; a playlist's shelf (ordinary or
/// smart) is named for the playlist, as it is now (renamed since, it follows).
fn shelf_title(st: &State, s: &crate::settings::ShelfConfig) -> String {
    if s.title.is_none()
        && let ShelfQuery::Playlist { id } = s.query
        && let Some(p) = query::playlists(&st.db).unwrap_or_default().into_iter().find(|p| p.id == id)
    {
        return p.name;
    }
    s.title()
}

fn describe(q: &ShelfQuery, st: &State) -> String {
    match q {
        ShelfQuery::Genre { name } => format!("Genre: {name}"),
        ShelfQuery::Label { name } => format!("Label: {name}"),
        ShelfQuery::Playlist { id } => {
            let p = query::playlists(&st.db).unwrap_or_default().into_iter().find(|p| p.id == *id);
            let smart = p.as_ref().is_some_and(query::Playlist::smart);
            let name = p.map_or_else(|| "missing".to_owned(), |p| p.name);
            format!("{}: {name}", if smart { "Smart playlist" } else { "Playlist" })
        }
        other => other.default_title(),
    }
}

pub fn refresh_shelf_editor(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let rows: Vec<ShelfEdit> = st
        .settings
        .borrow()
        .shelves
        .iter()
        .map(|s| ShelfEdit {
            title: shelf_title(st, s).into(),
            description: describe(&s.query, st).into(),
            layout: slint_layout(s.layout),
            limit: s.limit as i32,
            rows: s.rows.clamp(1, crate::settings::GRID_ROWS_MAX) as i32,
            visible: s.visible,
        })
        .collect();
    let ed = app.global::<ShelfEditor>();
    ed.set_rows(ModelRc::new(VecModel::from(rows)));
    let named = |v: Vec<(String, i64)>| {
        ModelRc::new(VecModel::from(
            v.into_iter().take(40).map(|(name, count)| crate::NamedCount { name: name.into(), count: count as i32 }).collect::<Vec<_>>(),
        ))
    };
    ed.set_genres(named(query::genres(&st.db).unwrap_or_default()));
    ed.set_labels(named(query::labels(&st.db).unwrap_or_default()));
    refresh_playlists(st);
}

pub fn shelves_changed(st: &Rc<State>) {
    st.save_settings_soon();
    refresh_shelf_editor(st);
    refresh_home(st);
}

/// The shelves were moved or one removed: what is kept by a shelf's place in the settings (a
/// random shelf's shuffle, the album a hero features) goes with it to its new place (`to`:
/// none when it is gone).
pub fn shelves_moved(st: &Rc<State>, to: impl Fn(usize) -> Option<usize>) {
    rekey(&mut st.shelf_seeds.borrow_mut(), &to);
    HERO_AT.with(|h| rekey(&mut h.borrow_mut(), &to));
    HERO_ART.with(|a| rekey(&mut a.borrow_mut(), &to));
}

fn rekey<V>(m: &mut HashMap<usize, V>, to: impl Fn(usize) -> Option<usize>) {
    *m = std::mem::take(m).into_iter().filter_map(|(k, v)| Some((to(k)?, v))).collect();
}

pub fn reset_shelves(st: &Rc<State>) {
    st.shelf_seeds.borrow_mut().clear();
    HERO_AT.with(|h| h.borrow_mut().clear());
    st.settings.borrow_mut().shelves = settings::default_shelves();
    shelves_changed(st);
}

// ---- library status ----------------------------------------------------------------

pub fn refresh_folders(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let folders: Vec<slint::SharedString> =
        st.settings.borrow().folders.iter().map(|p| p.display().to_string().into()).collect();
    app.global::<Library>().set_folders(ModelRc::new(VecModel::from(folders)));
}

pub fn refresh_player_settings(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let s = st.settings.borrow();
    let p = app.global::<Player>();
    p.set_volume(s.volume);
    p.set_preamp(format!("{:+} dB", s.preamp_db.round() as i32).replace("+0", "0").into());
    p.set_muted(s.muted);
    p.set_limit_peaks(s.limit_peaks);
    p.set_prevent_clipping(s.prevent_clipping);
    p.set_close_quits(s.close_action == settings::CloseAction::Quit);
    p.set_tray_click_shows(s.tray_click == settings::TrayClick::ShowWindow);
    p.set_no_tray(!crate::platform::system::tray_host());
    p.set_bar_autohide(s.bar_autohide);
    p.set_tray_animation(s.tray_animation);
    p.set_tray_animation_offered(crate::platform::system::TRAY_ANIMATES);
    let edition = crate::edition::get();
    let autostart = crate::platform::integration::autostart(edition);
    p.set_autostart_offered(autostart.is_some());
    p.set_autostart(autostart.unwrap_or(false));
    p.set_portable(edition.portable().is_some());
    p.set_default_apps_offered(crate::platform::integration::default_apps_available(edition));
    app.global::<crate::Autoscroll>().set_enabled(s.autoscroll);
    p.set_visualizer(s.visualizer);
    p.set_lyrics_online(s.lyrics_online);
    p.set_gain_mode(
        match s.gain {
            settings::GainSetting::Off => "Off",
            settings::GainSetting::Track => "Track",
            settings::GainSetting::Album => "Album",
        }
        .into(),
    );
    p.set_crossfade(s.crossfade.enabled);
    p.set_crossfade_secs(crate::crossfade::secs_text(s.crossfade.secs).into());
    p.set_crossfade_albums(s.crossfade.album_gapless);
    p.set_crossfade_skips(s.crossfade.on_skip);
    app.global::<Nav>().set_sidebar_pinned(s.sidebar_pinned);
    app.global::<Nav>().set_queue_pinned(s.queue_pinned);
    drop(s);
    crate::metadata::details::sync(st);
}

/// Everything that depends on library contents. A browse page on screen is filled again
/// (Artists, Genres, Labels), and an open playlist or album page; the others fill when visited.
pub fn refresh_all(st: &Rc<State>) {
    forget_links();
    // What smart playlists hold may have changed with the library.
    crate::playlists::smart::changed(st);
    refresh_home(st);
    if st.grid_loaded.get() {
        refresh_grid(st);
    }
    refresh_playlists(st);
    if let Some(app) = st.app.upgrade() {
        match app.global::<Nav>().get_page() {
            Page::Artists if st.history.borrow().current == Route::Page(Page::Artists) => fill_artists(st),
            Page::Genres => fill_groups(st, false),
            Page::Labels => fill_groups(st, true),
            page @ (Page::Playlist | Page::Album) => refresh_open_list(st, page),
            _ => {}
        }
    }
}

/// The open playlist or album page read again after the library changed, where it is scrolled
/// (the same rows keep their selection); when it is gone, the place before it.
fn refresh_open_list(st: &Rc<State>, page: Page) {
    let current = st.history.borrow().current.clone();
    let there = match current {
        Route::Playlist(id) if page == Page::Playlist => fill_playlist(st, id),
        Route::Album(id) if page == Page::Album => fill_album(st, id),
        _ => return,
    };
    if !there && !(st.history.borrow().can_back() && back(st)) {
        go(st, if page == Page::Album { Page::Albums } else { Page::Playlists });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "See all" opens its page when the search thread has it, unless the user has gone on.
    #[test]
    fn a_section_in_full_opens_only_where_it_was_asked_for() {
        let search = Route::Page(Page::Search);
        let to = Route::SearchAll { kind: SearchKind::Tracks, query: "year:1994".into() };
        assert!(search_all_opens(Some(&search), &search, &to), "still on the search page");
        assert!(!search_all_opens(Some(&search), &Route::Page(Page::Home), &to), "gone elsewhere meanwhile");
        assert!(search_all_opens(None, &to, &to), "back to it, filled in place");
        assert!(!search_all_opens(None, &search, &to), "left it again before it came");
    }

    /// A shelf moved down past another, then the first removed: each keeps its own.
    #[test]
    fn what_a_shelf_keeps_follows_it_when_shelves_move() {
        let mut m: HashMap<usize, &str> = [(0, "a"), (1, "b"), (2, "c")].into();
        rekey(&mut m, |k| Some(match k {
            0 => 1,
            1 => 0,
            k => k,
        }));
        assert_eq!(m, [(1, "a"), (0, "b"), (2, "c")].into());
        rekey(&mut m, |k| k.checked_sub(1));
        assert_eq!(m, [(0, "a"), (1, "c")].into());
    }

    #[test]
    fn the_quick_filter_matches_every_word_folded() {
        let album = |title: &str, artist: &str| Album {
            id: 0,
            title: title.into(),
            artist: artist.into(),
            date: None,
            genre: None,
            label: None,
            release_type: None,
            art: None,
            ..Default::default()
        };
        let keys: Vec<String> = [album("Lemonade", "Beyoncé"), album("Ágætis byrjun", "Sigur Rós"), album("いただきバベル", "電音部"), album("Lemon", "米津玄師")]
            .iter()
            .map(grid_key)
            .collect();
        assert_eq!(filter_albums(&keys, "lemon"), vec![0, 3]);
        assert_eq!(filter_albums(&keys, "beyonce lemon"), vec![0], "every word");
        assert_eq!(filter_albums(&keys, "LEMON beyoncé"), vec![0]);
        assert_eq!(filter_albums(&keys, "sigur ros"), vec![1]);
        assert_eq!(filter_albums(&keys, "バベル"), vec![2]);
        assert_eq!(filter_albums(&keys, "ばべる"), vec![2], "katakana typed as hiragana");
        assert_eq!(filter_albums(&keys, "  "), vec![0, 1, 2, 3], "blank: everything");
        assert!(filter_albums(&keys, "zzz").is_empty());
    }

    #[test]
    fn reveal_scrolls_just_enough() {
        // View 72..1000 (window coordinates); the page at scroll -500.
        assert_eq!(reveal_scroll(-500.0, 300.0, 52.0, 72.0, 1000.0), None, "already shown");
        assert_eq!(reveal_scroll(-500.0, 20.0, 52.0, 72.0, 1000.0), Some(-448.0), "above: down to the top");
        assert_eq!(reveal_scroll(-500.0, 990.0, 52.0, 72.0, 1000.0), Some(-542.0), "below: up to the bottom");
        assert_eq!(reveal_scroll(-10.0, -200.0, 52.0, 72.0, 1000.0), Some(0.0), "never past the top of the page");
        assert_eq!(reveal_scroll(-500.0, 900.0, 2000.0, 72.0, 1000.0), Some(-1328.0), "too tall: its top");
    }

    #[test]
    fn browse_tiles_say_how_many_albums() {
        let g = |n| Group { name: "X".into(), key: "n:x".into(), albums: n, art: None };
        let a = group_albums(vec![g(0), g(1), g(12)]);
        assert_eq!(a.iter().map(|a| a.artist.as_str()).collect::<Vec<_>>(), vec!["On tracks", "1 album", "12 albums"]);
        assert_eq!(a.iter().map(|a| a.id).collect::<Vec<_>>(), vec![0, 1, 2]);
    }
}
