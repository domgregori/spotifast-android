//! Portable user-preference types, shared by the desktop binary and the
//! Android library.
//!
//! This holds the enums and small structs that other portable modules name,
//! with no `egui`/`fastframe`/OS coupling. The desktop-only parts of the
//! settings (theme cache, MilkDrop, the `Settings` struct that persists them,
//! and the localized `label`/`resolve` helpers) stay in the desktop crate's
//! `settings` module, which re-exports these types.

use serde::{Deserialize, Serialize};

/// Local Library identity only. Never sent to Spotify as a context URI.
pub const LIKED_SONGS_KEY: &str = "spotifast:liked-songs";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryShelf {
    #[default]
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySort {
    Library,
    RecentlyPlayed,
    Name,
    RecentlyAdded,
    Local,
    Spotify,
}

impl LibrarySort {
    pub fn supports(self, shelf: LibraryShelf) -> bool {
        match self {
            Self::RecentlyPlayed | Self::Name | Self::Library => true,
            Self::RecentlyAdded => matches!(shelf, LibraryShelf::Albums | LibraryShelf::Podcasts),
            Self::Local | Self::Spotify => shelf == LibraryShelf::Playlists,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Dark,
    Light,
    #[default]
    System,
}

/// Whether a Home shelf is drawn. Hidden shelves still refresh normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeShelfSettings {
    pub visible: bool,
}

impl Default for HomeShelfSettings {
    fn default() -> Self {
        Self { visible: true }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSettings {
    pub made_for_you: HomeShelfSettings,
    pub recommendations: HomeShelfSettings,
}

/// What moves behind the player bar's controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerBarVis {
    #[default]
    Off,
    Spectrum,
    Waveform,
}

impl PlayerBarVis {
    /// The mode a click on the player bar moves to: spectrum, waveform,
    /// then off, as Winamp's visualizer cycles.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Spectrum,
            Self::Spectrum => Self::Waveform,
            Self::Waveform => Self::Off,
        }
    }
}

/// Mini-player visualizer mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisMode {
    #[default]
    Bars,
    Scope,
    Off,
}

impl VisMode {
    /// Next mode in the display's click cycle.
    pub fn next(self) -> Self {
        match self {
            Self::Bars => Self::Scope,
            Self::Scope => Self::Off,
            Self::Off => Self::Bars,
        }
    }
}

/// The interface language: the operating system's, or one chosen in Settings.
///
/// Stored as `"system"` or a locale tag such as `"es"` or `"pt-BR"`. A file
/// without the field follows the system; a tag this version does not carry
/// also follows the system, rather than making the whole file unreadable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LanguageChoice {
    #[default]
    System,
    Locale(crate::locale::Locale),
}

impl Serialize for LanguageChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::System => "system",
            Self::Locale(locale) => locale.tag(),
        })
    }
}

impl<'de> Deserialize<'de> for LanguageChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(value
            .as_str()
            .and_then(crate::locale::Locale::from_tag)
            .map_or(Self::System, Self::Locale))
    }
}

use std::path::Path;

