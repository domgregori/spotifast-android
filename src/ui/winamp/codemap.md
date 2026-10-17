# src/ui/winamp/

## Responsibility

Draws Spotifast's Winamp mini-player: an egui reimplementation of the classic
Winamp 2.x main window, equalizer, and playlist editor, painted from the
sprites of a loaded classic `.wsz` skin. It renders skin bitmaps at an integer
nearest-neighbor scale and turns pointer input into the same `Action`s as the
main window. The mini-player is a second way of looking at the one app, not a
second app: quitting stays in the menu, while the logo and Eject return to the
big window. **Desktop-only**; the Winamp mini-player and skins are explicitly
out of scope for the planned Android port (`HANDOFF.md`, and `src/skin/`).

Files covered here: `mod.rs` (main window, layout helpers, shared `View` and
`menu`), `equalizer.rs`, `playlist.rs`, `pixel_text.rs`.

## Design

**Skin-space drawing model.** Everything is positioned in *skin pixels* and
scaled by `unit` (logical points per skin pixel), a whole number of screen
pixels (`unit()` uses `WinampState::scale`). `View<K>` (`mod.rs`) is the
immediate-mode painter shared with the sub-windows: `origin` (window top-left),
`unit`, the `Skin`, an uploaded `HashMap<Sheet, TextureId>`, and an optional
`Mask` for non-rectangular skins. `View::rect(Area)` converts a
`skin::layout::Area` into an egui `Rect`.

**Sprite blitting.** `Sprite` (`src/skin/sprites.rs`) is a `(Sheet, x, y, w, h)`
rect inside a sheet bitmap. `View::paint` calls `Skin::sprite(sprite)` to get
the bitmap plus the part it actually holds (`Sprite::clipped_to`, so short
sheets degrade instead of panicking), computes a UV rect, and draws with
`painter.image(texture, dest, uv, Color32::WHITE)`. When `view.mask` is set
(a `skin::config::Mask` from `region.txt`), painting and `fill` walk
`Mask::spans(row)` so nothing is drawn outside the window shape. Textures are
uploaded once per sheet in `WinampState::textures` (`src/winamp.rs`) with
`TextureOptions::NEAREST`, so scaling stays hard-edged.

**Widget idioms.** Rather than egui widgets, controls are hand-rolled over
`Ui::interact` with stable `Id::new(("winamp", id))`: `button` (normal/pressed
sprite), `lamp_button` (sprite shown only while on or held), `slider` (returns
`((Response, SliderEvent)`, a fraction of the thumb's travel, drag memory in
`ui.data`), plus `wheel_notches` from `src/ui/widgets.rs` for volume wheels.
Actions are queued on `app.actions` and applied later by `App::apply_actions`
(no state mutation inside the borrowed view).

**Window stack.** `stack_height()` sums the open windows; `window_size()` and
`initial_size()` give the logical size, and `fit_window` resizes the viewport
(`MinInnerSize`/`MaxInnerSize`/`InnerSize`) once the display scale is known.
`show()` draws the main window (`full_window` or `shade_bar`), then offsets a
new `View` downward by each open sub-window's height, so the EQ and playlist
hang beneath in Winamp's order, sharing one borderless window and one mask.

**Main window parts** (`mod.rs`): `title_bar`, `clutter_bar` (the O A I D V
lamps and their menus), `status` (play/pause/stop, work indicator, mono/stereo),
`time_display` (four digit cells from `Numbers`/`NumsEx`, minus/blank handling,
blink while paused), `visualiser` (bars or scope in `skin.vis_colors`, fed by
`WinampState::tap`/`analyser`), `marquee` (character scroll via
`WinampState::marquee`, falling back to `pixel_line`/`marquee_pixels` when the
bitmap font cannot say the text), `rates`, `sliders` (volume/balance/seek with
`sprites::volume_frame`/`balance_frame` and Winamp's centre snap in
`balance_of`), `windows_buttons`, `transport`, `shuffle_repeat`. The shade bar
(`shade_bar`) is the rolled-up 14px strip with a mini transport and seek bar.

**Shared menu.** `menu()` builds a skin-styled `egui::Popup` (playlist colours
as text/selection, square corners, no shadow) sized to the scale by `menu_font`
and capped by `menu_limit`, using `crate::autoscroll::show` so long lists
scroll inside the window.

