# src/ui/

## Responsibility

The egui view layer: it lays out the window's panels and pages and draws every
visible control. It never mutates application state while drawing. Instead each
control pushes an `Action` onto `App::actions` (`src/app.rs:510`), and the
application applies those actions after the frame has been drawn. This one-way
rule keeps the immediate-mode borrow of `App` in a view free of conflicts.

## Design

**Immediate-mode drawing.** Every frame re-runs the view functions and redraws
from scratch; there is no retained widget tree. Views borrow `&mut App` only to
read its state snapshots (page data, `Loadable` payloads, `palette`, `locale`)
and to push actions.

**Action / emit architecture.** Actions are declared once in
`crate::model::Action` (`src/model.rs:1001`) and queued with
`app.actions.push(Action::…)`. The layer is a pure emitter: e.g.
`player_bar::show` emits `Action::TogglePlay`, `Action::Seek`, `Action::SetVolume`,
`Action::ToggleQueuePanel` (`src/ui/player_bar.rs:691`, `:790`, `:839`, `:916`);
`topbar::show` emits `Action::Back`, `Action::Forward`, `Action::Search`,
`Action::SignOut` (`src/ui/topbar.rs:268`, `:279`, `:383`, `:491`). A few
handlers (keyboard, some menus) collect a local `Vec<Action>` first and then
push, so they can rewrite a shortcut into a concrete page
(`src/ui/keys.rs:25`, `:140`).

**Window shell in `mod.rs`.** `ui::show` (`src/ui/mod.rs:35`) is the entry
point called from eframe. It runs shortcuts and then composes the frame:
`keys::handle`, the login screen when signed out, `player_bar::show`, the
optional `sidebar::show` / `queue::side_panel` / `lyrics::side_panel`,
`central` for the page, then the overlays `devices::popup`, `dialogs::show`,
`update::show`, `widgets::drag_ghost`, and `toasts`. `central`
(`src/ui/mod.rs:259`) is the `Page` → view dispatcher (match at
`src/ui/mod.rs:304`): `Page::Home` → `home::show`, `Page::Search` →
`search::show`, `Page::LikedSongs` → `collection::liked`,
`Page::Albums|Artists|Podcasts|Episodes` → `library::show`,
`Page::Playlist`/`Page::Album` → `collection::playlist`/`album`,
`Page::Artist` → `artist::show`, `Page::Show` → `show::show`,
`Page::Radio` → `radio::radio`, `Page::Queue` → `queue::page`,
`Page::Settings` → `settings::show`, `Page::TopSongs` → `collection::top_songs`.

**Reusable widgets in `widgets.rs`.** `src/ui/widgets.rs` holds the shared
vocabulary every screen draws with: covers (`cover`, `paint_cover`,
`playing_cover`, `CardCover`), cards and grids (`card`, `card_row_height`,
`shelf`, `grid`, `virtual_rows`), track tables (`TrackRow`, `track_row`,
`track_row_response`, `table_header`), menus (`menu_item`, `menu_submenu`,
`menu_frame`, `picked_menu`, `playlist_picker`, `item_menu`,
`context_menu_items`), inputs (`search_field`, `text_edit`, `thin_slider`,
`switch`, `chips`), settings rows (`setting_row`, `labeled_field`,
`proxy_manual_form`), and states (`loading_row`, `error_row`, `empty_state`,
`drag_ghost`). Layout constants (`CARD_WIDTH`, `CARD_ART`, `CARD_GAP`,
`PAGE_PADDING`) live here too.

**Shared components.** `sidebar.rs` (left navigation + Your Library),
`topbar.rs` (nav arrows, search, account menu), `player_bar.rs` (bottom
now-playing bar) are drawn once per frame by `ui::show`; `dialogs.rs` draws
modal overlays, `devices.rs` the Connect picker popup, `queue.rs` and
`lyrics.rs` both work as a page and as a side panel, and `update.rs` an update
notice.

**Screen files, one line each:**

- `mod.rs` — window shell, page dispatch, panel sizing, toasts, custom titlebar.
- `collection.rs` — playlist/album/Liked Songs/Top Songs pages: `Hero`, action row, track `Table`.
- `widgets.rs` — the shared widget library described above.
- `sidebar.rs` — left panel navigation and the Your Library tree (`RootlistEntry`).
- `settings.rs` — the Settings page (account, playback, themes, Winamp, EQ, proxy, updates).
- `player_bar.rs` — bottom now-playing bar: transport, seek, volume, device/queue/lyrics buttons.
- `home.rs` — the Home page shelves and `podcast_episodes`.
- `lyrics.rs` — synced lyrics in a side panel or fullscreen overlay.
- `search.rs` — the Search page with filters and results.
- `topbar.rs` — back/forward, search field, account menu above every page.
- `keys.rs` — keyboard shortcut handling and the `shortcuts` help list.
- `dialogs.rs` — modal dialogs (playlist details, confirmations, shortcuts).
- `prerelease.rs` — countdown page for unreleased albums, called from `collection::album` (`src/ui/collection.rs:1685`).
- `queue.rs` — the queue as a page or side panel, with reorder drag.
- `show.rs` — podcast pages and `episode_row` rows.
- `devices.rs` — Spotify Connect device picker popup.
- `artist.rs` — the artist page.
- `login.rs` — the sign-in screen (and its proxy form).
- `radio.rs` — a radio mix page laid out as a playlist.
- `library.rs` — full-page library grids (albums, artists, podcasts, episodes).
- `update.rs` — the update-available banner.