use crate::proxy::{
    ManualKind, ManualProxy, ProxyConfig, ProxyMode, infer_legacy_proxy_mode, proxy_mode_is_system,
    split_legacy_address,
};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The Spotify Connect name other devices see.
    pub device_name: String,
    /// 96, 160, or 320 kbps.
    pub bitrate: u16,
    pub normalisation: bool,
    pub autoplay: bool,
    pub gapless: bool,
    /// librespot backend name; `None` picks the platform default.
    pub audio_backend: Option<String>,
    pub audio_device: Option<String>,
    /// Windows output buffer in milliseconds. Smaller values may click under
    /// load; larger values delay playback controls.
    /// See [`100`].
    #[serde(default = "default_buffer_ms")]
    pub audio_buffer_ms: u32,
    pub audio_cache: bool,
    pub audio_cache_mb: u64,
    pub theme: ThemeChoice,
    /// The interface language; older files without it follow the system.
    pub language: LanguageChoice,
    /// Filename selected from the local themes directory.
    pub custom_theme: Option<String>,
    /// Last accepted appearance, retained if its source file becomes unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_theme_cache: Option<serde_json::Value>,
    /// Last detected system palette, so following Omarchy survives a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_theme_cache: Option<serde_json::Value>,
    pub home: HomeSettings,
    /// Tint the interface with the colour of the playing album's art.
    pub accent_from_art: bool,
    /// A spectrum or waveform of the playing song behind the player bar.
    pub player_bar_vis: PlayerBarVis,
    /// Allow clicks on the player bar background to cycle visualizer modes.
    pub player_bar_vis_click: bool,
    /// How many frames a second the player bar's visualizer and the mini
    /// player's analyser draw; 0 is uncapped. MilkDrop has its own.
    pub visualizer_fps: u32,
    /// Last local volume, 0..=65535.
    pub volume: u16,
    /// Whether the library sidebar is visible.
    pub sidebar_visible: bool,
    /// The playing album's art docked large at the sidebar's bottom.
    pub art_expanded: bool,
    /// Use compact single-line rows without cover art in the sidebar.
    pub sidebar_compact: bool,
    /// Show the Library as responsive cover cards instead of rows.
    pub sidebar_grid: bool,
    pub sidebar_width: f32,
    pub lyrics_width: f32,
    pub queue_width: f32,
    /// Use compact single-line rows without cover art in track lists.
    pub tracklist_compact: bool,
    /// Linux: middle-click a list to autoscroll it. Off by default, because
    /// Linux desktops usually paste the primary selection on middle click.
    /// Windows always autoscrolls and macOS never does.
    pub middle_click_autoscroll: bool,
    pub search_history: Vec<String>,
    pub show_shortcut_hints: bool,
    /// An optional personal Spotify Web API application id. The shared
    /// application remains active for coverage when this is present.
    pub web_client_id: Option<String>,
    /// Legacy reminder time, retained for older Spotifast versions.
    pub personal_app_nudge_at: Option<String>,
    /// The listener has dismissed or followed the personal-app introduction.
    pub personal_app_intro_seen: bool,
    /// Local playback has been authorized at least once on this machine, so
    /// the app can resume it silently instead of prompting.
    pub playback_authorized: bool,
    /// Closing the window hides to the tray and keeps the music playing.
    pub keep_playing_in_background: bool,
    /// Show the interactive Now Playing widget when hovering over the MacBook notch.
    pub mac_notch_widget: bool,
    /// Ask GitHub once a day whether a newer release exists.
    pub check_for_updates: bool,
    pub download_updates_automatically: bool,
    /// Context URIs and the local Liked Songs key, in pin order.
    pub pinned_contexts: Vec<String>,
    /// Older settings keep Liked Songs first until it is moved or unpinned.
    pub liked_songs_pinned: bool,
    /// The sidebar's own playlist order, set by dragging rows. Kept while
    /// another sort is selected; empty means no saved local arrangement.
    pub sidebar_order: Vec<String>,
    /// Explicit order per Library shelf. Missing shelves keep their previous
    /// behaviour; selecting another order never deletes the local arrangement.
    pub library_sort: std::collections::BTreeMap<LibraryShelf, LibrarySort>,
    /// Show only playlists owned by the signed-in account in the playlist shelf.
    pub library_by_you: bool,
    /// Interface zoom, egui's zoom factor; Ctrl+plus/minus changes it.
    pub zoom: f32,
    /// The Winamp window is open.
    pub winamp_window: bool,
    /// Windows and X11: keep a taskbar button while the Winamp window is visible.
    pub winamp_show_taskbar: bool,
    /// Windows and Linux: draw Spotifast's own title bar and window buttons
    /// instead of the desktop frame.
    pub custom_titlebar: bool,
    /// Skin file or folder name. `None` selects the built-in skin.
    pub skin: Option<String>,
    /// Pick a different skin each time the mini player opens; `skin` holds
    /// the one picked.
    pub random_skin: bool,
    /// Screen pixels per skin pixel; `None` picks double size for the
    /// display.
    pub skin_scale: Option<u8>,
    /// The Winamp window stays above other windows.
    pub winamp_on_top: bool,
    /// The mini player's visualiser: bars, scope, or off.
    pub vis: VisMode,
    /// The playlist window is open under the mini player.
    pub playlist_open: bool,
    /// How tall the playlist window is, in skin pixels.
    pub playlist_height: u32,
    /// The equalizer window is open under the mini player.
    pub eq_open: bool,
    /// The equalizer shapes local playback.
    pub eq_on: bool,
    /// The preamp, in decibels, never above zero.
    pub eq_preamp_db: f32,
    /// The ten bands, in decibels, 60 Hz to 16 kHz.
    pub eq_bands_db: [f32; 10],
    /// The balance, -1 all left to 1 all right.
    pub balance: f32,
    /// Play both channels the same.
    pub mono: bool,
    /// The playlist window is rolled up to its title bar.
    pub playlist_shaded: bool,
    /// The equalizer window is rolled up to its title bar.
    pub eq_shaded: bool,
    /// The main window is rolled up to its title bar.
    pub winamp_shaded: bool,
    /// The MilkDrop window is open (its own window, not part of the skin).
    pub milkdrop_open: bool,
    /// How long each preset plays before the next, in seconds.
    pub milkdrop_seconds: u32,
    /// How many frames a second the MilkDrop window draws; 0 is uncapped.
    pub milkdrop_fps: u32,
    /// Last reported MilkDrop screen refresh rate. The first value sets the
    /// default frame rate; this field is not directly configurable.
    pub milkdrop_screen_hz: u32,
    /// The picture's inner resolution: 1 full, 2 half, 4 quarter.
    pub milkdrop_scale: u32,
    /// The MilkDrop window fills the screen.
    pub milkdrop_fullscreen: bool,
    /// The MilkDrop window's size in logical points, when not full-screen.
    pub milkdrop_size: [f32; 2],
    /// Which proxy to use. Older files without this field stay on `system`.
    #[serde(default, skip_serializing_if = "proxy_mode_is_system")]
    pub proxy_mode: ProxyMode,
    /// Combined address from older settings files. Split into host and port
    /// on load; not written again.
    #[serde(default, skip_serializing)]
    pub proxy: String,
    /// Host of a manual HTTP or SOCKS5 proxy. Ignored when the mode is Off
    /// or System.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_host: String,
    /// Port of a manual HTTP or SOCKS5 proxy.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_port: String,
    /// Optional proxy login for Web requests.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_username: String,
    /// Optional proxy password, in memory and the platform credential store.
    /// Legacy JSON copies remain until protected migration succeeds.
    #[serde(default, skip_serializing)]
    pub proxy_password: String,
    /// Legacy settings and their password must survive until protected migration
    /// succeeds. This flag is only in memory and is never persisted.
    #[serde(skip)]
    pub proxy_password_legacy: bool,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Serialization already omits the password and old combined URL.
        // Keep the proxy username out of diagnostics as well.
        let mut preferences = serde_json::to_value(self).map_err(|_| std::fmt::Error)?;
        if let Some(fields) = preferences.as_object_mut() {
            fields.remove("proxy_username");
        }
        f.debug_struct("Settings")
            .field("preferences", &preferences)
            .finish()
    }
}