**Equalizer** (`equalizer.rs`): `show` (full) draws the `EqMain` background and
title bar, the ON/AUTO/PRESETS buttons, `presets_menu` from `eq::PRESETS`, and
`graph`. `graph` draws `EQ_PREAMP_LINE` then a Catmull-Rom curve through
`bands_db` (`spline`), colouring each row from the `EQ_GRAPH_LINE` pixels of
the sheet. `slider` draws vertical sliders from `sprites::eq_slider_frame`
frames; `fraction`/`decibels` map gains across `eq::RANGE_DB`. Shade mode
(`shade`) uses the compact `EqEx` volume/balance sliders with three-look thumbs
(`look`). Audio processing itself lives in `crate::eq`, not here.

**Playlist** (`playlist.rs`): `frame` tiles the `PlEdit` border to any height;
`rows` builds the list (currently playing, then the queue, Winamp-numbered via
`label`, with de-duplication while the Web API queue lags a double-click);
`list` draws `Row`s with `PixelText` and handles wheel/click/Ctrl-click
selection (by list row); `scrollbar`, `grip` (corner resize snapping to
`PLAYLIST_RESIZE_STEP`), `times`, `mini_transport`, and `menus` (ADD/REM/SEL/
MISC/LIST adapted to Spotify queue actions). `shade` is the rolled-up bar.

**Pixel text** (`pixel_text.rs`): `PixelText` rasterises each distinct line once
at skin resolution (`SIZE_PX = 11`) with skrifa monochrome hinting and a
`tiny_skia` fill (`anti_alias: false`), caches it as a `NEAREST` `TextureHandle`
(`CACHE_LIMIT = 512`), and records the ink bounds (`Line { width, height,
ink_top, ink_height }`) so callers scale glyphs, not padding. `FACES` is a
font-fallback chain (Arial/system via `system_fonts::pledit_face`, Inter, bundled
NotoEmoji, system scripts) mirroring Windows font linking; `glyph_for` falls
back to the first face's `?`. `width` measures advances so `fit` can ellipsise.

## Flow

1. A skin is read off-thread by `WinampState` (`src/winamp.rs::load`/`install`)
   into a `Skin` with decoded sheets plus `playlist`/`vis_colors`/`regions`
   (`src/skin/mod.rs`). `Skin::builtin()` stands in for gaps.
2. `App` calls `ui::winamp::show(app, ui)` (`src/app.rs:11326`, when
   `settings.winamp_window`); the window size comes from `initial_size` in
   `src/entrypoint.rs`.
3. `show` computes `unit`, fits the window, takes `WinampState::textures`, and
   builds the root `View` over `ui.max_rect().min` with the active `Mask`.
4. Layout constants (`src/skin/layout.rs` `Area`s, `sprites.rs` `Sprite`s) are
   resolved to rects; the per-region draw functions above blit sprites, fill
   pixels, run interactions, and push `Action`s; `equalizer::show` and
   `playlist::show` run on downward-offset views.
5. `show` ends by driving repaints (`theme::visualiser_frame` while the
   visualiser moves, else `request_repaint_after`) and returns.

## Integration

- **Consumes `src/skin/`**: `skin::layout` (`Area`, dimensions), `sprites`
  (`Sheet`, `Sprite`, `sprites::*`), `font` (`glyph`, `covered`), and
  `skin::{Skin, Mask, PlaylistStyle, VisColors, Regions}` from `src/skin/mod.rs`
  and `src/skin/config.rs`.
- **Consumes `src/winamp.rs`**: `WinampState` (skin, textures, marquee state,
  `playlist_scroll`/`playlist_selection`/`playlist_text`, `tap`, `analyser`,
  `balance_preview`, `time_remaining`, `MAX_SCALE`), plus `winamp::marquee_strip`.
- **Consumes `src/ui/widgets.rs`** (`SliderEvent`, `wheel_notches`) and
  `src/ui/keys.rs` (`handle`, `platform_shortcut`, `MILKDROP_SHORTCUT`).
- **Emits `crate::model::Action`** (transport, seek/volume/balance/EQ, window
  toggles, skin installs), applied in `src/app.rs`.
- **egui/eframe**: `Ui`, `Painter` (with clip rects), `Response`/`Sense`,
  `Popup`, `ViewportCommand`, textures; `src/theme`, `src/vis`, `src/autoscroll`,
  `src/util`, `src/i18n` for the visualiser, scrolling, formatting, and labels.
- **Desktop-only**: the fixed-pixel sprite shell, borderless viewport resizing,
  drag windows, and `.wsz` skins have no Android equivalent; `HANDOFF.md` lists
  the Winamp mini-player (`src/skin/`, `src/ui/winamp/`) as out of scope for
  the Android port.