## Flow

1. eframe calls the app's `update` each frame; it invokes `crate::ui::show(self, ui)`
   (`src/app.rs:11328`), or `crate::ui::winamp::show` when the Winamp window is
   active (`src/app.rs:11326`).
2. `ui::show` (`src/ui/mod.rs:35`) runs `keys::handle(app, ctx)`, then draws the
   shell and, through `central`, exactly one page view function.
3. Each control calls `app.actions.push(Action::…)`. Variants span navigation
   (`Open(Page)`, `Back`, `Forward`, `OpenUri`, `OpenLink`), playback
   (`TogglePlay`, `Next`, `Previous`, `Seek`, `PlayContext`, `PlayFromRow`,
   `PlayEpisode`, `PlayUris`, `SetVolume`, `ToggleShuffle`, `CycleRepeat`),
   library and playlists (`ToggleSaved`, `AddToPlaylist`,
   `ConfirmAddToPlaylist`, `SetLibrarySort`, `ArrangeLibrary`), panels and UI
   (`ToggleSidebar`, `ToggleQueuePanel`, `ToggleLyricsPanel`, `ShowDialog(Dialog)`,
   `SettingsChanged`, `Search`, `FocusSearch`), and system (`SignIn`, `SignOut`,
   `ApplyProxy`, `InstallUpdate`, `Transfer`, `ActivateReceiver`, `Quit`).
4. `keys::handle` (`src/ui/keys.rs:20`) also emits into `app.actions`, so
   shortcuts travel the same route as clicks.
5. On the next logic pass, `App::apply_actions` (`src/app.rs:9435`, called at
   `src/app.rs:10999` and `:11330`) drains the queue with `std::mem::take`,
   looping until no new actions were produced, and dispatches each to
   `App::apply` (`src/app.rs:9463`). That is where optimistic local state is
   updated and backend work or persistence is scheduled; the view layer itself
   changes nothing.

Views request extra frames where animation needs them, e.g.
`ctx.request_repaint_after` in `lyrics.rs:813`, `widgets.rs:1658`,
`prerelease.rs:144`, and `ui.ctx().request_repaint()` in `collection.rs:1112`.

## Integration

**Consumes** (read-only during drawing):

- `crate::app::App` — its fields `palette` (`src/app.rs:333`), `locale`
  (`src/app.rs:336`), per-page `Loadable` snapshots, and `actions: Vec<Action>`
  (`src/app.rs:510`) which views append to.
- `crate::model` — `Action`, `Page`, `Loadable`, `Dialog`, `RowContext`,
  `RowPick`, `PlayableItem`, `TableItem`, `QueueTab`, `SearchFilter`, `Toast`.
- `crate::api::models` — `Track`, `Album`, `Playlist`, `Image`, `PlayableItem`,
  `pick_image`.
- `crate::images` — `ArtLoader`, `cover_pixels`, `scaled_uri`,
  `COVER_TEXTURE` (used by `widgets::cover`/`playing_cover`, `src/ui/widgets.rs:141`).
- `crate::theme` — `Palette`, `Icon`, colours and metrics; `crate::i18n` —
  `Locale`, `gettext`, `ngettext`, `pgettext`.
- `crate::backend` — `AuthStatus`, `LocalPlayback`; `crate::player` —
  `RepeatMode`, `RootlistEntry`; plus `crate::autoscroll`, `crate::window`,
  `crate::updates::DownloadState`, `crate::prerelease::Prerelease`.

**External crates:** `egui`/`eframe` (the GUI itself) and the shared
fastframe forks: `fastframe_titlebar` (`enabled`, `drag`, `resize`, `footprint`,
`controls`, `Footprint`, `Style`, `Labels` — `src/ui/mod.rs:51`, `:452`),
`fastframe_theme` (`display_name`, `src/ui/settings.rs:994`) and
`fastframe_fonts` (`INTER`, `system::fallbacks`, used by the Winamp text path).

**Desktop-only.** This layer is built on egui/eframe and winit window behaviour
(custom titlebars, resize/drag regions, per-frame viewport commands) and is not
part of the Android target; an Android client replaces it with Jetpack Compose
while reusing the non-UI layers (model, backend, player).