impl Default for Settings {
    /// Supply the initial preferences and defaults for fields absent from older
    /// settings files, including an inactive playlist ownership filter.
    fn default() -> Self {
        Self {
            device_name: "Spotifast".to_string(),
            bitrate: 320,
            normalisation: false,
            autoplay: true,
            gapless: true,
            audio_backend: None,
            audio_device: None,
            audio_buffer_ms: default_buffer_ms(),
            audio_cache: true,
            audio_cache_mb: 1024,
            theme: ThemeChoice::System,
            language: LanguageChoice::System,
            custom_theme: None,
            custom_theme_cache: None,
            system_theme_cache: None,
            home: HomeSettings::default(),
            accent_from_art: true,
            player_bar_vis: PlayerBarVis::Off,
            player_bar_vis_click: true,
            visualizer_fps: 60,
            volume: (u16::MAX as u32 * 70 / 100) as u16,
            sidebar_visible: true,
            art_expanded: false,
            sidebar_compact: false,
            sidebar_grid: false,
            sidebar_width: 250.0,
            lyrics_width: 360.0,
            queue_width: 360.0,
            tracklist_compact: false,
            middle_click_autoscroll: false,
            search_history: Vec::new(),
            show_shortcut_hints: true,
            web_client_id: None,
            personal_app_nudge_at: None,
            personal_app_intro_seen: false,
            playback_authorized: false,
            keep_playing_in_background: true,
            mac_notch_widget: false,
            check_for_updates: true,
            download_updates_automatically: false,
            pinned_contexts: Vec::new(),
            liked_songs_pinned: true,
            sidebar_order: Vec::new(),
            library_sort: std::collections::BTreeMap::new(),
            library_by_you: false,
            zoom: 1.0,
            winamp_window: false,
            winamp_show_taskbar: true,
            custom_titlebar: false,
            skin: None,
            random_skin: false,
            skin_scale: None,
            winamp_on_top: false,
            vis: VisMode::default(),
            playlist_open: false,
            playlist_height: 174,
            eq_open: false,
            eq_on: false,
            eq_preamp_db: 0.0,
            eq_bands_db: [0.0; 10],
            balance: 0.0,
            mono: false,
            playlist_shaded: false,
            eq_shaded: false,
            winamp_shaded: false,
            milkdrop_open: false,
            milkdrop_seconds: 10,
            milkdrop_fps: 60,
            milkdrop_screen_hz: 0,
            milkdrop_scale: 1,
            milkdrop_fullscreen: false,
            milkdrop_size: [640.0, 480.0],
            proxy_mode: ProxyMode::System,
            proxy: String::new(),
            proxy_host: String::new(),
            proxy_port: String::new(),
            proxy_username: String::new(),
            proxy_password: String::new(),
            proxy_password_legacy: false,
        }
    }
}

fn default_buffer_ms() -> u32 {
    100
}

impl Settings {

    pub fn proxy_password_record(
        &self,
    ) -> Result<Option<crate::proxy::ProxyPassword>, String> {
        if self.proxy_password.is_empty() {
            return Ok(None);
        }
        let manual = ManualProxy::parse(
            ManualKind::Http,
            &self.proxy_host,
            &self.proxy_port,
            &self.proxy_username,
            &self.proxy_password,
        )?;
        Ok(manual.password_record())
    }

    pub fn restore_proxy_password(
        &mut self,
        saved: &crate::proxy::ProxyPassword,
    ) -> bool {
        let Ok(mut manual) = ManualProxy::parse(
            ManualKind::Http,
            &self.proxy_host,
            &self.proxy_port,
            &self.proxy_username,
            "",
        ) else {
            return false;
        };
        if !manual.restore_password(saved) {
            return false;
        }
        self.proxy_password = manual.password().to_string();
        true
    }

    pub fn library_pins(&self) -> Vec<String> {
        let mut pins = self.pinned_contexts.clone();
        if !self.liked_songs_pinned {
            pins.retain(|key| key != LIKED_SONGS_KEY);
        } else if !pins.iter().any(|key| key == LIKED_SONGS_KEY) {
            pins.insert(0, LIKED_SONGS_KEY.into());
        }
        pins
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                // A file edited in Notepad or Windows PowerShell can start with
                // a byte order mark, which JSON does not allow.
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                let mut settings = serde_json::from_str(text).unwrap_or_else(|error| {
                    log::warn!("settings at {} are unreadable: {error}", path.display());
                    Self::default()
                });
                settings.migrate_proxy(text);
                settings.forget_unusable_screen_rate();
                settings.proxy_password_legacy = !settings.proxy_password.is_empty();
                settings
            }
            Err(_) => Self::default(),
        }
    }

    /// The settings as their file holds them.
    pub fn encode(&self) -> Option<String> {
        serde_json::to_string_pretty(self)
            .map_err(|error| log::warn!("unable to encode settings: {error}"))
            .ok()
    }

    /// Writes the settings and waits for the disk. The app saves through
    /// its [`fastframe_file::Writer`] instead.

    pub fn platform_backend(&self) -> Option<String> {
        self.audio_backend.clone().or_else(|| {
            if cfg!(target_os = "linux") {
                Some("pulseaudio".to_string())
            } else {
                None
            }
        })
    }

    pub fn remember_search(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.search_history.retain(|entry| entry != query);
        self.search_history.insert(0, query.to_string());
        self.search_history.truncate(12);
    }

    /// Forgets the frame rate an earlier version set from Windows's 1 Hz
    /// "hardware default", taken then as MilkDrop's first screen rate, and
    /// that screen rate with it, so MilkDrop's next real report is learned
    /// as the first. A frame rate chosen since, or learned from a real
    /// screen before, stays, and so does the 1 Hz beside it, which no dial
    /// shows and the next real report replaces.
    pub(crate) fn forget_unusable_screen_rate(&mut self) {
        let hz = self.milkdrop_screen_hz;
        if hz == 0 || (hz >= 10) || self.milkdrop_fps != hz {
            return;
        }
        self.milkdrop_fps = 60;
        self.milkdrop_screen_hz = 0;
    }

    pub fn migrate_proxy(&mut self, text: &str) {
        if !text.contains("\"proxy_mode\"") {
            self.proxy_mode = infer_legacy_proxy_mode(&self.proxy);
        }
        if self.proxy_host.is_empty() && !self.proxy.trim().is_empty() {
            let (host, port) = split_legacy_address(&self.proxy);
            self.proxy_host = host;
            if self.proxy_port.is_empty() {
                self.proxy_port = port;
            }
        }
    }

    /// The proxy the HTTP client should use. Off and System always succeed.
    /// HTTP and SOCKS5 need a host and port.
    pub fn proxy_config(&self) -> Result<ProxyConfig, String> {
        match self.proxy_mode {
            ProxyMode::Off => Ok(ProxyConfig::Off),
            ProxyMode::System => Ok(ProxyConfig::System),
            ProxyMode::Http => Ok(ProxyConfig::Http(ManualProxy::parse(
                ManualKind::Http,
                &self.proxy_host,
                &self.proxy_port,
                &self.proxy_username,
                &self.proxy_password,
            )?)),
            ProxyMode::Socks => Ok(ProxyConfig::Socks(ManualProxy::parse(
                ManualKind::Socks,
                &self.proxy_host,
                &self.proxy_port,
                &self.proxy_username,
                &self.proxy_password,
            )?)),
        }
    }
}
